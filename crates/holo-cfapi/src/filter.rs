//! Cloud Filter callbacks → [`CallbackHandler`].

use std::sync::Arc;

use cloud_filter::{
    error::{CResult, CloudErrorKind},
    filter::{Request, StateWatchEvent, SyncFilter, info, ticket},
    utility::WriteAt,
};

use crate::{
    CallbackHandler, HydrateError, HydrationRequest, HydrationSink, LocalEvent,
    aligned::{AlignedWriter, align_range},
};

/// Transfer chunk size handed to `CfExecute` (multiple of 4 KiB).
const TRANSFER_CHUNK: usize = 1 << 20;

pub struct Bridge {
    handler: Arc<dyn CallbackHandler>,
    min_fetch: u64,
}

impl Bridge {
    pub(crate) fn new(handler: Arc<dyn CallbackHandler>, min_fetch: u64) -> Self {
        Self { handler, min_fetch }
    }
}

struct Sink<W: FnMut(u64, &[u8]) -> std::io::Result<()>>(AlignedWriter<W>);

impl<W: FnMut(u64, &[u8]) -> std::io::Result<()>> HydrationSink for Sink<W> {
    fn push(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.0.push(data)
    }
}

impl SyncFilter for Bridge {
    fn fetch_data(
        &self,
        request: Request,
        ticket: ticket::FetchData,
        info: info::FetchData,
    ) -> CResult<()> {
        let file_size = request.file_size();
        let range = align_range(info.required_file_range(), file_size, self.min_fetch);
        let req = HydrationRequest {
            path: request.path(),
            identity: request.file_blob().to_vec(),
            file_size,
            range: range.clone(),
            process: request
                .process()
                .and_then(|p| p.path())
                .map(|p| p.display().to_string()),
        };
        tracing::debug!(path = %req.path.display(), start = range.start, end = range.end, process = ?req.process, "hydrate");

        let writer = AlignedWriter::new(range, file_size, TRANSFER_CHUNK, |offset, data| {
            ticket
                .write_at(data, offset)
                .map_err(std::io::Error::other)?;
            let _ = ticket.report_progress(file_size, offset + data.len() as u64);
            Ok(())
        });
        let mut sink = Sink(writer);
        let path = request.path();
        if let Err(e) = self.handler.hydrate(req, &mut sink) {
            tracing::warn!(path = %path.display(), error = %e, "hydration failed");
            return Err(e.into());
        }
        sink.0.finish().map(|_| ()).map_err(|e| {
            tracing::warn!(path = %path.display(), error = %e, "hydration incomplete");
            CloudErrorKind::Unsuccessful
        })
    }

    fn fetch_placeholders(
        &self,
        _request: Request,
        _ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> CResult<()> {
        // Population is AlwaysFull: placeholders are created by the engine ahead of time.
        Ok(())
    }

    fn dehydrate(
        &self,
        _request: Request,
        ticket: ticket::Dehydrate,
        _info: info::Dehydrate,
    ) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn delete(
        &self,
        _request: Request,
        ticket: ticket::Delete,
        _info: info::Delete,
    ) -> CResult<()> {
        // Approve instantly; the remote delete is queued after `deleted` fires.
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn deleted(&self, request: Request, _info: info::Deleted) {
        self.handler.notify(LocalEvent::Deleted {
            path: request.path(),
        });
    }

    fn rename(&self, request: Request, ticket: ticket::Rename, info: info::Rename) -> CResult<()> {
        if !info.target_in_scope() {
            // Moved out of the sync root: from the cloud's point of view it's a delete.
            self.handler.notify(LocalEvent::Deleted {
                path: request.path(),
            });
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn renamed(&self, request: Request, info: info::Renamed) {
        self.handler.notify(LocalEvent::Renamed {
            from: info.source_path(),
            to: request.path(),
        });
    }

    fn state_changed(&self, event: StateWatchEvent) {
        match event {
            StateWatchEvent::Changed(paths) => self.handler.notify(LocalEvent::Changed(paths)),
            StateWatchEvent::Stopped(e) => self
                .handler
                .notify(LocalEvent::WatcherStopped(e.to_string())),
        }
    }
}

impl From<HydrateError> for CloudErrorKind {
    fn from(e: HydrateError) -> Self {
        match e {
            HydrateError::Offline(_) => CloudErrorKind::NetworkUnavailable,
            HydrateError::Refused(_) => CloudErrorKind::AccessDenied,
            _ => CloudErrorKind::Unsuccessful,
        }
    }
}
