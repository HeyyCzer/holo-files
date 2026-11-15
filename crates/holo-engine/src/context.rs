use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicU32},
    time::{Duration, Instant},
};

use holo_cfapi::LocalEvent;
use holo_core::{CloudProvider, RelPath, RemoteItemId, ignore_rules::IgnoreRules};
use holo_state::{RootRecord, StateStore};
use tokio::sync::{Notify, mpsc};

use crate::{EngineConfig, LocalTree};

/// Everything a root's tasks share.
pub struct RootContext {
    pub root: RootRecord,
    pub local_root: PathBuf,
    pub provider: Arc<dyn CloudProvider>,
    pub state: Arc<StateStore>,
    pub tree: Arc<dyn LocalTree>,
    pub ignore: IgnoreRules,
    pub cfg: EngineConfig,
    pub suppress: Suppressor,
    /// Wake the remote poller early (e.g. after a conflict).
    pub wake_remote: Notify,
    /// Wake the op executor (new op enqueued).
    pub wake_ops: Notify,
    pub events: mpsc::UnboundedSender<LocalEvent>,
    /// Consecutive failed remote applies (see `remote::pull`).
    pub pull_failures: AtomicU32,
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

    pub fn is_ignored(&self, rel: &RelPath, is_dir: bool) -> bool {
        self.ignore.is_ignored(rel, is_dir)
    }
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
