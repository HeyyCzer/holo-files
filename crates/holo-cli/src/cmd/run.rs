use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use holo_core::CloudProvider;
use holo_engine::runtime::RootHandle;
use holo_state::RootRecord;

use super::Ctx;
use crate::providers;

/// How often the daemon re-reads the root list, so `add-root` / `remove-root`
/// take effect without restarting it.
const RELOAD_INTERVAL: Duration = Duration::from_secs(2);
/// Back-off before retrying a root that failed to start.
const RETRY_FAILED: Duration = Duration::from_secs(60);

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

struct Daemon<'a> {
    ctx: &'a Ctx,
    engine_cfg: holo_engine::EngineConfig,
    load: Option<Arc<dyn holo_engine::adaptive::LoadProbe>>,
    providers: HashMap<String, Arc<dyn CloudProvider>>,
    running: HashMap<String, RootHandle>,
    failed: HashMap<String, Instant>,
}

impl Daemon<'_> {
    /// Start enabled roots that aren't running; stop the ones removed or disabled.
    async fn reconcile(&mut self) -> anyhow::Result<()> {
        let wanted: HashMap<String, RootRecord> = self
            .ctx
            .state
            .roots()?
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| (r.id.clone(), r))
            .collect();

        let gone: Vec<String> = self
            .running
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(h) = self.running.remove(&id) {
                tracing::info!(root = %h.ctx.root.local_path, "root removed or disabled; stopping");
                h.shutdown().await;
            }
        }
        self.failed.retain(|id, _| wanted.contains_key(id));

        for (id, root) in wanted {
            if self.running.contains_key(&id) {
                continue;
            }
            if self
                .failed
                .get(&id)
                .is_some_and(|t| t.elapsed() < RETRY_FAILED)
            {
                continue;
            }
            let path = root.local_path.clone();
            tracing::info!(root = %path, id = %id, remote = %root.remote_path, "starting root");
            match self.start(root).await {
                Ok(h) => {
                    tracing::info!(root = %path, "syncing");
                    self.failed.remove(&id);
                    self.running.insert(id, h);
                }
                Err(e) => {
                    tracing::error!(root = %path, error = %format!("{e:#}"), "failed to start root");
                    self.failed.insert(id, Instant::now());
                }
            }
        }
        Ok(())
    }

    async fn start(&mut self, root: RootRecord) -> anyhow::Result<RootHandle> {
        let provider = match self.providers.get(&root.account_id) {
            Some(p) => p.clone(),
            None => {
                let account = self.ctx.account(Some(&root.account_id))?;
                let p = providers::for_account(&self.ctx.cfg, &self.ctx.paths, &account).await?;
                self.providers.insert(root.account_id.clone(), p.clone());
                p
            }
        };
        holo_engine::runtime::start_root(
            root,
            provider,
            self.ctx.state.clone(),
            local_tree(),
            self.engine_cfg.clone(),
            holo_engine::runtime::StartOptions {
                connect_cloud_filter: cfg!(windows),
                load: self.load.clone(),
            },
        )
        .await
    }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let _lock = single_instance(ctx)?;
    let mut d = Daemon {
        ctx,
        engine_cfg: ctx.cfg.engine(&ctx.paths),
        load: crate::load::probe(),
        providers: HashMap::new(),
        running: HashMap::new(),
        failed: HashMap::new(),
    };
    d.reconcile().await?;
    if d.running.is_empty() {
        tracing::warn!("no sync root running yet; waiting for `holo add-root`");
    }
    tracing::info!(
        roots = d.running.len(),
        "holo is running; press Ctrl+C to stop"
    );

    let mut tick = tokio::time::interval(RELOAD_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stop = super::service::stop_requested();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            r = tokio::signal::ctrl_c() => { r?; break; }
            () = &mut stop => { tracing::info!("stop requested by `holo service stop`"); break; }
            _ = tick.tick() => {
                if let Err(e) = d.reconcile().await {
                    tracing::warn!(error = %format!("{e:#}"), "reloading sync roots failed");
                }
            }
        }
    }
    tracing::info!("stopping…");
    for (_, h) in d.running.drain() {
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
