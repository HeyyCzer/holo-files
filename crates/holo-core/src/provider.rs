use std::{ops::Range, path::Path, pin::Pin};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::{
    error::ProviderResult,
    model::{
        AccountInfo, Capabilities, ChangeBatch, ChangeCursor, ProviderKind, RemoteItem,
        RemoteItemId,
    },
};

/// Streamed download body.
pub type ByteStream = Pin<Box<dyn Stream<Item = ProviderResult<Bytes>> + Send>>;

/// Where an upload goes.
#[derive(Debug, Clone, Copy)]
pub enum UploadTarget<'a> {
    /// Create a new file under `parent`. Fails with `NameConflict` if the name exists.
    New {
        parent: &'a RemoteItemId,
        name: &'a str,
    },
    /// Replace the content of an existing file. With `if_match`, fails with
    /// `PreconditionFailed` if the remote changed meanwhile.
    Replace {
        id: &'a RemoteItemId,
        if_match: Option<&'a str>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct UploadRequest<'a> {
    pub target: UploadTarget<'a>,
    /// Local file to read. Must be fully hydrated.
    pub source: &'a Path,
    pub size: u64,
}

/// A cloud storage backend.
///
/// Implementations must be cheap to share (`Arc<dyn CloudProvider>`) and handle
/// authentication, throttling and transient retries internally. Everything above
/// this trait (engine, placeholders, state) is provider-agnostic.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> Capabilities;

    async fn account(&self) -> ProviderResult<AccountInfo>;

    /// Resolve a `/`-separated path relative to the drive root (`""` or `/` = root).
    async fn item_by_path(&self, path: &str) -> ProviderResult<RemoteItem>;

    async fn item(&self, id: &RemoteItemId) -> ProviderResult<RemoteItem>;

    /// Page through changes below `scope`. `cursor == None` starts a full enumeration.
    /// Returns `ResyncRequired` when the cursor expired.
    async fn changes(
        &self,
        scope: &RemoteItemId,
        cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<ChangeBatch>;

    /// Download content. `range` is a half-open byte range; `None` = whole file.
    async fn download(
        &self,
        id: &RemoteItemId,
        range: Option<Range<u64>>,
    ) -> ProviderResult<ByteStream>;

    async fn create_folder(&self, parent: &RemoteItemId, name: &str) -> ProviderResult<RemoteItem>;

    async fn upload(&self, request: UploadRequest<'_>) -> ProviderResult<RemoteItem>;

    /// Rename and/or move. Pass the current parent / name to leave one unchanged.
    async fn move_item(
        &self,
        id: &RemoteItemId,
        new_parent: &RemoteItemId,
        new_name: &str,
    ) -> ProviderResult<RemoteItem>;

    /// Delete (to the provider's recycle bin when it has one).
    async fn delete(&self, id: &RemoteItemId) -> ProviderResult<()>;
}
