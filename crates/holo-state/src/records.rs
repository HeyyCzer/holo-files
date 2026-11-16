use chrono::{DateTime, Utc};
use holo_core::{ContentHash, ItemKind, ProviderKind, RelPath, RemoteItem, RemoteItemId};
use rusqlite::Row;

use crate::from_ms;

#[derive(Debug, Clone)]
pub struct AccountRecord {
    /// Local account id, e.g. `onedrive-1a2b3c`.
    pub id: String,
    pub provider: ProviderKind,
    pub remote_id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl AccountRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let provider: String = r.get(1)?;
        Ok(Self {
            id: r.get(0)?,
            provider: provider.parse().map_err(|e: String| {
                rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, e.into())
            })?,
            remote_id: r.get(2)?,
            display_name: r.get(3)?,
            email: r.get(4)?,
            created_at: from_ms(r.get(5)?),
        })
    }
}

#[derive(Debug, Clone)]
pub struct RootRecord {
    pub id: String,
    pub account_id: String,
    pub local_path: String,
    pub remote_root_id: RemoteItemId,
    pub remote_path: String,
    pub enabled: bool,
    pub cursor: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl RootRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            account_id: r.get(1)?,
            local_path: r.get(2)?,
            remote_root_id: RemoteItemId(r.get(3)?),
            remote_path: r.get(4)?,
            enabled: r.get(5)?,
            cursor: r.get(6)?,
            created_at: from_ms(r.get(7)?),
        })
    }
}

/// Last-synchronized state of one item. `rel_path` is the *local* path (names may be
/// sanitized for Windows); `name` is the remote name.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRecord {
    pub root_id: String,
    pub remote_id: RemoteItemId,
    pub parent_id: Option<RemoteItemId>,
    pub name: String,
    pub rel_path: RelPath,
    pub is_dir: bool,
    pub size: u64,
    pub modified: DateTime<Utc>,
    pub etag: Option<String>,
    pub ctag: Option<String>,
    pub hash: Option<ContentHash>,
    pub synced_at: DateTime<Utc>,
}

impl ItemRecord {
    pub fn from_remote(root_id: &str, item: &RemoteItem, rel_path: RelPath) -> Self {
        Self {
            root_id: root_id.to_owned(),
            remote_id: item.id.clone(),
            parent_id: item.parent_id.clone(),
            name: item.name.clone(),
            rel_path,
            is_dir: item.is_dir(),
            size: item.size,
            modified: item.modified,
            etag: item.etag.clone(),
            ctag: item.ctag.clone(),
            hash: item.hash.clone(),
            synced_at: Utc::now(),
        }
    }

    pub fn to_remote(&self) -> RemoteItem {
        RemoteItem {
            id: self.remote_id.clone(),
            parent_id: self.parent_id.clone(),
            name: self.name.clone(),
            kind: if self.is_dir {
                ItemKind::Folder
            } else {
                ItemKind::File
            },
            size: self.size,
            modified: self.modified,
            etag: self.etag.clone(),
            ctag: self.ctag.clone(),
            hash: self.hash.clone(),
            deleted: false,
        }
    }

    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let rel: String = r.get(4)?;
        let hash: Option<String> = r.get(10)?;
        Ok(Self {
            root_id: r.get(0)?,
            remote_id: RemoteItemId(r.get(1)?),
            parent_id: r.get::<_, Option<String>>(2)?.map(RemoteItemId),
            name: r.get(3)?,
            rel_path: RelPath::parse(&rel).unwrap_or_default(),
            is_dir: r.get(5)?,
            size: r.get::<_, i64>(6)? as u64,
            modified: from_ms(r.get(7)?),
            etag: r.get(8)?,
            ctag: r.get(9)?,
            hash: hash.as_deref().and_then(ContentHash::from_storage),
            synced_at: from_ms(r.get(11)?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    Pending,
    /// Blocked by a safety guard (e.g. mass delete) until the user confirms.
    Held,
    /// Permanently failed; kept for inspection.
    Failed,
}

impl OpState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Held => "held",
            Self::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "held" => Self::Held,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpRecord {
    pub id: i64,
    pub root_id: String,
    pub kind: String,
    pub payload: String,
    pub state: OpState,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub not_before: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl OpRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let state: String = r.get(4)?;
        Ok(Self {
            id: r.get(0)?,
            root_id: r.get(1)?,
            kind: r.get(2)?,
            payload: r.get(3)?,
            state: OpState::parse(&state),
            attempts: r.get(5)?,
            last_error: r.get(6)?,
            not_before: from_ms(r.get(7)?),
            created_at: from_ms(r.get(8)?),
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OpCounts {
    pub pending: u64,
    pub held: u64,
    pub failed: u64,
}

/// What happened, for the "recent activity" list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// Local file sent to the cloud.
    Uploaded,
    /// Local folder created in the cloud.
    FolderCreated,
    /// Local move/rename applied in the cloud (`path` is the new path).
    Moved,
    /// Local deletion applied in the cloud.
    Deleted,
    /// New item in the cloud, now visible locally.
    CloudAdded,
    /// Item changed in the cloud (content, or moved when `detail` holds the old path).
    CloudChanged,
    /// Item deleted in the cloud, removed locally.
    CloudDeleted,
    /// Cloud-only file downloaded because an app opened it (`detail`: the app).
    Downloaded,
}

impl ActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::FolderCreated => "folder_created",
            Self::Moved => "moved",
            Self::Deleted => "deleted",
            Self::CloudAdded => "cloud_added",
            Self::CloudChanged => "cloud_changed",
            Self::CloudDeleted => "cloud_deleted",
            Self::Downloaded => "downloaded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "uploaded" => Self::Uploaded,
            "folder_created" => Self::FolderCreated,
            "moved" => Self::Moved,
            "deleted" => Self::Deleted,
            "cloud_added" => Self::CloudAdded,
            "cloud_changed" => Self::CloudChanged,
            "cloud_deleted" => Self::CloudDeleted,
            "downloaded" => Self::Downloaded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ActivityRecord {
    pub id: i64,
    pub root_id: String,
    pub at: DateTime<Utc>,
    pub kind: Option<ActivityKind>,
    pub path: String,
    pub detail: Option<String>,
    pub bytes: Option<u64>,
    /// Set when it failed for good.
    pub error: Option<String>,
}

impl ActivityRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let kind: String = r.get(3)?;
        Ok(Self {
            id: r.get(0)?,
            root_id: r.get(1)?,
            at: from_ms(r.get(2)?),
            kind: ActivityKind::parse(&kind),
            path: r.get(4)?,
            detail: r.get(5)?,
            bytes: r.get::<_, Option<i64>>(6)?.map(|b| b.max(0) as u64),
            error: r.get(7)?,
        })
    }
}

/// A new activity entry (see `StateStore::add_activity`).
#[derive(Debug, Clone)]
pub struct NewActivity<'a> {
    pub kind: ActivityKind,
    pub path: &'a str,
    pub detail: Option<&'a str>,
    pub bytes: Option<u64>,
    pub error: Option<&'a str>,
}
