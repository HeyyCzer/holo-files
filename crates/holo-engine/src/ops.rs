//! Local → remote operation journal. Ops are persisted before execution, re-check the
//! current local state when they run (so they are idempotent), and survive crashes.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context as _;
use chrono::Utc;
use holo_core::{ProviderError, RelPath, RemoteItemId, UploadRequest, UploadTarget};
use holo_state::{ItemRecord, OpRecord, OpState};
use serde::{Deserialize, Serialize};
use tokio::{sync::watch, task::JoinSet};

use crate::{adaptive::Controller, context::RootContext};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalOp {
    Upload { path: RelPath },
    CreateFolder { path: RelPath },
    Move { id: RemoteItemId, to: RelPath },
    Delete { id: RemoteItemId, path: RelPath },
}

impl LocalOp {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Upload { .. } => "upload",
            Self::CreateFolder { .. } => "create_folder",
            Self::Move { .. } => "move",
            Self::Delete { .. } => "delete",
        }
    }
}

/// Run later without counting as a failure (file busy, remote not caught up, …).
#[derive(Debug, thiserror::Error)]
#[error("deferred: {reason}")]
pub struct Defer {
    pub reason: String,
    pub delay: Duration,
}

fn defer(reason: impl Into<String>, delay: Duration) -> anyhow::Error {
    Defer {
        reason: reason.into(),
        delay,
    }
    .into()
}

pub fn enqueue(ctx: &RootContext, op: &LocalOp) -> anyhow::Result<i64> {
    let id = ctx
        .state
        .enqueue_op(ctx.id(), op.kind(), &serde_json::to_string(op)?)?;
    ctx.wake_ops.notify_one();
    Ok(id)
}

/// Enqueue but block until the user confirms (`holo ops release`).
pub fn enqueue_held(ctx: &RootContext, op: &LocalOp, reason: &str) -> anyhow::Result<i64> {
    let id = ctx
        .state
        .enqueue_op(ctx.id(), op.kind(), &serde_json::to_string(op)?)?;
    ctx.state.set_op_state(id, OpState::Held, Some(reason))?;
    Ok(id)
}

/// How long to wait before looking again at a file another process is writing.
const IN_USE_RETRY: Duration = Duration::from_secs(60);

fn backoff(attempts: u32) -> Duration {
    Duration::from_secs((5u64 << attempts.min(7)).min(600))
}

/// What the executor is doing right now; published to the state store (kv
/// `progress/<root id>`) so `holo status --stream` / `holo ops --stream` can show it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecProgress {
    /// Ops finished (completed or given up) since the daemon started.
    pub done: u64,
    pub failed: u64,
    pub running: Vec<RunningOp>,
    /// Ops allowed to run at once right now, its upper bound, and why.
    #[serde(default)]
    pub workers: usize,
    #[serde(default)]
    pub max_workers: usize,
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub workers_reason: String,
    /// Sync paused by the user: running ops finish, no new ones start.
    #[serde(default)]
    pub paused: bool,
    /// Unix ms of the last update.
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningOp {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub started_ms: i64,
}

pub fn progress_key(root_id: &str) -> String {
    format!("progress/{root_id}")
}

/// Moves and deletes rewrite paths other ops depend on, so they run alone and in
/// journal order. Everything else runs in parallel, never two ops on the same path.
fn exclusive(op: &LocalOp) -> bool {
    matches!(op, LocalOp::Move { .. } | LocalOp::Delete { .. })
}

fn op_path(op: &LocalOp) -> &RelPath {
    match op {
        LocalOp::Upload { path } | LocalOp::CreateFolder { path } => path,
        LocalOp::Move { to, .. } => to,
        LocalOp::Delete { path, .. } => path,
    }
}

type Finished = (OpRecord, LocalOp, anyhow::Result<()>);

/// How often adaptive concurrency is re-evaluated.
const ADAPT_EVERY: Duration = Duration::from_secs(2);

