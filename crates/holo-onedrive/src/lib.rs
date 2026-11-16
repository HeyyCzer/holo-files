//! OneDrive (personal and business) over Microsoft Graph.

mod dto;
mod graph;
mod upload;

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use holo_auth::{OAuthConfig, TokenManager};
use holo_core::{
    AccountInfo, ByteStream, Capabilities, ChangeBatch, ChangeCursor, CloudProvider, HashAlgo,
    ProviderError, ProviderKind, ProviderResult, RemoteItem, RemoteItemId, UploadRequest,
};
use reqwest::{Method, StatusCode};
use serde_json::json;

use crate::{
    dto::{DeltaPage, Drive, DriveItem, Me},
    graph::{GRAPH, GraphClient},
};

pub use graph::GraphOptions;

/// Fields requested on every item query (smaller payloads, faster delta).
const ITEM_SELECT: &str = "id,name,size,lastModifiedDateTime,eTag,cTag,parentReference,file,folder,deleted,root,package,remoteItem";

pub fn oauth_config(client_id: &str, tenant: Option<&str>) -> OAuthConfig {
    let tenant = tenant.unwrap_or("common");
    OAuthConfig {
        authorize_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
        token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
        client_id: client_id.to_owned(),
        client_secret: None,
        scopes: vec![
            "Files.ReadWrite".into(),
            "User.Read".into(),
            "offline_access".into(),
        ],
        extra_authorize_params: vec![("prompt".into(), "select_account".into())],
        redirect_host: "localhost".into(),
    }
}

pub struct OneDriveProvider {
    graph: GraphClient,
    drive: Drive,
}

impl OneDriveProvider {
    /// Connects and reads drive metadata (drive type decides whether delta can be scoped).
    pub async fn connect(tokens: Arc<TokenManager>, opts: GraphOptions) -> ProviderResult<Self> {
        let graph = GraphClient::new(tokens, opts)?;
        let drive: Drive = graph.get_json(&format!("{GRAPH}/me/drive")).await?;
        Ok(Self { graph, drive })
    }

    fn is_business(&self) -> bool {
        self.drive
            .drive_type
            .as_deref()
            .is_some_and(|t| t != "personal")
    }
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode one name for use inside a `:/{name}:` path segment.
pub(crate) fn encode_segment(name: &str) -> String {
    url::form_urlencoded::byte_serialize(name.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[async_trait]
impl CloudProvider for OneDriveProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OneDrive
    }

    fn throttle_count(&self) -> u64 {
        self.graph.throttle_count()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            ranged_download: true,
            hash_algo: Some(HashAlgo::QuickXor),
            // Graph only supports delta on non-root folders for personal drives.
            scoped_changes: !self.is_business(),
            allows_duplicate_names: false,
        }
    }

    async fn account(&self) -> ProviderResult<AccountInfo> {
        let me: Me = self
            .graph
            .get_json(&format!(
                "{GRAPH}/me?$select=id,displayName,mail,userPrincipalName"
            ))
            .await?;
        let drive: Drive = self.graph.get_json(&format!("{GRAPH}/me/drive")).await?;
        Ok(AccountInfo {
            remote_id: me.id,
            display_name: me.display_name.unwrap_or_default(),
            email: me.mail.or(me.user_principal_name),
            quota_total: drive.quota.as_ref().and_then(|q| q.total),
            quota_used: drive.quota.as_ref().and_then(|q| q.used),
        })
    }

    async fn item_by_path(&self, path: &str) -> ProviderResult<RemoteItem> {
        let p = encode_path(path);
        let url = if p.is_empty() {
            format!("{GRAPH}/me/drive/root?$select={ITEM_SELECT}")
        } else {
            format!("{GRAPH}/me/drive/root:/{p}?$select={ITEM_SELECT}")
        };
        let item: DriveItem = self.graph.get_json(&url).await?;
        item.into_remote()
            .ok_or_else(|| ProviderError::Other("unsupported item type".into()))
    }

    async fn item(&self, id: &RemoteItemId) -> ProviderResult<RemoteItem> {
        let item: DriveItem = self
            .graph
            .get_json(&format!(
                "{GRAPH}/me/drive/items/{id}?$select={ITEM_SELECT}"
            ))
            .await?;
        item.into_remote()
            .ok_or_else(|| ProviderError::Other("unsupported item type".into()))
    }

