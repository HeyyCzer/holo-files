//! Filesystem abstraction. Production uses Cloud Filter placeholders; tests use an
//! in-memory tree.

use std::{io, path::Path};

use holo_cfapi::{LocalState, PinState, PlaceholderSpec};
use holo_core::{ContentHash, HashAlgo};

/// Blocking filesystem operations the engine needs. Call from `spawn_blocking`.
pub trait LocalTree: Send + Sync + 'static {
    fn state(&self, path: &Path) -> io::Result<Option<LocalState>>;
    /// Create a dehydrated, in-sync placeholder `parent/name`.
    fn create_placeholder(
        &self,
        parent: &Path,
        name: &str,
        spec: &PlaceholderSpec,
    ) -> io::Result<()>;
    /// Returns `true` if the file was pinned and should be re-hydrated.
    fn update_placeholder(
        &self,
        path: &Path,
        spec: &PlaceholderSpec,
        dehydrate: bool,
    ) -> io::Result<bool>;
    fn mark_synced(
        &self,
        path: &Path,
        identity: &[u8],
        expected_usn: Option<i64>,
    ) -> io::Result<()>;
    /// Strip the remote identity so the item is treated as a new local item.
    fn detach(&self, path: &Path) -> io::Result<()>;
    fn usn(&self, path: &Path) -> io::Result<Option<i64>>;
    fn set_pin(&self, path: &Path, pin: PinState) -> io::Result<()>;
    fn hydrate(&self, path: &Path) -> io::Result<()>;
    fn dehydrate(&self, path: &Path) -> io::Result<()>;
    /// Hash a fully local file. Must never hydrate.
    fn hash(&self, path: &Path, algo: HashAlgo) -> io::Result<Option<ContentHash>>;

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Remove an empty directory.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
    /// Names of direct children.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<String>>;
    /// Move content to the holo trash (outside the sync root) instead of deleting it.
    fn trash(&self, path: &Path, trash_dir: &Path) -> io::Result<()>;
}

#[cfg(windows)]
pub use cf::CfLocalTree;

#[cfg(windows)]
mod cf {
    use super::*;
    use holo_cfapi::placeholder as ph;

    /// Real implementation over Cloud Filter placeholders.
    pub struct CfLocalTree;

    impl LocalTree for CfLocalTree {
        fn state(&self, path: &Path) -> io::Result<Option<LocalState>> {
            ph::state(path)
        }
        fn create_placeholder(
            &self,
            parent: &Path,
            name: &str,
            spec: &PlaceholderSpec,
        ) -> io::Result<()> {
            ph::create(parent, name, spec)
        }
        fn update_placeholder(
            &self,
            path: &Path,
            spec: &PlaceholderSpec,
            dehydrate: bool,
        ) -> io::Result<bool> {
            ph::update(path, spec, dehydrate)
        }
        fn mark_synced(
            &self,
            path: &Path,
            identity: &[u8],
            expected_usn: Option<i64>,
        ) -> io::Result<()> {
            ph::mark_synced(path, identity, expected_usn)
        }
        fn detach(&self, path: &Path) -> io::Result<()> {
            ph::detach(path)
        }
        fn usn(&self, path: &Path) -> io::Result<Option<i64>> {
            ph::usn(path).map(Some)
        }
        fn set_pin(&self, path: &Path, pin: PinState) -> io::Result<()> {
            ph::set_pin(path, pin)
        }
        fn hydrate(&self, path: &Path) -> io::Result<()> {
            ph::hydrate(path)
        }
        fn dehydrate(&self, path: &Path) -> io::Result<()> {
            ph::dehydrate(path)
        }
        fn hash(&self, path: &Path, algo: HashAlgo) -> io::Result<Option<ContentHash>> {
            match ph::state(path)? {
                Some(s) if s.placeholder.as_ref().is_some_and(|p| p.partial) => Ok(None),
                Some(_) => holo_core::hash::hash_file(path, algo),
                None => Ok(None),
            }
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            std::fs::rename(from, to)
        }
        fn remove_file(&self, path: &Path) -> io::Result<()> {
            std::fs::remove_file(path)
        }
        fn remove_dir(&self, path: &Path) -> io::Result<()> {
            std::fs::remove_dir(path)
        }
        fn read_dir(&self, path: &Path) -> io::Result<Vec<String>> {
            std::fs::read_dir(path)?
                .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect()
        }
        fn trash(&self, path: &Path, trash_dir: &Path) -> io::Result<()> {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            std::fs::create_dir_all(trash_dir)?;
            let target = trash_dir.join(format!("{stamp} {name}"));
            // rename fails across volumes; fall back to copy + delete.
            if std::fs::rename(path, &target).is_err() {
                std::fs::copy(path, &target)?;
                std::fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}