/// Executor loop for one root; how many ops run at once comes from `cfg.concurrency`.
pub async fn run(ctx: Arc<RootContext>, mut shutdown: watch::Receiver<bool>) {
    let mut control = Controller::new(ctx.cfg.concurrency);
    let mut adapt = tokio::time::interval(ADAPT_EVERY);
    adapt.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tasks: JoinSet<Finished> = JoinSet::new();
    let mut running: HashMap<i64, (RelPath, RunningOp)> = HashMap::new();
    let mut barrier = false;
    let mut progress = ExecProgress {
        max_workers: ctx.cfg.concurrency.max(),
        auto: control.is_auto(),
        ..Default::default()
    };
    let publish = |p: &mut ExecProgress, running: &HashMap<i64, (RelPath, RunningOp)>| {
        p.running = running.values().map(|(_, r)| r.clone()).collect();
        p.running.sort_by_key(|r| r.id);
        p.updated_ms = Utc::now().timestamp_millis();
        if let Ok(json) = serde_json::to_string(p) {
            let _ = ctx.state.kv_set(&progress_key(ctx.id()), &json);
        }
    };
    let adjust = |control: &mut Controller, progress: &mut ExecProgress| {
        if control.is_auto() {
            let backlog = ctx
                .state
                .op_counts(ctx.id())
                .map(|c| c.pending)
                .unwrap_or(0);
            let load = ctx.load.as_ref().and_then(|p| p.sample());
            let before = control.workers();
            let now = control.update(
                std::time::Instant::now(),
                backlog,
                ctx.provider.throttle_count(),
                load,
            );
            if now != before {
                tracing::debug!(workers = now, reason = control.reason(), "concurrency");
            }
        }
        progress.workers = control.workers();
        progress.workers_reason = control.reason().to_string();
    };
    adjust(&mut control, &mut progress);
    publish(&mut progress, &running);

    loop {
        if *shutdown.borrow() {
            break;
        }
        let workers = control.workers();
        let paused = ctx.paused();
        progress.paused = paused;
        // Fill free slots, oldest ops first.
        if !paused && !barrier && tasks.len() < workers {
            let started = tasks.len();
            match ctx.state.due_ops(ctx.id(), workers * 4 + running.len()) {
                Ok(recs) => {
                    for rec in recs {
                        if tasks.len() >= workers {
                            break;
                        }
                        if running.contains_key(&rec.id) {
                            continue;
                        }
                        let op: LocalOp = match serde_json::from_str(&rec.payload) {
                            Ok(op) => op,
                            Err(e) => {
                                let _ = ctx.state.set_op_state(
                                    rec.id,
                                    OpState::Failed,
                                    Some(&format!("bad payload: {e}")),
                                );
                                continue;
                            }
                        };
                        let path = op_path(&op).clone();
                        if exclusive(&op) {
                            if !tasks.is_empty() {
                                break; // let earlier ops finish first
                            }
                            barrier = true;
                        } else if running.values().any(|(p, _)| *p == path) {
                            continue;
                        }
                        running.insert(
                            rec.id,
                            (
                                path.clone(),
                                RunningOp {
                                    id: rec.id,
                                    kind: op.kind().to_string(),
                                    path: path.as_str().to_string(),
                                    started_ms: Utc::now().timestamp_millis(),
                                },
                            ),
                        );
                        let c = ctx.clone();
                        tasks.spawn(async move {
                            let r = execute(&c, &op).await;
                            (rec, op, r)
                        });
                        if barrier {
                            break;
                        }
                    }
                }
                Err(e) => tracing::error!(error = %e, "failed to read op journal"),
            }
            if tasks.len() != started {
                publish(&mut progress, &running);
            }
        }

        if tasks.is_empty() {
            let wait = ctx
                .state
                .next_op_due(ctx.id())
                .ok()
                .flatten()
                .and_then(|due| (due - Utc::now()).to_std().ok())
                .unwrap_or(Duration::from_secs(30))
                .min(Duration::from_secs(30));
            let wait = if paused { ADAPT_EVERY } else { wait };
            tokio::select! {
                _ = ctx.wake_ops.notified() => {}
                _ = tokio::time::sleep(wait) => {}
                _ = adapt.tick() => {
                    adjust(&mut control, &mut progress);
                    publish(&mut progress, &running);
                }
                _ = shutdown.changed() => break,
            }
            continue;
        }

        tokio::select! {
            joined = tasks.join_next() => {
                let Some(joined) = joined else { continue };
                match joined {
                    Ok((rec, op, result)) => {
                        running.remove(&rec.id);
                        if exclusive(&op) {
                            barrier = false;
                        }
                        match finish(&ctx, &rec, &op, result) {
                            Some(true) => progress.done += 1,
                            Some(false) => { progress.done += 1; progress.failed += 1; }
                            None => {}
                        }
                    }
                    Err(e) => {
                        // A panicking op: forget what was running; the journal row stays
                        // pending and is picked up again.
                        tracing::error!(error = %e, "op task panicked");
                        running.clear();
                        barrier = false;
                    }
                }
                publish(&mut progress, &running);
            }
            _ = ctx.wake_ops.notified() => {}
            _ = adapt.tick() => {
                adjust(&mut control, &mut progress);
                publish(&mut progress, &running);
            }
            _ = shutdown.changed() => break,
        }
    }
    // Ops are idempotent and still journaled: dropping in-flight ones is safe.
    tasks.shutdown().await;
    running.clear();
    publish(&mut progress, &running);
}

