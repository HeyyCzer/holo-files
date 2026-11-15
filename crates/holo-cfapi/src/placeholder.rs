//! Placeholder operations used by the engine. All functions are blocking and
//! should be called from `spawn_blocking` in async code.

use std::{
    fs::{self, OpenOptions},
    io,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use cloud_filter::{
    ext::FileExt,
    metadata::Metadata,
    placeholder::{self, ConvertOptions, PinOptions, Placeholder, UpdateOptions},
    placeholder_create_info::PlaceholderCreateInfo,
};
use nt_time::FileTime;

use crate::{LocalState, PinState, PlaceholderSpec, PlaceholderState};

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Map a Windows error to `io::Error`, keeping the Win32 code when there is one
/// (so `ErrorKind::AlreadyExists`, `NotFound`, … work).
pub fn win_err(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

fn filetime(t: DateTime<Utc>) -> FileTime {
    FileTime::try_from(SystemTime::from(t)).unwrap_or(FileTime::NT_TIME_EPOCH)
}

fn metadata(spec: &PlaceholderSpec) -> Metadata {
    let t = filetime(spec.modified);
    let base = if spec.is_dir {
        Metadata::directory()
    } else {
        Metadata::file().size(spec.size)
    };
    base.created(t).written(t).changed(t).accessed(t)
}

/// Open a handle that never triggers hydration (attribute-only access).
fn open_meta(path: &Path) -> io::Result<Placeholder> {
    Ok(OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .into())
}

/// CfApi oplock handle with write access. Placeholder updates/conversions need it
/// **exclusive** (the platform rejects shared handles with ERROR_INVALID_HANDLE);
/// pin changes work with a shared one.
fn open_write(path: &Path, exclusive: bool) -> io::Result<Placeholder> {
    let mut o = Placeholder::options().write_access();
    if exclusive {
        o = o.exclusive();
    }
    o.open(path).map_err(win_err)
}

/// Create a dehydrated, in-sync placeholder `parent/name`.
pub fn create(parent: &Path, name: &str, spec: &PlaceholderSpec) -> io::Result<()> {
    let mut info = PlaceholderCreateInfo::new(name)
        .metadata(metadata(spec))
        .blob(spec.identity.clone())
        .mark_in_sync();
    if spec.is_dir {
        info = info.has_no_children();
    }
    info.create(parent).map(|_| ()).map_err(win_err)
}

/// Current state of `path`, `None` if it does not exist.
pub fn state(path: &Path) -> io::Result<Option<LocalState>> {
    let md = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let attrs = md.file_attributes();
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    let size = if is_dir { 0 } else { md.file_size() };
    let modified = md.modified().map(DateTime::<Utc>::from).unwrap_or_default();

    // Every placeholder is a reparse point; skip the CfApi query for plain files
    // (faster scans, and works outside sync roots).
    let info = if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        None
    } else {
        open_meta(path)?.info().map_err(win_err)?
    };
    let placeholder = info.map(|i| {
        let not_local =
            attrs & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_OFFLINE) != 0;
        PlaceholderState {
            in_sync: i.is_in_sync(),
            pin: map_pin(i.pin_state()),
            partial: !is_dir && (not_local || (i.on_disk_data_size().max(0) as u64) < size),
            identity: i.blob().to_vec(),
        }
    });
    Ok(Some(LocalState {
        is_dir,
        size,
        modified,
        placeholder,
    }))
}

/// Update metadata + identity and mark in sync. With `dehydrate`, local content is
/// discarded so the next read fetches the new remote version.
///
/// Pinned files cannot be dehydrated; they are temporarily unpinned, updated and
/// re-pinned. Returns `true` if the file was pinned (caller should re-hydrate it).
pub fn update(path: &Path, spec: &PlaceholderSpec, dehydrate: bool) -> io::Result<bool> {
    let pinned = dehydrate
        && !spec.is_dir
        && state(path)?
            .and_then(|s| s.placeholder)
            .is_some_and(|p| p.pin == PinState::Pinned);
    if pinned {
        set_pin(path, PinState::Unspecified)?;
    }
    let result = (|| {
        let mut ph = open_write(path, true)?;
        let mut opts = UpdateOptions::default()
            .metadata(metadata(spec))
            .blob(&spec.identity)
            .mark_in_sync();
        if dehydrate && !spec.is_dir {
            opts = opts.dehydrate();
        }
        ph.update(opts, None).map(|_| ()).map_err(win_err)
    })();
    if pinned {
        set_pin(path, PinState::Pinned)?;
    }
    result.map(|_| pinned)
}

