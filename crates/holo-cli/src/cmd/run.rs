use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, bail};
use holo_core::CloudProvider;

use super::Ctx;
use crate::providers;

/// Holds an exclusive handle so only one daemon runs per user.
fn single_instance(ctx: &Ctx) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(&ctx.paths.data)?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(ctx.paths.lock())
        .context("another `holo run` is already running")
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let _lock = single_instance(ctx)?;
    let roots: Vec<_> = ctx
        .state
        .roots()?
        .into_iter()
        .filter(|r| r.enabled)
        .collect();
    if roots.is_empty() {
        bail!("no sync roots; add one with `holo add-root`");
    }

    let engine_cfg = ctx.cfg.engine(&ctx.paths);
    let mut providers: HashMap<String, Arc<dyn CloudProvider>> = HashMap::new();
    let mut handles = Vec::new();
    let load = crate::load::probe();

    for root in roots {
        let provider = match providers.get(&root.account_id) {
            Some(p) => p.clone(),
            None => {
                let account = ctx.account(Some(&root.account_id))?;
                let p = providers::for_account(&ctx.cfg, &ctx.paths, &account).await?;
                providers.insert(root.account_id.clone(), p.clone());
                p
            }
        };
        let tree = local_tree();
        let path = root.local_path.clone();
        match holo_engine::runtime::start_root(
            root,
            provider,
            ctx.state.clone(),
            tree,
            engine_cfg.clone(),
            holo_engine::runtime::StartOptions {
                connect_cloud_filter: cfg!(windows),
                load: load.clone(),
            },
        )
        .await
        {
            Ok(h) => {
                tracing::info!(root = %path, "syncing");
                handles.push(h);
            }
            Err(e) => {
                tracing::error!(root = %path, error = %format!("{e:#}"), "failed to start root")
            }
        }
    }
    if handles.is_empty() {
        bail!("no sync root could be started");
    }

    tracing::info!(
        roots = handles.len(),
        "holo is running; press Ctrl+C to stop"
    );
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        () = super::service::stop_requested() => tracing::info!("stop requested by `holo service stop`"),
    }
    tracing::info!("stopping…");
    for h in handles {
        h.shutdown().await;
    }
    Ok(())
}

#[cfg(windows)]
fn local_tree() -> Arc<dyn holo_engine::LocalTree> {
    Arc::new(holo_engine::local_tree::CfLocalTree)
}

#[cfg(not(windows))]
fn local_tree() -> Arc<dyn holo_engine::LocalTree> {
    unimplemented!("holo only runs on Windows")
}
