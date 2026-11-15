use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which cloud backend an account belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OneDrive,
    GoogleDrive,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 2] = [ProviderKind::OneDrive, ProviderKind::GoogleDrive];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneDrive => "onedrive",
            Self::GoogleDrive => "gdrive",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "onedrive" | "one-drive" => Ok(Self::OneDrive),
            "gdrive" | "google-drive" | "googledrive" => Ok(Self::GoogleDrive),
            other => Err(format!(
                "unknown provider '{other}' (expected onedrive | gdrive)"
            )),
        }
    }
}

/// Opaque, provider-assigned item identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RemoteItemId(pub String);

impl RemoteItemId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    File,
    Folder,
}

impl ItemKind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Folder)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HashAlgo {
    QuickXor,
    Sha1,
    Sha256,
    Md5,
}

impl HashAlgo {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuickXor => "quickxor",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Md5 => "md5",
        }
    }
}

impl FromStr for HashAlgo {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "quickxor" => Self::QuickXor,
            "sha1" => Self::Sha1,
            "sha256" => Self::Sha256,
            "md5" => Self::Md5,
            other => return Err(format!("unknown hash algorithm '{other}'")),
        })
    }
}

/// A content hash as reported by the provider (encoding is provider native,
/// e.g. base64 for quickXorHash, lowercase hex for md5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHash {
    pub algo: HashAlgo,
    pub value: String,
}

impl ContentHash {
    /// Serialized as `algo:value` for storage.
    pub fn to_storage(&self) -> String {
        format!("{}:{}", self.algo.as_str(), self.value)
    }

    pub fn from_storage(s: &str) -> Option<Self> {
        let (algo, value) = s.split_once(':')?;
        Some(Self {
            algo: algo.parse().ok()?,
            value: value.to_owned(),
        })
    }
}

/// Snapshot of a remote item. `deleted == true` items come from change feeds and
/// only carry `id` (other fields may be empty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteItem {
    pub id: RemoteItemId,
    pub parent_id: Option<RemoteItemId>,
    pub name: String,
    pub kind: ItemKind,
    pub size: u64,
    pub modified: DateTime<Utc>,
    /// Changes on any modification (metadata or content).
    pub etag: Option<String>,
    /// Changes only when content changes (OneDrive cTag). Providers without one leave it `None`.
    pub ctag: Option<String>,
    pub hash: Option<ContentHash>,
    pub deleted: bool,
}

impl RemoteItem {
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }

    /// Whether `self` has different *content* than `other` (ignoring renames / moves).
    pub fn content_differs(&self, other: &RemoteItem) -> bool {
        if self.kind != other.kind {
            return true;
        }
        if self.is_dir() {
            return false;
        }
        match (&self.hash, &other.hash) {
            (Some(a), Some(b)) if a.algo == b.algo => return a.value != b.value,
            _ => {}
        }
        match (&self.ctag, &other.ctag) {
            (Some(a), Some(b)) => a != b,
            _ => self.size != other.size || self.modified != other.modified,
        }
    }
}

/// Opaque position in a provider's change feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCursor(pub String);

/// One page of changes. When `has_more` is false, `cursor` points at the "latest"
/// position and should be persisted; the next call returns only newer changes.
#[derive(Debug, Clone)]
pub struct ChangeBatch {
    pub items: Vec<RemoteItem>,
    pub cursor: ChangeCursor,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    /// Stable provider-side account identifier.
    pub remote_id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub quota_total: Option<u64>,
    pub quota_used: Option<u64>,
}

/// What a provider can do; the engine adapts to it instead of special-casing providers.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// HTTP-range style partial download, enabling partial hydration.
    pub ranged_download: bool,
    /// The hash algorithm the provider reports for files, if any.
    pub hash_algo: Option<HashAlgo>,
    /// Whether `changes()` can be scoped to a folder (OneDrive) or is drive-wide (Google).
    pub scoped_changes: bool,
    /// Whether two siblings may share a name (Google Drive allows it, Windows does not).
    pub allows_duplicate_names: bool,
}
