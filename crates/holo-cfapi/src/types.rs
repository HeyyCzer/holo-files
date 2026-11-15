use std::{ops::Range, path::PathBuf};

use chrono::{DateTime, Utc};

/// Local filesystem events reported by the Cloud Filter bridge. Delivered from
/// platform threads; handlers must only enqueue.
#[derive(Debug, Clone)]
pub enum LocalEvent {
    /// Paths whose state may have changed (content, attributes, pin state, new files).
    Changed(Vec<PathBuf>),
    /// A placeholder was renamed or moved inside the sync root.
    Renamed { from: PathBuf, to: PathBuf },
    /// A placeholder was deleted (or moved out of the sync root).
    Deleted { path: PathBuf },
    /// The directory watcher died; the engine should rescan and reconnect.
    WatcherStopped(String),
}

/// A hydration request from the platform, already aligned for transfer.
#[derive(Debug, Clone)]
pub struct HydrationRequest {
    pub path: PathBuf,
    /// Placeholder identity blob (the engine stores the remote item id here).
    pub identity: Vec<u8>,
    pub file_size: u64,
    /// Byte range to fetch; start is 4 KiB aligned, end is aligned or EOF.
    pub range: Range<u64>,
    /// Image name of the process that triggered the read, when known.
    pub process: Option<String>,
}

/// Where hydration bytes go. Bytes must be pushed sequentially from `range.start`.
pub trait HydrationSink {
    fn push(&mut self, data: &[u8]) -> std::io::Result<()>;
}

#[derive(Debug, thiserror::Error)]
pub enum HydrateError {
    #[error("offline: {0}")]
    Offline(String),
    #[error("not found remotely")]
    NotFound,
    #[error("hydration refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Other(String),
}

/// Implemented by the engine; the Cloud Filter bridge calls into it.
pub trait CallbackHandler: Send + Sync + 'static {
    /// Blocking: runs on a platform worker thread while the reader waits.
    fn hydrate(
        &self,
        req: HydrationRequest,
        sink: &mut dyn HydrationSink,
    ) -> Result<(), HydrateError>;

    /// Non-blocking: enqueue and return.
    fn notify(&self, event: LocalEvent);
}

/// Metadata used to create/update a placeholder.
#[derive(Debug, Clone)]
pub struct PlaceholderSpec {
    pub is_dir: bool,
    pub size: u64,
    pub modified: DateTime<Utc>,
    /// Identity blob (≤ 4 KiB). The engine uses the remote item id.
    pub identity: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinState {
    Unspecified,
    /// "Always keep on this device".
    Pinned,
    /// "Free up space".
    Unpinned,
    Excluded,
    Inherit,
}

/// Snapshot of a local path as Cloud Filter sees it.
#[derive(Debug, Clone)]
pub struct LocalState {
    pub is_dir: bool,
    pub size: u64,
    pub modified: DateTime<Utc>,
    /// `None` = regular file/folder, not (yet) a placeholder.
    pub placeholder: Option<PlaceholderState>,
}

#[derive(Debug, Clone)]
pub struct PlaceholderState {
    pub in_sync: bool,
    pub pin: PinState,
    /// Some content is not on disk (dehydrated or partially hydrated).
    pub partial: bool,
    pub identity: Vec<u8>,
}

impl LocalState {
    pub fn is_placeholder(&self) -> bool {
        self.placeholder.is_some()
    }
    /// A local edit not yet reconciled: regular file, or placeholder not in sync.
    pub fn needs_upload(&self) -> bool {
        self.placeholder.as_ref().is_none_or(|p| !p.in_sync)
    }
    pub fn identity(&self) -> Option<&[u8]> {
        self.placeholder
            .as_ref()
            .map(|p| p.identity.as_slice())
            .filter(|b| !b.is_empty())
    }
}

/// Registration parameters for a sync root.
#[derive(Debug, Clone)]
pub struct SyncRootSpec {
    /// Stable provider name shown in the registry (no `!`).
    pub provider_name: String,
    /// Unique per root (becomes part of the sync root id; no `!`).
    pub account_key: String,
    pub display_name: String,
    pub path: PathBuf,
    /// `path,index` icon resource, e.g. `%SystemRoot%\system32\imageres.dll,-1043`.
    pub icon: String,
    pub version: String,
    /// Optional `https://` URL Explorer links to as the recycle bin.
    pub recycle_bin_url: Option<String>,
}