/// Record an op's outcome. `Some(true)` done, `Some(false)` failed for good, `None` retried later.
fn finish(
    ctx: &RootContext,
    rec: &OpRecord,
    op: &LocalOp,
    result: anyhow::Result<()>,
) -> Option<bool> {
    let (outcome, status) = match result {
        Ok(()) => (ctx.state.complete_op(rec.id), Some(true)),
        Err(e) => match e.downcast_ref::<Defer>() {
            Some(d) => {
                tracing::debug!(op = ?op, reason = %d.reason, "op deferred");
                (
                    ctx.state.retry_op(rec.id, &d.reason, Utc::now() + d.delay),
                    None,
                )
            }
            None if rec.attempts + 1 >= ctx.cfg.max_op_attempts => {
                tracing::error!(op = ?op, error = %format!("{e:#}"), "op failed permanently");
                (
                    ctx.state
                        .set_op_state(rec.id, OpState::Failed, Some(&format!("{e:#}"))),
                    Some(false),
                )
            }
            None => {
                let delay = backoff(rec.attempts);
                tracing::warn!(op = ?op, error = %format!("{e:#}"), retry_in = ?delay, "op failed");
                (
                    ctx.state
                        .retry_op(rec.id, &format!("{e:#}"), Utc::now() + delay),
                    None,
                )
            }
        },
    };
    if let Err(e) = outcome {
        tracing::error!(error = %e, "failed to update op journal");
    }
    status
}

async fn blocking<T: Send + 'static>(
    ctx: &Arc<RootContext>,
    f: impl FnOnce(&RootContext) -> std::io::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let c = ctx.clone();
    Ok(tokio::task::spawn_blocking(move || f(&c)).await??)
}

pub async fn execute(ctx: &Arc<RootContext>, op: &LocalOp) -> anyhow::Result<()> {
    match op {
        LocalOp::Upload { path } => upload(ctx, path).await,
        LocalOp::CreateFolder { path } => ensure_remote_dir(ctx, path).await.map(|_| ()),
        LocalOp::Move { id, to } => move_item(ctx, id, to).await,
        LocalOp::Delete { id, path } => delete(ctx, id, path).await,
    }
}

/// Remote id of folder `rel`, creating it (and its parents) remotely if needed.
pub async fn ensure_remote_dir(
    ctx: &Arc<RootContext>,
    rel: &RelPath,
) -> anyhow::Result<RemoteItemId> {
    if rel.is_root() {
        return Ok(ctx.remote_root().clone());
    }
    if let Some(r) = ctx.state.item_by_path(ctx.id(), rel)?
        && r.is_dir
    {
        return Ok(r.remote_id);
    }
    let _guard = ctx.dir_lock.lock().await;
    create_remote_dir(ctx, rel).await
}

