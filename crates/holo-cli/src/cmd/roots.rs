use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use chrono::Utc;
use holo_core::{CloudProvider, ProviderError, ProviderKind, RemoteItem};
use holo_state::RootRecord;

use super::Ctx;
use crate::providers;

pub const PROVIDER_NAME: &str = "HoloFiles";

pub fn sync_root_spec(
    root: &RootRecord,
    account_label: &str,
    kind: ProviderKind,
) -> holo_cfapi::SyncRootSpec {
    let remote = if root.remote_path == "/" {
        String::new()
    } else {
        format!(" {}", root.remote_path)
    };
    holo_cfapi::SyncRootSpec {
        provider_name: PROVIDER_NAME.into(),
        account_key: root.id.clone(),
        display_name: format!("Holo - {account_label}{remote}"),
        path: PathBuf::from(&root.local_path),
        icon: r"%SystemRoot%\system32\imageres.dll,-1043".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        recycle_bin_url: match kind {
            ProviderKind::OneDrive => Some("https://onedrive.live.com/?v=recyclebin".into()),
            ProviderKind::GoogleDrive => Some("https://drive.google.com/drive/trash".into()),
        },
    }
}

pub fn validate_local(ctx: &Ctx, local: &Path) -> anyhow::Result<PathBuf> {
    let local = std::path::absolute(local)?;
    let s = local.to_string_lossy().to_lowercase();
    if let Ok(tmp) = std::env::var("TEMP")
        && s.starts_with(&tmp.to_lowercase())
    {
        bail!("sync roots under %TEMP% are rejected by Windows; pick another folder");
    }
    for r in ctx.state.roots()? {
        let other = r.local_path.to_lowercase();
        if s == other
            || s.starts_with(&format!("{other}\\"))
            || other.starts_with(&format!("{s}\\"))
        {
            bail!("overlaps existing sync root {}", r.local_path);
        }
    }
    #[cfg(windows)]
    match holo_cfapi::root::owner(&local)? {
        Some(owner) if owner == PROVIDER_NAME => {
            bail!("{} is already inside a holo sync root", local.display())
        }
        Some(owner) => bail!(
            "{} is inside a sync root managed by '{owner}' (e.g. the official OneDrive client). Unlink that client from this folder first, or pick a folder outside it.",
            local.display()
        ),
        None => {}
    }
    Ok(local)
}

/// `mkdir -p` on the remote: resolve each prefix of `path`, creating the missing ones.
async fn create_remote_dirs(
    provider: &dyn CloudProvider,
    path: &str,
) -> anyhow::Result<RemoteItem> {
    let mut cur = provider.item_by_path("/").await?;
    let mut prefix = String::new();
    for seg in path.split(['/', '\\']).filter(|s| !s.is_empty()) {
        prefix.push('/');
        prefix.push_str(seg);
        cur = match provider.item_by_path(&prefix).await {
            Ok(item) => item,
            Err(ProviderError::NotFound) => match provider.create_folder(&cur.id, seg).await {
                Ok(item) => {
                    println!("Created remote folder {prefix}");
                    item
                }
                // Someone else created it in the meantime.
                Err(ProviderError::NameConflict) => provider.item_by_path(&prefix).await?,
                Err(e) => return Err(e.into()),
            },
            Err(e) => return Err(e.into()),
        };
        if !cur.is_dir() {
            bail!("'{prefix}' exists and is not a folder");
        }
    }
    Ok(cur)
}