    async fn changes(
        &self,
        scope: &RemoteItemId,
        cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<ChangeBatch> {
        let url = match cursor {
            Some(c) => c.0.clone(),
            None if self.is_business() => {
                format!("{GRAPH}/me/drive/root/delta?$select={ITEM_SELECT}")
            }
            None => format!("{GRAPH}/me/drive/items/{scope}/delta?$select={ITEM_SELECT}"),
        };
        let page: DeltaPage = match self.graph.get_json(&url).await {
            Err(ProviderError::Http { status: 410, .. }) => {
                return Err(ProviderError::ResyncRequired);
            }
            other => other?,
        };
        let items = page
            .value
            .into_iter()
            .filter_map(DriveItem::into_remote)
            .collect();
        match (page.next_link, page.delta_link) {
            (Some(next), _) => Ok(ChangeBatch {
                items,
                cursor: ChangeCursor(next),
                has_more: true,
            }),
            (None, Some(delta)) => Ok(ChangeBatch {
                items,
                cursor: ChangeCursor(delta),
                has_more: false,
            }),
            (None, None) => Err(ProviderError::Other(
                "delta page without nextLink/deltaLink".into(),
            )),
        }
    }

    async fn download(
        &self,
        id: &RemoteItemId,
        range: Option<Range<u64>>,
    ) -> ProviderResult<ByteStream> {
        // /content answers 302 with a pre-authenticated URL; fetch it without auth so the
        // Range header survives and the CDN serves the bytes directly.
        let location = self
            .graph
            .redirect_location(&format!("{GRAPH}/me/drive/items/{id}/content"))
            .await?;
        let resp = self
            .graph
            .unauthenticated(Method::GET, &location, |rb| match &range {
                Some(r) if r.end > r.start => {
                    rb.header("Range", format!("bytes={}-{}", r.start, r.end - 1))
                }
                _ => rb,
            })
            .await?;
        // A 200 to a ranged request means the server ignored Range: drop the prefix so
        // callers always receive bytes starting at `range.start`.
        let mut skip = match &range {
            Some(r) if resp.status() == StatusCode::OK && r.start > 0 => {
                tracing::debug!("server ignored Range; skipping {} bytes", r.start);
                r.start
            }
            _ => 0,
        };
        let stream = resp.bytes_stream().filter_map(move |r| {
            let out = match r {
                Err(e) => Some(Err(ProviderError::Network(e.to_string()))),
                Ok(chunk) if skip >= chunk.len() as u64 => {
                    skip -= chunk.len() as u64;
                    None
                }
                Ok(chunk) => {
                    let start = std::mem::take(&mut skip) as usize;
                    Some(Ok(chunk.slice(start..)))
                }
            };
            futures::future::ready(out)
        });
        Ok(Box::pin(stream))
    }

    async fn create_folder(&self, parent: &RemoteItemId, name: &str) -> ProviderResult<RemoteItem> {
        let body =
            json!({ "name": name, "folder": {}, "@microsoft.graph.conflictBehavior": "fail" });
        let item: DriveItem = self
            .graph
            .send_json(
                Method::POST,
                &format!("{GRAPH}/me/drive/items/{parent}/children"),
                Some(&body),
                None,
            )
            .await?;
        item.into_remote()
            .ok_or_else(|| ProviderError::Other("bad create_folder response".into()))
    }

    async fn upload(&self, request: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
        upload::upload(&self.graph, request).await
    }

    async fn move_item(
        &self,
        id: &RemoteItemId,
        new_parent: &RemoteItemId,
        new_name: &str,
    ) -> ProviderResult<RemoteItem> {
        let body = json!({ "parentReference": { "id": new_parent.as_str() }, "name": new_name });
        let item: DriveItem = self
            .graph
            .send_json(
                Method::PATCH,
                &format!("{GRAPH}/me/drive/items/{id}"),
                Some(&body),
                None,
            )
            .await?;
        item.into_remote()
            .ok_or_else(|| ProviderError::Other("bad move response".into()))
    }

    async fn delete(&self, id: &RemoteItemId) -> ProviderResult<()> {
        match self
            .graph
            .send(
                Method::DELETE,
                &format!("{GRAPH}/me/drive/items/{id}"),
                None,
                None,
            )
            .await
        {
            Ok(_) | Err(ProviderError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_paths_for_graph() {
        assert_eq!(encode_path("/Docs/My File#1.txt"), "Docs/My%20File%231.txt");
        assert_eq!(encode_path(""), "");
    }
}
