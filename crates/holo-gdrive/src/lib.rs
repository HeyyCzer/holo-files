//! Google Drive provider — scaffold.
//!
//! The engine is already provider-agnostic, so implementing Google Drive means filling
//! in this file against Drive API v3. Notes for whoever implements it:
//!
//! - **Changes**: `changes.list` is drive-wide (`startPageToken` / `newStartPageToken`).
//!   Return `Capabilities::scoped_changes = false`; the engine filters by parent chain.
//! - **Ranged download**: `files.get?alt=media` honours `Range`.
//! - **Hash**: `md5Checksum` (also `sha256Checksum`) → `HashAlgo::Md5` / `Sha256`;
//!   add the hasher in `holo_core::hash`.
//! - **Duplicate names** are allowed in Drive: `allows_duplicate_names = true`; the
//!   engine renames collisions locally.
//! - **Google Docs/Sheets** have no binary content; skip or export them.
//! - **Upload**: resumable upload (`uploadType=resumable`) with chunks multiple of 256 KiB.
//! - **Delete**: prefer `files.update { trashed: true }` (recoverable) over `files.delete`.

use std::ops::Range;

use async_trait::async_trait;
use holo_auth::OAuthConfig;
use holo_core::{
    AccountInfo, ByteStream, Capabilities, ChangeBatch, ChangeCursor, CloudProvider, HashAlgo,
    ProviderError, ProviderKind, ProviderResult, RemoteItem, RemoteItemId, UploadRequest,
};

pub fn oauth_config(client_id: &str, client_secret: Option<&str>) -> OAuthConfig {
    OAuthConfig {
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
        token_url: "https://oauth2.googleapis.com/token".into(),
        client_id: client_id.to_owned(),
        client_secret: client_secret.map(str::to_owned),
        scopes: vec![
            "https://www.googleapis.com/auth/drive".into(),
            "openid".into(),
            "email".into(),
        ],
        extra_authorize_params: vec![
            ("access_type".into(), "offline".into()),
            ("prompt".into(), "consent".into()),
        ],
        redirect_host: "127.0.0.1".into(),
    }
}

#[derive(Default)]
pub struct GoogleDriveProvider;

const TODO: ProviderError = ProviderError::NotImplemented("Google Drive provider");

#[async_trait]
impl CloudProvider for GoogleDriveProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GoogleDrive
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            ranged_download: true,
            hash_algo: Some(HashAlgo::Md5),
            scoped_changes: false,
            allows_duplicate_names: true,
        }
    }

    async fn account(&self) -> ProviderResult<AccountInfo> {
        Err(TODO)
    }
    async fn item_by_path(&self, _path: &str) -> ProviderResult<RemoteItem> {
        Err(TODO)
    }
    async fn item(&self, _id: &RemoteItemId) -> ProviderResult<RemoteItem> {
        Err(TODO)
    }
    async fn changes(
        &self,
        _scope: &RemoteItemId,
        _cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<ChangeBatch> {
        Err(TODO)
    }
    async fn download(
        &self,
        _id: &RemoteItemId,
        _range: Option<Range<u64>>,
    ) -> ProviderResult<ByteStream> {
        Err(TODO)
    }
    async fn create_folder(
        &self,
        _parent: &RemoteItemId,
        _name: &str,
    ) -> ProviderResult<RemoteItem> {
        Err(TODO)
    }
    async fn upload(&self, _request: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
        Err(TODO)
    }
    async fn move_item(
        &self,
        _id: &RemoteItemId,
        _p: &RemoteItemId,
        _n: &str,
    ) -> ProviderResult<RemoteItem> {
        Err(TODO)
    }
    async fn delete(&self, _id: &RemoteItemId) -> ProviderResult<()> {
        Err(TODO)
    }
}
