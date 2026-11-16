use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, atomic::AtomicU32},
    time::{Duration, Instant},
};

use holo_cfapi::LocalEvent;
use holo_core::{
    CloudProvider, RelPath, RemoteItemId,
    ignore_rules::{IGNORE_FILE, IgnoreRules},
};
use holo_state::{RootRecord, StateStore};
use tokio::sync::{Notify, mpsc};

use crate::{EngineConfig, LocalTree, ops::LocalOp};

/// Everything a root's tasks share.
pub struct RootContext {
    pub root: RootRecord,
    pub local_root: PathBuf,
    pub provider: Arc<dyn CloudProvider>,
    pub state: Arc<StateStore>,
    pub tree: Arc<dyn LocalTree>,
    /// Replaced as a whole when a `.holoignore` changes (see `reload_ignore_file`).
    pub ignore: RwLock<Arc<IgnoreRules>>,
    pub cfg: EngineConfig,
    pub suppress: Suppressor,
    /// Wake the remote poller early (e.g. after a conflict).
    pub wake_remote: Notify,
    /// Wake the op executor (new op enqueued).
    pub wake_ops: Notify,
    pub events: mpsc::UnboundedSender<LocalEvent>,
    /// Consecutive failed remote applies (see `remote::pull`).
    pub pull_failures: AtomicU32,
    /// Serializes remote folder creation so parallel ops don't race to create
    /// the same parent.
    pub dir_lock: tokio::sync::Mutex<()>,
    /// How busy the PC is, for adaptive concurrency (`None`: unknown).
    pub load: Option<Arc<dyn crate::adaptive::LoadProbe>>,
}

impl RootContext {
    pub fn id(&self) -> &str {
        &self.root.id
    }

    pub fn remote_root(&self) -> &RemoteItemId {
        &self.root.remote_root_id
    }

    pub fn local(&self, rel: &RelPath) -> PathBuf {
        rel.to_local(&self.local_root)
    }

    pub fn rel(&self, path: &Path) -> Option<RelPath> {
        RelPath::from_local(&self.local_root, path)
    }

    /// Current ignore rules.
    pub fn ignore_rules(&self) -> Arc<IgnoreRules> {
        self.ignore
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn is_ignored(&self, rel: &RelPath, is_dir: bool) -> bool {
        self.ignore_rules().is_ignored(rel, is_dir)
    }

    /// Re-read `<dir>/.holoignore` (blocking; may hydrate it) and, if the rules
    /// changed, drop queued ops for paths that are now ignored.
    pub fn reload_ignore_file(&self, dir: &RelPath) {
        let file = self.local(&dir.join(IGNORE_FILE));
        let text = match self.tree.read_text(&file) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "can't read ignore file");
                return;
            }
        };
        {
            let mut cur = self.ignore.write().unwrap_or_else(|p| p.into_inner());
            let mut rules = (**cur).clone();
            match &text {
                Some(t) => {
                    if let Err(e) = rules.set_file(dir, t) {
                        tracing::warn!(file = %file.display(), error = %e, "invalid ignore file; keeping the previous rules");
                        return;
                    }
                }
                None => {
                    if !rules.remove_file(dir) {
                        return;
                    }
                }
            }
            *cur = Arc::new(rules);
        }
        tracing::info!(root = self.id(), file = %file.display(), removed = text.is_none(), "ignore rules reloaded");
        self.drop_ignored_ops();
    }

    /// Load every `.holoignore` the state store knows about (blocking). The root's
    /// file is read at startup; nested ones are found here and by local scans.
    pub fn load_known_ignore_files(&self) {
        let Ok(items) = self.state.all_items(self.id()) else {
            return;
        };
        for item in items {
            if !item.is_dir
                && item.rel_path.name().eq_ignore_ascii_case(IGNORE_FILE)
                && let Some(dir) = item.rel_path.parent()
                && !dir.is_root()
            {
                self.reload_ignore_file(&dir);
            }
        }
    }

    /// Forget journaled uploads/folder creations whose path is now ignored.
    pub fn drop_ignored_ops(&self) {
        let Ok(ops) = self.state.ops(Some(self.id())) else {
            return;
        };
        let rules = self.ignore_rules();
        let mut dropped = 0;
        for rec in ops {
            let Ok(op) = serde_json::from_str::<LocalOp>(&rec.payload) else {
                continue;
            };
            let ignored = match &op {
                LocalOp::CreateFolder { path } => rules.is_ignored(path, true),
                LocalOp::Upload { path } => {
                    let is_dir =
                        matches!(self.tree.state(&self.local(path)), Ok(Some(st)) if st.is_dir);
                    rules.is_ignored(path, is_dir)
                }
                LocalOp::Move { .. } | LocalOp::Delete { .. } => false,
            };
            if ignored && self.state.complete_op(rec.id).is_ok() {
                dropped += 1;
            }
        }
        if dropped > 0 {
            tracing::info!(
                root = self.id(),
                dropped,
                "dropped queued ops for newly ignored paths"
            );
        }
    }
}

/// Whether `rel` is a `.holoignore` file name.
pub fn is_ignore_file(rel: &RelPath) -> bool {
    !rel.is_root() && rel.name().eq_ignore_ascii_case(IGNORE_FILE)
}

/// Paths the engine itself is changing. Filesystem notifications for them are ignored
/// for a short while, so we never echo our own writes back to the cloud.
#[derive(Default)]
pub struct Suppressor {
    entries: Mutex<HashMap<String, Instant>>,
}

const SUPPRESS_FOR: Duration = Duration::from_secs(5);

impl Suppressor {
    pub fn add(&self, rel: &RelPath) {
        let mut m = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        m.insert(rel.key(), Instant::now() + SUPPRESS_FOR);
    }

    pub fn is_suppressed(&self, rel: &RelPath) -> bool {
        let mut m = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        m.retain(|_, until| *until > now);
        m.contains_key(&rel.key())
    }
}
