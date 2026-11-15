//! Graph JSON shapes (only the fields we use) and conversion into core types.

use chrono::{DateTime, Utc};
use holo_core::{ContentHash, HashAlgo, ItemKind, RemoteItem, RemoteItemId};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub id: String,
    pub display_name: Option<String>,
    pub mail: Option<String>,
    pub user_principal_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    pub drive_type: Option<String>,
    pub quota: Option<Quota>,
}

#[derive(Debug, Deserialize)]
pub struct Quota {
    pub total: Option<u64>,
    pub used: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaPage {
    pub value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub delta_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    pub id: String,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub last_modified_date_time: Option<DateTime<Utc>>,
    pub e_tag: Option<String>,
    pub c_tag: Option<String>,
    pub parent_reference: Option<ParentReference>,
    pub file: Option<FileFacet>,
    pub folder: Option<serde_json::Value>,
    pub deleted: Option<serde_json::Value>,
    pub root: Option<serde_json::Value>,
    /// OneNote notebooks and similar: not representable as plain files.
    pub package: Option<serde_json::Value>,
    /// Items shared from another drive ("Add to my OneDrive"): out of scope for now.
    pub remote_item: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ParentReference {
    pub id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileFacet {
    pub hashes: Option<Hashes>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hashes {
    pub quick_xor_hash: Option<String>,
    pub sha256_hash: Option<String>,
    pub sha1_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadSession {
    #[serde(rename = "uploadUrl")]
    pub upload_url: String,
}

impl DriveItem {
    /// `None` for item types we deliberately skip (packages, remote/shared items).
    pub fn into_remote(self) -> Option<RemoteItem> {
        if self.package.is_some() || self.remote_item.is_some() {
            return None;
        }
        let deleted = self.deleted.is_some();
        let kind = if self.folder.is_some() || self.root.is_some() {
            ItemKind::Folder
        } else {
            ItemKind::File
        };
        let hash = self.file.and_then(|f| f.hashes).and_then(|h| {
            h.quick_xor_hash
                .map(|v| ContentHash {
                    algo: HashAlgo::QuickXor,
                    value: v,
                })
                .or(h.sha256_hash.map(|v| ContentHash {
                    algo: HashAlgo::Sha256,
                    value: v,
                }))
                .or(h.sha1_hash.map(|v| ContentHash {
                    algo: HashAlgo::Sha1,
                    value: v,
                }))
        });
        Some(RemoteItem {
            id: RemoteItemId(self.id),
            parent_id: self.parent_reference.and_then(|p| p.id).map(RemoteItemId),
            name: self.name.unwrap_or_default(),
            kind,
            size: self.size.unwrap_or(0),
            modified: self.last_modified_date_time.unwrap_or_default(),
            etag: self.e_tag,
            ctag: self.c_tag,
            hash,
            deleted,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct GraphErrorBody {
    pub error: GraphError,
}

#[derive(Debug, Deserialize)]
pub struct GraphError {
    pub code: String,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delta_page() {
        let json = r#"{
          "value": [
            {"id":"R","name":"root","root":{},"folder":{"childCount":1}},
            {"id":"F","name":"a.txt","size":5,"lastModifiedDateTime":"2026-01-02T03:04:05Z",
             "eTag":"e1","cTag":"c1","parentReference":{"id":"R"},
             "file":{"hashes":{"quickXorHash":"q=="}}},
            {"id":"D","deleted":{"state":"deleted"},"parentReference":{"id":"R"}},
            {"id":"N","name":"Notebook","package":{"type":"oneNote"}}
          ],
          "@odata.deltaLink":"https://graph/delta?token=x"
        }"#;
        let page: DeltaPage = serde_json::from_str(json).unwrap();
        let items: Vec<_> = page
            .value
            .into_iter()
            .filter_map(DriveItem::into_remote)
            .collect();
        assert_eq!(items.len(), 3);
        assert!(items[0].is_dir());
        assert_eq!(items[1].hash.as_ref().unwrap().value, "q==");
        assert_eq!(items[1].parent_id.as_ref().unwrap().as_str(), "R");
        assert!(items[2].deleted);
        assert_eq!(
            page.delta_link.as_deref(),
            Some("https://graph/delta?token=x")
        );
    }
}
