//! Sync root registration and connection.

use std::{io, path::Path, sync::Arc};

use cloud_filter::{
    ext::ProviderStatus,
    root::{
        Connection, HydrationPolicy, HydrationType, PopulationType, ProtectionMode, SecurityId,
        Session, SyncRootId, SyncRootIdBuilder, SyncRootInfo,
    },
};

use crate::{CallbackHandler, SyncRootSpec, filter::Bridge, placeholder::win_err};

fn root_id(spec: &SyncRootSpec) -> io::Result<SyncRootId> {
    Ok(SyncRootIdBuilder::new(&spec.provider_name)
        .user_security_id(SecurityId::current_user().map_err(win_err)?)
        .account_name(&spec.account_key)
        .build())
}

/// Provider name of the Cloud Filter sync root containing `path` (e.g. `OneDrive`),
/// or `None` if it is not inside any. Missing paths are checked via their nearest
/// existing ancestor.
pub fn owner(path: &Path) -> io::Result<Option<String>> {
    use std::os::windows::fs::OpenOptionsExt;

    use cloud_filter::ext::FileExt;

    let mut p = path;
    while !p.exists() {
        match p.parent() {
            Some(parent) => p = parent,
            None => return Ok(None),
        }
    }
    let f = std::fs::OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(p)?;
    match f.sync_root_info() {
        Ok(info) => Ok(info.map(|i| i.provider_name().to_string_lossy())),
        // The Cloud Files filter isn't attached to volumes without any sync root yet.
        Err(e) if e.code() == windows::Win32::Foundation::ERROR_INVALID_FUNCTION.to_hresult() => {
            Ok(None)
        }
        Err(e) => Err(win_err(e)),
    }
}

pub fn is_supported() -> bool {
    cloud_filter::root::is_supported().unwrap_or(false)
}

/// Register (or re-register) a sync root. The directory must exist on NTFS.
///
/// Policy choices:
/// - **Progressive hydration**: readers get bytes as they arrive, and they persist.
///   (Not `StreamingAllowed`: that streams to the reader *without* keeping data on disk,
///   so every open would re-download.)
/// - **AlwaysFull population**: the engine creates every placeholder from the remote
///   change feed, so directory listing never waits for the network.
/// - **Auto-dehydration allowed**: Storage Sense may free up space.
pub fn register(spec: &SyncRootSpec) -> io::Result<()> {
    std::fs::create_dir_all(&spec.path)?;
    let mut info = SyncRootInfo::default()
        .with_display_name(&spec.display_name)
        .with_icon(&spec.icon)
        .with_version(&spec.version)
        .with_hydration_type(HydrationType::Progressive)
        .with_hydration_policy(HydrationPolicy::AutoDehydrationAllowed)
        .with_population_type(PopulationType::AlwaysFull)
        .with_allow_pinning(true)
        .with_protection_mode(ProtectionMode::Personal)
        .with_show_siblings_as_group(false)
        .with_path(&spec.path)
        .map_err(win_err)?;
    if let Some(url) = &spec.recycle_bin_url {
        info = info.with_recycle_bin_uri(url).map_err(win_err)?;
    }
    root_id(spec)?.register(info).map_err(win_err)
}

pub fn is_registered(spec: &SyncRootSpec) -> io::Result<bool> {
    root_id(spec)?.is_registered().map_err(win_err)
}

/// Remove the registration. Fully hydrated files become regular files; dehydrated
/// placeholders may be removed by Windows (their content still lives in the cloud).
pub fn unregister(spec: &SyncRootSpec) -> io::Result<()> {
    root_id(spec)?.unregister().map_err(win_err)
}

/// Live connection; dropping it disconnects (registration and placeholders remain).
pub struct RootConnection {
    conn: Connection<Bridge>,
}

impl RootConnection {
    pub fn set_status(&self, status: ProviderState) {
        let s = match status {
            ProviderState::Idle => ProviderStatus::Idle,
            ProviderState::Syncing => ProviderStatus::SyncIncremental,
            ProviderState::Populating => ProviderStatus::PopulateNamespace,
            ProviderState::Offline => ProviderStatus::ConnectivityLost,
            ProviderState::Error => ProviderStatus::Error,
        };
        if let Err(e) = self.conn.set_status(s) {
            tracing::debug!(error = %e, "set provider status failed");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ProviderState {
    Idle,
    Syncing,
    Populating,
    Offline,
    Error,
}

/// Connect callbacks for an already registered root.
///
/// `min_fetch` = minimum bytes fetched per hydration request (read-ahead).
pub fn connect(
    path: &Path,
    handler: Arc<dyn CallbackHandler>,
    min_fetch: u64,
) -> io::Result<RootConnection> {
    let conn = Session::new()
        // Our own scans/hashing must never pull files down.
        .block_implicit_hydration()
        .connect(path, Bridge::new(handler, min_fetch))
        .map_err(win_err)?;
    Ok(RootConnection { conn })
}
