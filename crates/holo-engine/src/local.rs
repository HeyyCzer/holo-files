//! Local → journal: turn Cloud Filter notifications into durable ops.
//!
//! Callbacks only enqueue `LocalEvent`s; this task debounces them, classifies each
//! path against its current state and writes ops to the journal.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use holo_cfapi::{LocalEvent, PinState};
use holo_core::RelPath;
use tokio::sync::{mpsc, watch};

use crate::{
    context::{RootContext, is_ignore_file},
    ops::{self, LocalOp},
};

const DELETE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
pub enum Classified {
    Upload(RelPath),
    CreateFolder(RelPath),
    Hydrate(PathBuf),
    Dehydrate(PathBuf),
    /// Look again later (file still being written).
    Defer(PathBuf),
    /// New folder: its children need classifying too.
    Walk(Vec<PathBuf>),
}

/// Decide what a changed path needs (blocking).
pub fn classify(ctx: &RootContext, path: &Path) -> std::io::Result<Vec<Classified>> {
    let Some(rel) = ctx.rel(path) else {
        return Ok(vec![]);
    };
    if rel.is_root() {
        return Ok(vec![]);
    }
    if is_ignore_file(&rel) {
        ctx.reload_ignore_file(&rel.parent().unwrap_or_default());
    }
    if ctx.suppress.is_suppressed(&rel) {
        return Ok(vec![]);
    }
    let Some(st) = ctx.tree.state(path)? else {
        return Ok(vec![]);
    };
    if ctx.is_ignored(&rel, st.is_dir) {
        return Ok(vec![]);
    }

    if st.is_dir {
        if !st.needs_upload() {
            return Ok(vec![]);
        }
        let children = ctx
            .tree
            .read_dir(path)?
            .into_iter()
            .map(|n| path.join(n))
            .collect();
        return Ok(vec![
            Classified::CreateFolder(rel),
            Classified::Walk(children),
        ]);
    }

    match &st.placeholder {
        Some(p) if p.in_sync => Ok(match p.pin {
            PinState::Pinned if p.partial => vec![Classified::Hydrate(path.to_path_buf())],
            PinState::Unpinned if !p.partial => vec![Classified::Dehydrate(path.to_path_buf())],
            _ => vec![],
        }),
        _ => {
            let age = (chrono::Utc::now() - st.modified)
                .to_std()
                .unwrap_or_default();
            if age < ctx.cfg.settle_time {
                Ok(vec![Classified::Defer(path.to_path_buf())])
            } else {
                Ok(vec![Classified::Upload(rel)])
            }
        }
    }
}

struct LocalTask {
    ctx: Arc<RootContext>,
    /// Shutdown signal, checked inside long scans so stopping never waits for them.
    stop: watch::Receiver<bool>,
    dirty: HashMap<PathBuf, Instant>,
    deletes: VecDeque<Instant>,
}

impl LocalTask {
    fn mark(&mut self, p: PathBuf) {
        self.dirty.insert(p, Instant::now());
    }

    /// A `.holoignore` was removed or renamed away: reload its folder's rules.
    fn ignore_file_gone(&self, path: &Path) {
        if let Some(rel) = self.ctx.rel(path)
            && is_ignore_file(&rel)
        {
            let ctx = self.ctx.clone();
            let dir = rel.parent().unwrap_or_default();
            tokio::task::spawn_blocking(move || ctx.reload_ignore_file(&dir));
        }
    }

