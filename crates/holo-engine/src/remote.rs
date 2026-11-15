//! Remote → local: pull the change feed and apply it to the local tree.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use anyhow::Context as _;
use holo_cfapi::PlaceholderSpec;
use holo_core::{ChangeCursor, ProviderError, RelPath, RemoteItem, RemoteItemId, naming};
use holo_state::ItemRecord;

use crate::{
    context::RootContext,
    ops::{self, LocalOp},
    plan::{self, RemoteAction},
};

/// After this many consecutive failed applies, the cursor advances anyway so one
/// poisoned item can't make every poll re-download the whole batch.
const MAX_APPLY_RETRIES: u32 = 3;

pub fn spec(item: &RemoteItem) -> PlaceholderSpec {
    PlaceholderSpec {
        is_dir: item.is_dir(),
        size: item.size,
        modified: item.modified,
        identity: item.id.as_str().as_bytes().to_vec(),
    }
}

/// Fetch all pending remote changes and apply them. Returns the number of changes seen.
pub async fn pull(ctx: &Arc<RootContext>) -> anyhow::Result<usize> {
    let root = ctx.state.root(ctx.id())?.context("root no longer exists")?;
    let mut cursor = root.cursor.map(ChangeCursor);
    let mut full = cursor.is_none();
    let mut changes = Vec::new();
    loop {
        match ctx
            .provider
            .changes(ctx.remote_root(), cursor.as_ref())
            .await
        {
            Ok(batch) => {
                changes.extend(batch.items);
                cursor = Some(batch.cursor);
                if !batch.has_more {
                    break;
                }
            }
            Err(ProviderError::ResyncRequired) => {
                tracing::warn!(root = ctx.id(), "change cursor expired; re-enumerating");
                cursor = None;
                full = true;
                changes.clear();
            }
            Err(e) => return Err(e.into()),
        }
    }

    let n = changes.len();
    if n > 0 || full {
        let c = ctx.clone();
        let result = tokio::task::spawn_blocking(move || apply(&c, &changes, full)).await?;
        if let Err(e) = result {
            let failures = ctx.pull_failures.fetch_add(1, Ordering::Relaxed) + 1;
            if failures < MAX_APPLY_RETRIES {
                return Err(e.context("will retry from the same cursor"));
            }
            tracing::error!(root = ctx.id(), error = %e, "giving up on failed remote changes; advancing cursor");
        }
    }
    ctx.pull_failures.store(0, Ordering::Relaxed);
    ctx.state
        .set_cursor(ctx.id(), cursor.as_ref().map(|c| c.0.as_str()))?;
    Ok(n)
}