pub async fn add(
    ctx: &Ctx,
    local: &Path,
    remote: &str,
    account: Option<&str>,
    create: bool,
) -> anyhow::Result<()> {
    let account = ctx.account(account)?;
    let local = validate_local(ctx, local)?;
    let provider = providers::for_account(&ctx.cfg, &ctx.paths, &account).await?;
    let remote_item = match provider.item_by_path(remote).await {
        Err(ProviderError::NotFound) if create => create_remote_dirs(provider.as_ref(), remote)
            .await
            .with_context(|| format!("creating remote folder '{remote}'"))?,
        Err(ProviderError::NotFound) => {
            bail!("remote folder '{remote}' not found (pass --create to create it)")
        }
        other => other.with_context(|| format!("remote folder '{remote}'"))?,
    };
    if !remote_item.is_dir() {
        bail!("'{remote}' is not a folder");
    }

    if local.exists() && std::fs::read_dir(&local)?.next().is_some() {
        eprintln!(
            "note: {} is not empty; existing files will be merged (identical files are linked, different ones kept as conflict copies).",
            local.display()
        );
    }
    std::fs::create_dir_all(&local)?;

    let root = RootRecord {
        id: uuid::Uuid::new_v4().simple().to_string(),
        account_id: account.id.clone(),
        local_path: local.to_string_lossy().into_owned(),
        remote_root_id: remote_item.id.clone(),
        remote_path: if remote.starts_with('/') {
            remote.to_owned()
        } else {
            format!("/{remote}")
        },
        enabled: true,
        cursor: None,
        created_at: Utc::now(),
    };
    #[cfg(windows)]
    {
        if !holo_cfapi::root::is_supported() {
            bail!("this Windows build does not support the Cloud Filter API");
        }
        let label = account
            .email
            .clone()
            .unwrap_or(account.display_name.clone());
        holo_cfapi::root::register(&sync_root_spec(&root, &label, account.provider))
            .context("registering the sync root with Windows (the folder must be on NTFS)")?;
    }
    ctx.state.insert_root(&root)?;
    println!(
        "Added sync root {} ↔ {}:{}  (id {})",
        root.local_path, account.id, root.remote_path, root.id
    );
    println!("Start syncing with `holo run`.");
    Ok(())
}

pub fn remove(ctx: &Ctx, key: &str, yes: bool) -> anyhow::Result<()> {
    let root = ctx.root(key)?;
    if !yes {
        bail!(
            "this unregisters {} from Windows: files available only in the cloud will disappear locally \
             (they stay in the cloud), fully downloaded files remain as normal files. Re-run with --yes.",
            root.local_path
        );
    }
    #[cfg(windows)]
    {
        let account = ctx.account(Some(&root.account_id))?;
        let spec = sync_root_spec(&root, &account.display_name, account.provider);
        // A running daemon holds the Cloud Filter connection, which blocks
        // unregistering. Disabling the root makes it let go within a few seconds.
        ctx.state.set_root_enabled(&root.id, false)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if !holo_cfapi::root::is_registered(&spec).unwrap_or(true) {
                break;
            }
            match holo_cfapi::root::unregister(&spec) {
                Ok(()) => break,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                // Keep the DB record (disabled): deleting it would orphan the Windows
                // registration (Explorer entry stays) with no way to retry from holo.
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "unregistering {} from Windows failed; the root is now disabled, retry `holo remove-root`",
                            root.local_path
                        )
                    });
                }
            }
        }
    }
    ctx.state.delete_root(&root.id)?;
    println!("Removed sync root {}", root.local_path);
    Ok(())
}

/// `remove-root --all`: try every root, report all failures at the end.
pub fn remove_all(ctx: &Ctx, yes: bool) -> anyhow::Result<()> {
    let roots = ctx.state.roots()?;
    if roots.is_empty() {
        println!("No sync roots.");
        return Ok(());
    }
    let mut failed = 0;
    for r in roots {
        if let Err(e) = remove(ctx, &r.id, yes) {
            eprintln!("! {}: {e:#}", r.local_path);
            failed += 1;
        }
    }
    anyhow::ensure!(failed == 0, "{failed} root(s) could not be removed");
    Ok(())
}

pub fn list(ctx: &Ctx) -> anyhow::Result<()> {
    let roots = ctx.state.roots()?;
    if roots.is_empty() {
        println!("No sync roots. Add one with `holo add-root <folder> --remote /path`.");
    }
    for r in roots {
        println!(
            "{}  {}  ↔  {}:{}{}",
            r.id,
            r.local_path,
            r.account_id,
            r.remote_path,
            if r.enabled { "" } else { "  (disabled)" }
        );
    }
    Ok(())
}