/// `ensure_remote_dir` body; caller holds `dir_lock` (re-checks the state under it).
async fn create_remote_dir(ctx: &Arc<RootContext>, rel: &RelPath) -> anyhow::Result<RemoteItemId> {
    if rel.is_root() {
        return Ok(ctx.remote_root().clone());
    }
    if let Some(r) = ctx.state.item_by_path(ctx.id(), rel)?
        && r.is_dir
    {
        return Ok(r.remote_id);
    }
    let parent = Box::pin(create_remote_dir(ctx, &rel.parent().unwrap_or_default())).await?;
    let item = match ctx.provider.create_folder(&parent, rel.name()).await {
        Ok(item) => item,
        Err(ProviderError::NameConflict) => {
            let remote_path = format!(
                "{}/{}",
                ctx.root.remote_path.trim_end_matches('/'),
                rel.as_str()
            );
            ctx.provider.item_by_path(&remote_path).await?
        }
        Err(e) => return Err(e.into()),
    };
    let (local, id) = (ctx.local(rel), item.id.clone());
    ctx.suppress.add(rel);
    blocking(ctx, move |c| match c.tree.state(&local)? {
        Some(_) => c.tree.mark_synced(&local, id.as_str().as_bytes(), None),
        None => Ok(()),
    })
    .await?;
    ctx.state
        .upsert_item(&ItemRecord::from_remote(ctx.id(), &item, rel.clone()))?;
    Ok(item.id)
}

async fn upload(ctx: &Arc<RootContext>, path: &RelPath) -> anyhow::Result<()> {
    let local = ctx.local(path);
    let l = local.clone();
    let Some(st) = blocking(ctx, move |c| c.tree.state(&l)).await? else {
        return Ok(());
    };
    if ctx.is_ignored(path, st.is_dir) {
        return Ok(());
    }
    if st.is_dir {
        return ensure_remote_dir(ctx, path).await.map(|_| ());
    }
    if !st.needs_upload() {
        return Ok(());
    }
    let age = Utc::now() - st.modified;
    if age.to_std().unwrap_or_default() < ctx.cfg.settle_time {
        return Err(defer("file still being written", ctx.cfg.settle_time));
    }
    // Checked locally before any hashing or network: logs and databases held open
    // for writing would otherwise be uploaded, change mid-upload, and loop forever.
    let l = local.clone();
    if blocking(ctx, move |c| c.tree.in_use(&l)).await? {
        return Err(defer("file open by another process", IN_USE_RETRY));
    }

    let parent_id = ensure_remote_dir(ctx, &path.parent().unwrap_or_default()).await?;
    let by_identity = match st.identity().and_then(|b| std::str::from_utf8(b).ok()) {
        Some(id) => ctx.state.item(ctx.id(), &RemoteItemId::new(id))?,
        None => None,
    };
    let mut base = match by_identity {
        Some(b) => Some(b),
        None => ctx
            .state
            .item_by_path(ctx.id(), path)?
            .filter(|b| !b.is_dir),
    };

    // Unchanged content (touch, save without edits, first sync over identical files).
    if let Some(b) = &base
        && let Some(h) = b.hash.clone()
    {
        let (l, id) = (local.clone(), b.remote_id.clone());
        let same = blocking(ctx, move |c| {
            let same = c.tree.hash(&l, h.algo)?.as_ref() == Some(&h);
            if same {
                c.tree.mark_synced(&l, id.as_str().as_bytes(), None)?;
            }
            Ok(same)
        })
        .await?;
        if same {
            return Ok(());
        }
    }

    let l = local.clone();
    let usn = blocking(ctx, move |c| c.tree.usn(&l)).await?;
    loop {
        let target = match &base {
            Some(b) => UploadTarget::Replace {
                id: &b.remote_id,
                if_match: b.etag.as_deref(),
            },
            None => UploadTarget::New {
                parent: &parent_id,
                name: path.name(),
            },
        };
        let req = UploadRequest {
            target,
            source: &local,
            size: st.size,
        };
        match ctx.provider.upload(req).await {
            Ok(item) => {
                ctx.suppress.add(path);
                let (l, id) = (local.clone(), item.id.clone());
                let marked = blocking(ctx, move |c| {
                    c.tree.mark_synced(&l, id.as_str().as_bytes(), usn)
                })
                .await;
                ctx.state
                    .upsert_item(&ItemRecord::from_remote(ctx.id(), &item, path.clone()))?;
                if let Err(e) = marked {
                    tracing::info!(path = %path, error = %e, "file changed during upload; uploading again");
                    enqueue(ctx, &LocalOp::Upload { path: path.clone() })?;
                }
                tracing::info!(path = %path, size = st.size, "uploaded");
                return Ok(());
            }
            Err(ProviderError::PreconditionFailed) => {
                // Changed remotely since our base: keep both versions.
                let p = path.clone();
                blocking(ctx, move |c| {
                    crate::remote::move_aside(c, &p)
                        .map(|_| ())
                        .map_err(std::io::Error::other)
                })
                .await?;
                ctx.wake_remote.notify_one();
                return Ok(());
            }
            Err(ProviderError::NameConflict) => {
                // Exists remotely but we haven't seen it yet: let the poller catch up.
                ctx.wake_remote.notify_one();
                return Err(defer(
                    "name exists remotely; waiting for remote sync",
                    Duration::from_secs(15),
                ));
            }
            Err(ProviderError::NotFound) if base.is_some() => {
                // Base was deleted remotely: upload as a new file instead.
                ctx.state
                    .delete_item(ctx.id(), &base.take().expect("checked").remote_id)?;
            }
            Err(ProviderError::Io(e))
                if e.raw_os_error() == Some(32) || e.raw_os_error() == Some(33) =>
            {
                return Err(defer("file open by another process", IN_USE_RETRY));
            }
            Err(e) => return Err(e).context("upload"),
        }
    }
}