/// Plan and apply a batch (blocking).
pub fn apply(ctx: &RootContext, changes: &[RemoteItem], full: bool) -> anyhow::Result<()> {
    let known: HashMap<RemoteItemId, ItemRecord> = ctx
        .state
        .all_items(ctx.id())?
        .into_iter()
        .map(|r| (r.remote_id.clone(), r))
        .collect();
    let actions = plan::plan(ctx.remote_root(), &known, changes, full, &ctx.ignore);
    let deleting: HashSet<RemoteItemId> = actions
        .iter()
        .filter_map(|a| match a {
            RemoteAction::Delete { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    let mut failed = 0usize;
    for action in &actions {
        if let Err(e) = apply_action(ctx, action, &deleting) {
            failed += 1;
            tracing::warn!(root = ctx.id(), ?action, error = %e, "failed to apply remote change");
        }
    }
    if !actions.is_empty() {
        tracing::info!(
            root = ctx.id(),
            applied = actions.len() - failed,
            failed,
            "remote changes applied"
        );
    }
    anyhow::ensure!(failed == 0, "{failed} remote change(s) failed");
    Ok(())
}

fn identity_of(st: &holo_cfapi::LocalState) -> Option<RemoteItemId> {
    st.identity()
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(RemoteItemId::new)
}

fn apply_action(
    ctx: &RootContext,
    action: &RemoteAction,
    deleting: &HashSet<RemoteItemId>,
) -> anyhow::Result<()> {
    match action {
        RemoteAction::Create { item, path } => create(ctx, item, path, deleting),
        RemoteAction::Change {
            item, to, content, ..
        } => change(ctx, item, to, *content, deleting),
        RemoteAction::Delete { id, path, is_dir } => {
            ctx.suppress.add(path);
            remove_local(ctx, &ctx.local(path), path)?;
            if *is_dir {
                ctx.state.delete_subtree(ctx.id(), path)?;
            }
            ctx.state.delete_item(ctx.id(), id)?;
            Ok(())
        }
        RemoteAction::Forget { id } => {
            if let Some(r) = ctx.state.item(ctx.id(), id)? {
                ctx.state.delete_subtree(ctx.id(), &r.rel_path)?;
            }
            Ok(())
        }
    }
}

/// Make room at `path` for the remote item: remove a stale occupant that is being
/// deleted in the same batch, otherwise preserve it as a conflict copy.
fn clear_occupant(
    ctx: &RootContext,
    path: &RelPath,
    deleting: &HashSet<RemoteItemId>,
) -> anyhow::Result<()> {
    let local = ctx.local(path);
    let Some(st) = ctx.tree.state(&local)? else {
        return Ok(());
    };
    if let Some(id) = identity_of(&st)
        && deleting.contains(&id)
    {
        remove_local(ctx, &local, path)?;
        ctx.state.delete_item(ctx.id(), &id)?;
        if ctx.tree.state(&local)?.is_none() {
            return Ok(());
        }
    }
    move_aside(ctx, path)?;
    Ok(())
}

/// Rename a local item to a conflict name, detach it from the cloud and queue it for
/// upload as a new item. Nothing is ever overwritten silently.
pub fn move_aside(ctx: &RootContext, path: &RelPath) -> anyhow::Result<RelPath> {
    let name = naming::conflict_name(path.name(), &ctx.cfg.machine_name, chrono::Local::now());
    let new = path.parent().unwrap_or_default().join(&name);
    ctx.suppress.add(path);
    ctx.suppress.add(&new);
    ctx.tree.rename(&ctx.local(path), &ctx.local(&new))?;
    let _ = ctx.tree.detach(&ctx.local(&new));
    tracing::warn!(root = ctx.id(), from = %path, to = %new, "conflict: kept local version as a copy");
    ops::enqueue(ctx, &LocalOp::Upload { path: new.clone() })?;
    Ok(new)
}

fn create(
    ctx: &RootContext,
    item: &RemoteItem,
    path: &RelPath,
    deleting: &HashSet<RemoteItemId>,
) -> anyhow::Result<()> {
    let local = ctx.local(path);
    let parent = ctx.local(&path.parent().unwrap_or_default());
    ctx.suppress.add(path);
    if ctx.tree.state(&parent)?.is_none() {
        anyhow::bail!("parent folder missing locally: {}", parent.display());
    }
    let record = ItemRecord::from_remote(ctx.id(), item, path.clone());

    if let Some(st) = ctx.tree.state(&local)? {
        if identity_of(&st).as_ref() == Some(&item.id) {
            // Already there (re-applied after a crash).
            ctx.state.upsert_item(&record)?;
            return Ok(());
        }
        if st.is_dir && item.is_dir() && identity_of(&st).is_none() {
            // Same folder on both sides (e.g. first sync over existing files): merge.
            ctx.tree
                .mark_synced(&local, item.id.as_str().as_bytes(), None)?;
            ctx.state.upsert_item(&record)?;
            return Ok(());
        }
        if !st.is_dir
            && !item.is_dir()
            && identity_of(&st).is_none()
            && same_content(ctx, &local, item)?
        {
            ctx.tree
                .mark_synced(&local, item.id.as_str().as_bytes(), None)?;
            ctx.state.upsert_item(&record)?;
            return Ok(());
        }
        clear_occupant(ctx, path, deleting)?;
    }

    ctx.tree
        .create_placeholder(&parent, path.name(), &spec(item))?;
    ctx.state.upsert_item(&record)?;
    Ok(())
}

fn same_content(ctx: &RootContext, local: &Path, item: &RemoteItem) -> anyhow::Result<bool> {
    let Some(remote) = &item.hash else {
        return Ok(false);
    };
    Ok(ctx.tree.hash(local, remote.algo)?.as_ref() == Some(remote))
}

fn change(
    ctx: &RootContext,
    item: &RemoteItem,
    to: &RelPath,
    content: bool,
    deleting: &HashSet<RemoteItemId>,
) -> anyhow::Result<()> {
    // Re-read: an ancestor move earlier in this batch may have rebased the path.
    let Some(current) = ctx.state.item(ctx.id(), &item.id)? else {
        return create(ctx, item, to, deleting);
    };
    let from = current.rel_path.clone();

    if from != *to {
        let (lf, lt) = (ctx.local(&from), ctx.local(to));
        ctx.suppress.add(&from);
        ctx.suppress.add(to);
        if ctx.tree.state(&lf)?.is_none() {
            ctx.state.delete_item(ctx.id(), &item.id)?;
            return create(ctx, item, to, deleting);
        }
        // Case-only renames are the same file on NTFS: don't treat as occupied.
        if from.key() != to.key() && ctx.tree.state(&lt)?.is_some() {
            clear_occupant(ctx, to, deleting)?;
        }
        ctx.tree.rename(&lf, &lt)?;
        if current.is_dir {
            ctx.state.rebase_subtree(ctx.id(), &from, to)?;
        }
    }

    if content && !item.is_dir() {
        let local = ctx.local(to);
        match ctx.tree.state(&local)? {
            None => {
                let parent = ctx.local(&to.parent().unwrap_or_default());
                ctx.tree
                    .create_placeholder(&parent, to.name(), &spec(item))?;
            }
            Some(st) if !st.needs_upload() => {
                if ctx.tree.update_placeholder(&local, &spec(item), true)? {
                    // Pinned ("always keep on this device"): fetch the new version now.
                    let tree = ctx.tree.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = tree.hydrate(&local) {
                            tracing::warn!(path = %local.display(), error = %e, "re-hydration failed");
                        }
                    });
                }
            }
            Some(_) => {
                // Edited on both sides.
                move_aside(ctx, to)?;
                let parent = ctx.local(&to.parent().unwrap_or_default());
                ctx.tree
                    .create_placeholder(&parent, to.name(), &spec(item))?;
            }
        }
    }

    ctx.state
        .upsert_item(&ItemRecord::from_remote(ctx.id(), item, to.clone()))?;
    Ok(())
}

/// Remove local content for a remotely deleted item, keeping anything that has local
/// changes (those are detached and re-uploaded as new items).
fn remove_local(ctx: &RootContext, local: &Path, rel: &RelPath) -> anyhow::Result<()> {
    let Some(st) = ctx.tree.state(local)? else {
        return Ok(());
    };
    ctx.suppress.add(rel);
    if st.is_dir {
        for name in ctx.tree.read_dir(local)? {
            remove_local(ctx, &local.join(&name), &rel.join(&name))?;
        }
        if let Err(e) = ctx.tree.remove_dir(local) {
            tracing::info!(path = %local.display(), error = %e, "kept folder with local-only content");
            let _ = ctx.tree.detach(local);
        }
        return Ok(());
    }
    match &st.placeholder {
        Some(p) if p.in_sync && p.partial => ctx.tree.remove_file(local)?,
        // Fully local, unchanged: keep a copy in the holo trash just in case.
        Some(p) if p.in_sync => ctx.tree.trash(local, &ctx.cfg.trash_dir)?,
        _ if ctx.is_ignored(rel, false) => {}
        _ => {
            ctx.tree.detach(local)?;
            tracing::warn!(path = %rel, "deleted remotely but changed locally; re-uploading");
            ops::enqueue(ctx, &LocalOp::Upload { path: rel.clone() })?;
        }
    }
    Ok(())
}
