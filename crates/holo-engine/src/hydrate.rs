//! Cloud Filter callback handler: hydration (ranged download) + event forwarding.

use std::{path::Path, sync::Arc, time::Duration};

use futures::StreamExt;
use holo_cfapi::{CallbackHandler, HydrateError, HydrationRequest, HydrationSink, LocalEvent};
use holo_core::{ProviderError, RemoteItemId};

use crate::context::RootContext;

/// Max silence between two chunks before a hydration is abandoned.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Handler {
    ctx: Arc<RootContext>,
    rt: tokio::runtime::Handle,
}

impl Handler {
    pub fn new(ctx: Arc<RootContext>, rt: tokio::runtime::Handle) -> Self {
        Self { ctx, rt }
    }

    fn blocked(&self, process: Option<&str>) -> bool {
        let Some(exe) = process
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
        else {
            return false;
        };
        self.ctx
            .cfg
            .blocked_hydration_processes
            .iter()
            .any(|b| b.eq_ignore_ascii_case(exe))
    }
}

fn map_err(e: ProviderError) -> HydrateError {
    match e {
        ProviderError::NotFound => HydrateError::NotFound,
        e if e.is_transient() => HydrateError::Offline(e.to_string()),
        e => HydrateError::Other(e.to_string()),
    }
}

impl CallbackHandler for Handler {
    fn hydrate(
        &self,
        req: HydrationRequest,
        sink: &mut dyn HydrationSink,
    ) -> Result<(), HydrateError> {
        if self.blocked(req.process.as_deref()) {
            return Err(HydrateError::Refused(format!(
                "{} may not hydrate files",
                req.process.unwrap_or_default()
            )));
        }
        let id = std::str::from_utf8(&req.identity)
            .ok()
            .filter(|s| !s.is_empty())
            .map(RemoteItemId::new)
            .ok_or(HydrateError::NotFound)?;
        if req.range.is_empty() {
            return Ok(());
        }
        let provider = self.ctx.provider.clone();
        let started = std::time::Instant::now();
        let bytes = req.range.end - req.range.start;
        let range = req.range.clone();
        // Runs on a Cloud Filter worker thread (not a runtime thread), so block_on is fine.
        self.rt.block_on(async move {
            let mut stream = provider.download(&id, Some(range)).await.map_err(map_err)?;
            loop {
                let next = tokio::time::timeout(CHUNK_TIMEOUT, stream.next())
                    .await
                    .map_err(|_| HydrateError::Offline("download stalled".into()))?;
                match next {
                    None => break,
                    Some(chunk) => sink
                        .push(&chunk.map_err(map_err)?)
                        .map_err(|e| HydrateError::Other(e.to_string()))?,
                }
            }
            Ok::<_, HydrateError>(())
        })?;
        tracing::info!(
            path = %req.path.display(),
            bytes,
            ms = started.elapsed().as_millis() as u64,
            "hydrated"
        );
        Ok(())
    }

    fn notify(&self, event: LocalEvent) {
        let _ = self.ctx.events.send(event);
    }
}