    fn on_rename(&mut self, from: &Path, to: &Path) -> anyhow::Result<()> {
        self.ignore_file_gone(from);
        let (Some(from_rel), to_rel) = (self.ctx.rel(from), self.ctx.rel(to)) else {
            self.mark(to.to_path_buf());
            return Ok(());
        };
        let Some(to_rel) = to_rel else {
            return self.on_delete(from);
        };
        if self.ctx.suppress.is_suppressed(&from_rel) || self.ctx.suppress.is_suppressed(&to_rel) {
            return Ok(());
        }
        match self.ctx.state.item_by_path(self.ctx.id(), &from_rel)? {
            None => self.mark(to.to_path_buf()),
            Some(item) if self.ctx.is_ignored(&to_rel, item.is_dir) => return self.on_delete(from),
            Some(item) => {
                ops::enqueue(
                    &self.ctx,
                    &LocalOp::Move {
                        id: item.remote_id,
                        to: to_rel,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn on_delete(&mut self, path: &Path) -> anyhow::Result<()> {
        self.ignore_file_gone(path);
        let Some(rel) = self.ctx.rel(path) else {
            return Ok(());
        };
        if self.ctx.suppress.is_suppressed(&rel) {
            return Ok(());
        }
        let Some(item) = self.ctx.state.item_by_path(self.ctx.id(), &rel)? else {
            return Ok(());
        };
        let op = LocalOp::Delete {
            id: item.remote_id,
            path: rel.clone(),
        };

        let now = Instant::now();
        self.deletes.push_back(now);
        while self
            .deletes
            .front()
            .is_some_and(|t| now.duration_since(*t) > DELETE_WINDOW)
        {
            self.deletes.pop_front();
        }
        if self.deletes.len() > self.ctx.cfg.mass_delete_threshold {
            ops::enqueue_held(&self.ctx, &op, "mass delete guard")?;
            if self.deletes.len() == self.ctx.cfg.mass_delete_threshold + 1 {
                tracing::warn!(
                    root = self.ctx.id(),
                    "more than {} deletes in a minute: holding them. Review with `holo ops` and run `holo ops release` or `holo ops discard`.",
                    self.ctx.cfg.mass_delete_threshold
                );
            }
        } else {
            ops::enqueue(&self.ctx, &op)?;
        }
        Ok(())
    }

    async fn process_due(&mut self) {
        let debounce = self.ctx.cfg.debounce;
        let now = Instant::now();
        let due: Vec<PathBuf> = self
            .dirty
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= debounce)
            .map(|(p, _)| p.clone())
            .collect();
        if due.is_empty() {
            return;
        }
        for p in &due {
            self.dirty.remove(p);
        }
        let ctx = self.ctx.clone();
        let results = tokio::task::spawn_blocking(move || {
            due.into_iter()
                .map(|p| {
                    let r = classify(&ctx, &p);
                    (p, r)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        for (path, result) in results {
            match result {
                Ok(actions) => {
                    for a in actions {
                        self.act(a);
                    }
                }
                Err(e) => tracing::debug!(path = %path.display(), error = %e, "classify failed"),
            }
        }
    }

    fn act(&mut self, a: Classified) {
        let ctx = &self.ctx;
        let r = match a {
            Classified::Upload(rel) => {
                ops::enqueue(ctx, &LocalOp::Upload { path: rel }).map(|_| ())
            }
            Classified::CreateFolder(rel) => {
                ops::enqueue(ctx, &LocalOp::CreateFolder { path: rel }).map(|_| ())
            }
            Classified::Defer(p) => {
                self.mark(p);
                Ok(())
            }
            Classified::Walk(children) => {
                for c in children {
                    self.mark(c);
                }
                Ok(())
            }
            Classified::Hydrate(p) => {
                let tree = ctx.tree.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = tree.hydrate(&p) {
                        tracing::warn!(path = %p.display(), error = %e, "pin: hydration failed");
                    }
                });
                Ok(())
            }
            Classified::Dehydrate(p) => {
                let tree = ctx.tree.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = tree.dehydrate(&p) {
                        tracing::debug!(path = %p.display(), error = %e, "free up space: dehydration failed");
                    }
                });
                Ok(())
            }
        };
        if let Err(e) = r {
            tracing::error!(error = %e, "failed to journal local change");
        }
    }

    /// Walk the whole root; with `detect_missing`, known items absent locally are
    /// treated as deleted while we weren't watching.
    /// Abandoned (nothing journaled) if shutdown is requested meanwhile.
    async fn scan(&mut self, detect_missing: bool) {
        let ctx = self.ctx.clone();
        let stop = self.stop.clone();
        let scan = tokio::task::spawn_blocking(move || {
            let stopping = || *stop.borrow();
            let mut dirty = Vec::new();
            let mut stack = vec![ctx.local_root.clone()];
            while let Some(dir) = stack.pop() {
                if stopping() {
                    return None;
                }
                let Ok(names) = ctx.tree.read_dir(&dir) else {
                    continue;
                };
                // Rules first, so they apply to this folder's own entries.
                if names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(holo_core::ignore_rules::IGNORE_FILE))
                    && let Some(d) = ctx.rel(&dir)
                {
                    ctx.reload_ignore_file(&d);
                }
                for n in names {
                    let p = dir.join(&n);
                    let Some(rel) = ctx.rel(&p) else { continue };
                    let Ok(Some(st)) = ctx.tree.state(&p) else {
                        continue;
                    };
                    if ctx.is_ignored(&rel, st.is_dir) {
                        continue;
                    }
                    let pin_mismatch = st.placeholder.as_ref().is_some_and(|ph| {
                        (ph.pin == PinState::Pinned && ph.partial)
                            || (ph.pin == PinState::Unpinned && !ph.partial)
                    });
                    if st.needs_upload() || (!st.is_dir && pin_mismatch) {
                        dirty.push(p.clone());
                    }
                    if st.is_dir && !st.needs_upload() {
                        stack.push(p);
                    }
                }
            }
            let mut missing = Vec::new();
            if detect_missing {
                for (i, item) in ctx
                    .state
                    .all_items(ctx.id())
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    if i % 1024 == 0 && stopping() {
                        return None;
                    }
                    let p = ctx.local(&item.rel_path);
                    if matches!(ctx.tree.state(&p), Ok(None)) {
                        missing.push(p);
                    }
                }
            }
            Some((dirty, missing))
        });
        let Ok(Some((dirty, missing))) = scan.await else {
            return;
        };
        tracing::info!(
            root = self.ctx.id(),
            changed = dirty.len(),
            missing = missing.len(),
            "local scan"
        );
        for p in dirty {
            self.mark(p);
        }
        // Shallowest first; deleting a parent makes child deletes redundant.
        let mut missing = missing;
        missing.sort_by_key(|p| p.components().count());
        for p in missing {
            if let Err(e) = self.on_delete(&p) {
                tracing::warn!(error = %e, "failed to journal offline delete");
            }
        }
    }
}

/// Event loop for one root. Waits for `ready` (first remote pull) before the startup scan.
pub async fn run(
    ctx: Arc<RootContext>,
    mut events: mpsc::UnboundedReceiver<LocalEvent>,
    mut ready: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut task = LocalTask {
        ctx: ctx.clone(),
        stop: shutdown.clone(),
        dirty: HashMap::new(),
        deletes: VecDeque::new(),
    };
    tokio::select! {
        _ = ready.wait_for(|r| *r) => {}
        _ = shutdown.changed() => return,
    }
    task.scan(true).await;

    let mut full_scan = tokio::time::interval(ctx.cfg.full_scan_interval);
    full_scan.tick().await;
    loop {
        let next = task.dirty.values().min().map(|t| *t + ctx.cfg.debounce);
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return };
                let r = match ev {
                    LocalEvent::Changed(paths) => { paths.into_iter().for_each(|p| task.mark(p)); Ok(()) }
                    LocalEvent::Renamed { from, to } => task.on_rename(&from, &to),
                    LocalEvent::Deleted { path } => task.on_delete(&path),
                    LocalEvent::WatcherStopped(e) => {
                        tracing::error!(root = ctx.id(), error = %e, "directory watcher stopped; falling back to scans");
                        task.scan(false).await;
                        Ok(())
                    }
                };
                if let Err(e) = r { tracing::error!(error = %e, "local event handling failed"); }
            }
            _ = async { tokio::time::sleep_until(next.expect("guarded").into()).await }, if next.is_some() => {
                task.process_due().await;
            }
            _ = full_scan.tick() => task.scan(false).await,
            _ = shutdown.changed() => return,
        }
    }
}