async fn move_item(ctx: &Arc<RootContext>, id: &RemoteItemId, to: &RelPath) -> anyhow::Result<()> {
    let Some(current) = ctx.state.item(ctx.id(), id)? else {
        enqueue(ctx, &LocalOp::Upload { path: to.clone() })?;
        return Ok(());
    };
    if current.rel_path == *to {
        return Ok(());
    }
    if ctx.is_ignored(to, current.is_dir) {
        return delete(ctx, id, &current.rel_path).await;
    }
    let parent = ensure_remote_dir(ctx, &to.parent().unwrap_or_default()).await?;
    match ctx.provider.move_item(id, &parent, to.name()).await {
        Ok(item) => {
            if current.is_dir {
                ctx.state.rebase_subtree(ctx.id(), &current.rel_path, to)?;
            }
            ctx.state
                .upsert_item(&ItemRecord::from_remote(ctx.id(), &item, to.clone()))?;
            tracing::info!(from = %current.rel_path, to = %to, "moved");
            Ok(())
        }
        Err(ProviderError::NotFound) => {
            ctx.state.delete_subtree(ctx.id(), &current.rel_path)?;
            let l = ctx.local(to);
            blocking(ctx, move |c| c.tree.detach(&l)).await?;
            enqueue(ctx, &LocalOp::Upload { path: to.clone() })?;
            Ok(())
        }
        Err(ProviderError::NameConflict) => {
            ctx.wake_remote.notify_one();
            Err(defer(
                "target name exists remotely",
                Duration::from_secs(15),
            ))
        }
        Err(e) => Err(e).context("move"),
    }
}

async fn delete(ctx: &Arc<RootContext>, id: &RemoteItemId, path: &RelPath) -> anyhow::Result<()> {
    // Re-check: deletes can be undone, and the item may have moved since.
    let Some(current) = ctx.state.item(ctx.id(), id)? else {
        return Ok(());
    };
    let (here, there) = (ctx.local(path), ctx.local(&current.rel_path));
    let wanted = id.as_str().as_bytes().to_vec();
    let still_exists = blocking(ctx, move |c| {
        for p in [&here, &there] {
            if let Some(st) = c.tree.state(p)?
                && st.identity() == Some(wanted.as_slice())
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await?;
    if still_exists {
        return Ok(());
    }
    ctx.provider.delete(id).await.context("delete")?;
    if current.is_dir {
        ctx.state.delete_subtree(ctx.id(), &current.rel_path)?;
    }
    ctx.state.delete_item(ctx.id(), id)?;
    tracing::info!(path = %current.rel_path, "deleted remotely (recoverable from the cloud recycle bin)");
    Ok(())
}
