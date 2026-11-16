//! Wiring: build a root's context, connect Cloud Filter callbacks, spawn its tasks.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use holo_core::{CloudProvider, ignore_rules::IgnoreRules};
use holo_state::{RootRecord, StateStore};
use tokio::{
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
};

use crate::{
    EngineConfig, LocalTree,
    context::{RootContext, Suppressor},
    local, ops, remote,
};

pub struct RootHandle {
    pub ctx: Arc<RootContext>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    #[cfg(windows)]
    _connection: Option<holo_cfapi::root::RootConnection>,
}

pub struct StartOptions {
    /// Connect Cloud Filter callbacks (false in tests).
    pub connect_cloud_filter: bool,
    /// Machine load source for adaptive concurrency.
    pub load: Option<Arc<dyn crate::adaptive::LoadProbe>>,
}

pub async fn start_root(
    root: RootRecord,
    provider: Arc<dyn CloudProvider>,
    state: Arc<StateStore>,
    tree: Arc<dyn LocalTree>,
    cfg: EngineConfig,
    opts: StartOptions,
) -> anyhow::Result<RootHandle> {
    let local_root = PathBuf::from(&root.local_path);
    let ignore = IgnoreRules::load(&local_root, cfg.use_default_ignores, &cfg.ignore)
        .map_err(|e| anyhow::anyhow!("invalid ignore rules: {e}"))?;
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let ctx = Arc::new(RootContext {
        root,
        local_root,
        provider,
        state,
        tree,
        ignore,
        cfg,
        suppress: Suppressor::default(),
        wake_remote: Notify::new(),
        wake_ops: Notify::new(),
        events: events_tx,
        pull_failures: Default::default(),
        dir_lock: Default::default(),
        load: opts.load.clone(),
    });

    #[cfg(windows)]
    let connection = if opts.connect_cloud_filter {
        let handler = Arc::new(crate::hydrate::Handler::new(
            ctx.clone(),
            tokio::runtime::Handle::current(),
        ));
        let conn = holo_cfapi::root::connect(&ctx.local_root, handler, ctx.cfg.min_fetch)
            .with_context(|| format!("connecting sync root {}", ctx.local_root.display()))?;
        conn.set_status(holo_cfapi::root::ProviderState::Populating);
        Some(conn)
    } else {
        None
    };
    #[cfg(not(windows))]
    let _ = opts;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = watch::channel(false);

    let poller = {
        let ctx = ctx.clone();
        let mut shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            loop {
                match remote::pull(&ctx).await {
                    Ok(0) => tracing::debug!(root = ctx.id(), "no remote changes"),
                    Ok(n) => tracing::info!(root = ctx.id(), changes = n, "remote changes"),
                    Err(e) => {
                        tracing::warn!(root = ctx.id(), error = %format!("{e:#}"), "remote pull failed")
                    }
                }
                let _ = ready_tx.send(true);
                tokio::select! {
                    _ = tokio::time::sleep(ctx.cfg.poll_interval) => {}
                    _ = ctx.wake_remote.notified() => {}
                    _ = shutdown.changed() => return,
                }
            }
        })
    };
    let local = tokio::spawn(local::run(
        ctx.clone(),
        events_rx,
        ready_rx,
        shutdown_rx.clone(),
    ));
    let executor = tokio::spawn(ops::run(ctx.clone(), shutdown_rx));

    Ok(RootHandle {
        ctx,
        shutdown: shutdown_tx,
        tasks: vec![poller, local, executor],
        #[cfg(windows)]
        _connection: connection,
    })
}

impl RootHandle {
    /// Request a remote poll now.
    pub fn sync_now(&self) {
        self.ctx.wake_remote.notify_one();
        self.ctx.wake_ops.notify_one();
    }

    /// Stop tasks, then disconnect callbacks (placeholders and registration remain).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for t in self.tasks {
            let _ = t.await;
        }
    }
}
