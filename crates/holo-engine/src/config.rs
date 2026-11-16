use std::{path::PathBuf, time::Duration};

use crate::adaptive::Concurrency;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// How often to poll the remote change feed.
    pub poll_interval: Duration,
    /// Quiet period before a changed local file is processed.
    pub debounce: Duration,
    /// A file modified more recently than this is considered "still being written".
    pub settle_time: Duration,
    /// Minimum bytes fetched per hydration request (read-ahead).
    pub min_fetch: u64,
    /// More local deletes than this within one minute are held for confirmation.
    pub mass_delete_threshold: usize,
    /// Periodic full local scan (safety net for lost notifications).
    pub full_scan_interval: Duration,
    /// Used in conflict-copy names.
    pub machine_name: String,
    pub use_default_ignores: bool,
    pub ignore: Vec<String>,
    /// Executable names (case-insensitive) never allowed to trigger hydration,
    /// e.g. the search indexer, to avoid hydration storms.
    pub blocked_hydration_processes: Vec<String>,
    /// Where locally deleted content is preserved (outside any sync root).
    pub trash_dir: PathBuf,
    /// Attempts before an op is marked failed.
    pub max_op_attempts: u32,
    /// Journal ops executed at the same time (uploads, folder creation).
    pub concurrency: Concurrency,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            debounce: Duration::from_millis(1500),
            settle_time: Duration::from_secs(2),
            min_fetch: 4 * 1024 * 1024,
            mass_delete_threshold: 200,
            full_scan_interval: Duration::from_secs(15 * 60),
            machine_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into()),
            use_default_ignores: true,
            ignore: Vec::new(),
            blocked_hydration_processes: vec![
                "SearchProtocolHost.exe".into(),
                "SearchFilterHost.exe".into(),
                "SearchIndexer.exe".into(),
            ],
            trash_dir: std::env::temp_dir().join("holo-trash"),
            max_op_attempts: 8,
            concurrency: Concurrency::default(),
        }
    }
}
