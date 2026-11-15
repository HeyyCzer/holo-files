//! Simple (≤ threshold) and resumable session uploads.

use bytes::Bytes;
use holo_core::{ProviderError, ProviderResult, RemoteItem, UploadRequest, UploadTarget};
use reqwest::Method;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{
    dto::{DriveItem, UploadSession},
    encode_segment,
    graph::{GRAPH, GraphClient, error_from},
};

/// Graph requires session chunks to be multiples of 320 KiB, at most 60 MiB.
const CHUNK_UNIT: u64 = 320 * 1024;
const CHUNK_MAX: u64 = 60 * 1024 * 1024;

fn io(e: std::io::Error) -> ProviderError {
    ProviderError::Io(e)
}

fn to_item(item: DriveItem) -> ProviderResult<RemoteItem> {
    item.into_remote()
        .ok_or_else(|| ProviderError::Other("unexpected upload response".into()))
}

pub async fn upload(graph: &GraphClient, req: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
    if req.size <= graph.opts.simple_upload_max {
        simple(graph, req).await
    } else {
        session(graph, req).await
    }
}

async fn simple(graph: &GraphClient, req: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
    let data = Bytes::from(tokio::fs::read(req.source).await.map_err(io)?);
    let (url, if_match) = match req.target {
        UploadTarget::New { parent, name } => (
            format!(
                "{GRAPH}/me/drive/items/{parent}:/{}:/content?@microsoft.graph.conflictBehavior=fail",
                encode_segment(name)
            ),
            None,
        ),
        UploadTarget::Replace { id, if_match } => {
            (format!("{GRAPH}/me/drive/items/{id}/content"), if_match)
        }
    };
    let resp = graph.put_bytes(&url, data, if_match).await?;
    to_item(
        resp.json()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?,
    )
}

async fn session(graph: &GraphClient, req: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
    let (url, body, if_match) = match req.target {
        UploadTarget::New { parent, name } => (
            format!(
                "{GRAPH}/me/drive/items/{parent}:/{}:/createUploadSession",
                encode_segment(name)
            ),
            json!({ "item": { "@microsoft.graph.conflictBehavior": "fail" } }),
            None,
        ),
        UploadTarget::Replace { id, if_match } => (
            format!("{GRAPH}/me/drive/items/{id}/createUploadSession"),
            json!({ "item": { "@microsoft.graph.conflictBehavior": "replace" } }),
            if_match,
        ),
    };
    let session: UploadSession = graph
        .send_json(Method::POST, &url, Some(&body), if_match)
        .await?;
    let result = upload_chunks(graph, &session.upload_url, req).await;
    if result.is_err() {
        // Best effort: release the server-side session.
        let _ = graph
            .unauthenticated(Method::DELETE, &session.upload_url, |rb| rb)
            .await;
    }
    result
}

async fn upload_chunks(
    graph: &GraphClient,
    upload_url: &str,
    req: UploadRequest<'_>,
) -> ProviderResult<RemoteItem> {
    let chunk = (graph.opts.chunk_size.clamp(CHUNK_UNIT, CHUNK_MAX) / CHUNK_UNIT) * CHUNK_UNIT;
    let total = req.size;
    let mut file = tokio::fs::File::open(req.source).await.map_err(io)?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; chunk as usize];

    loop {
        let len = chunk.min(total - offset) as usize;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io)?;
        file.read_exact(&mut buf[..len]).await.map_err(io)?;
        let data = Bytes::copy_from_slice(&buf[..len]);
        let end = offset + len as u64 - 1;
        tracing::debug!(offset, end, total, "upload chunk");

        let resp = graph
            .unauthenticated(Method::PUT, upload_url, |rb| {
                rb.header("Content-Range", format!("bytes {offset}-{end}/{total}"))
                    .header("Content-Length", len.to_string())
                    .body(data.clone())
            })
            .await;

        match resp {
            Ok(r) if r.status().as_u16() == 200 || r.status().as_u16() == 201 => {
                return to_item(
                    r.json()
                        .await
                        .map_err(|e| ProviderError::Network(e.to_string()))?,
                );
            }
            Ok(r) if r.status().as_u16() == 202 => {
                offset = next_expected(r).await.unwrap_or(end + 1);
            }
            Ok(r) => return Err(error_from(r).await),
            // Chunk failed after retries: ask the session where to resume from.
            Err(e) if e.is_transient() => {
                let status = graph
                    .unauthenticated(Method::GET, upload_url, |rb| rb)
                    .await?;
                offset = next_expected(status).await.ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
        if offset >= total {
            return Err(ProviderError::Other(
                "upload session finished without item".into(),
            ));
        }
    }
}

async fn next_expected(resp: reqwest::Response) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Status {
        #[serde(rename = "nextExpectedRanges")]
        next_expected_ranges: Vec<String>,
    }
    let s: Status = resp.json().await.ok()?;
    s.next_expected_ranges
        .first()?
        .split('-')
        .next()?
        .parse()
        .ok()
}