/// Read the file's current USN (change journal position), used as an optimistic lock
/// between "start upload" and "mark in sync".
pub fn usn(path: &Path) -> io::Result<i64> {
    let f = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    f.usn().map_err(win_err)
}

/// After a successful upload: attach the identity and mark in sync. Regular files are
/// converted to placeholders. With `expected_usn`, fails (`ErrorKind::Other`) if the
/// file changed since that USN, so a racing edit is never marked as synced.
pub fn mark_synced(path: &Path, identity: &[u8], expected_usn: Option<i64>) -> io::Result<()> {
    // Query through a plain handle: CfGetPlaceholderInfo rejects CfApi oplock handles.
    let is_placeholder = open_meta(path)?.info().map_err(win_err)?.is_some();
    let mut ph = open_write(path, true)?;
    if is_placeholder {
        let mut usn = expected_usn.unwrap_or(0);
        ph.update(
            UpdateOptions::default().blob(identity).mark_in_sync(),
            Some(&mut usn),
        )
        .map_err(win_err)?;
    } else {
        if let Some(expected) = expected_usn
            && usn(path)? != expected
        {
            return Err(io::Error::other("file changed during upload"));
        }
        ph.convert_to_placeholder(
            ConvertOptions::default()
                .mark_in_sync()
                .blob(identity.to_vec()),
            None,
        )
        .map_err(win_err)?;
    }
    Ok(())
}

pub fn mark_not_in_sync(path: &Path) -> io::Result<()> {
    open_write(path, true)?
        .mark_in_sync(false, None)
        .map(|_| ())
        .map_err(win_err)
}

/// Forget the remote identity: the engine will treat the file as a brand-new local
/// file (used for conflict copies and files whose remote counterpart was deleted).
pub fn detach(path: &Path) -> io::Result<()> {
    if open_meta(path)?.info().map_err(win_err)?.is_none() {
        return Ok(());
    }
    open_write(path, true)?
        .update(
            UpdateOptions::default().remove_blob().mark_not_in_sync(),
            None,
        )
        .map(|_| ())
        .map_err(win_err)
}

pub fn set_pin(path: &Path, pin: PinState) -> io::Result<()> {
    let state = match pin {
        PinState::Unspecified => placeholder::PinState::Unspecified,
        PinState::Pinned => placeholder::PinState::Pinned,
        PinState::Unpinned => placeholder::PinState::Unpinned,
        PinState::Excluded => placeholder::PinState::Excluded,
        PinState::Inherit => placeholder::PinState::Inherit,
    };
    let mut opts = PinOptions::default();
    if path.is_dir() {
        opts.recurse();
    }
    open_write(path, false)?
        .mark_pin(state, opts)
        .map(|_| ())
        .map_err(win_err)
}

/// Download full content now (blocks until our own `fetch_data` callback finishes).
pub fn hydrate(path: &Path) -> io::Result<()> {
    Placeholder::open(path)
        .map_err(win_err)?
        .hydrate(..)
        .map_err(win_err)
}

/// Release local content of an in-sync file.
pub fn dehydrate(path: &Path) -> io::Result<()> {
    open_write(path, true)?
        .update(
            UpdateOptions::default().dehydrate().update_if_in_sync(),
            None,
        )
        .map(|_| ())
        .map_err(win_err)
}

fn map_pin(p: placeholder::PinState) -> PinState {
    match p {
        placeholder::PinState::Pinned => PinState::Pinned,
        placeholder::PinState::Unpinned => PinState::Unpinned,
        placeholder::PinState::Excluded => PinState::Excluded,
        placeholder::PinState::Inherit => PinState::Inherit,
        _ => PinState::Unspecified,
    }
}
