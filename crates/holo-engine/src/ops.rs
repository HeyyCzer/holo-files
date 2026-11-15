//! Local → remote operation journal. Ops are persisted before execution, re-check the
//! current local state when they run (so they are idempotent), and survive crashes.

use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use chrono::Utc;
use holo_core::{ProviderError, RelPath, RemoteItemId, UploadRequest, UploadTarget};
use holo_state::{ItemRecord, OpState};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::context::RootContext;

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

fn backoff(attempts: u32) -> Duration {
    Duration::from_secs((5u64 << attempts.min(7)).min(600))
}

/// Executor loop for one root.
pub async fn run(ctx: Arc<RootContext>, mut shutdown: watch::Receiver<bool>) {
    loop {
        while let Ok(Some(rec)) = ctx.state.next_op(ctx.id()) {
            if *shutdown.borrow() {
                return;
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
            let result = execute(&ctx, &op).await;
            let outcome = match result {
                Ok(()) => ctx.state.complete_op(rec.id),
                Err(e) => match e.downcast_ref::<Defer>() {
                    Some(d) => {
                        tracing::debug!(op = ?op, reason = %d.reason, "op deferred");
                        ctx.state.retry_op(rec.id, &d.reason, Utc::now() + d.delay)
                    }
                    None if rec.attempts + 1 >= ctx.cfg.max_op_attempts => {
                        tracing::error!(op = ?op, error = %format!("{e:#}"), "op failed permanently");
                        ctx.state
                            .set_op_state(rec.id, OpState::Failed, Some(&format!("{e:#}")))
                    }
                    None => {
                        let delay = backoff(rec.attempts);
                        tracing::warn!(op = ?op, error = %format!("{e:#}"), retry_in = ?delay, "op failed");
                        ctx.state
                            .retry_op(rec.id, &format!("{e:#}"), Utc::now() + delay)
                    }
                },
            };
            if let Err(e) = outcome {
                tracing::error!(error = %e, "failed to update op journal");
            }
        }

        let wait = ctx
            .state
            .next_op_due(ctx.id())
            .ok()
            .flatten()
            .and_then(|due| (due - Utc::now()).to_std().ok())
            .unwrap_or(Duration::from_secs(30))
            .min(Duration::from_secs(30));
        tokio::select! {
            _ = ctx.wake_ops.notified() => {}
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
    }
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
    let parent = Box::pin(ensure_remote_dir(ctx, &rel.parent().unwrap_or_default())).await?;
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
                return Err(defer(
                    "file locked by another process",
                    Duration::from_secs(10),
                ));
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
