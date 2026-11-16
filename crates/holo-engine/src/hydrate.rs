//! Cloud Filter callback handler: hydration (ranged download) + event forwarding.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use futures::StreamExt;
use holo_cfapi::{CallbackHandler, HydrateError, HydrationRequest, HydrationSink, LocalEvent};
use holo_core::{ProviderError, RemoteItemId};
use serde::{Deserialize, Serialize};

use crate::context::RootContext;

/// Max silence between two chunks before a hydration is abandoned.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
/// Min interval between progress writes to the state store while bytes flow.
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// Finished hydrations kept for display.
const RECENT: usize = 8;

/// Downloads triggered by apps opening cloud-only files; published to the state store
/// (kv `hydrate/<root id>`) so the UI can show them (they aren't journal ops).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HydrateProgress {
    pub active: Vec<ActiveHydration>,
    /// Most recent first.
    pub recent: Vec<FinishedHydration>,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveHydration {
    pub id: u64,
    pub path: String,
    /// Executable name of the process that asked for the data.
    pub process: Option<String>,
    pub file_size: u64,
    /// Bytes requested in this callback, and how many arrived so far.
    pub bytes: u64,
    pub done: u64,
    pub started_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishedHydration {
    pub path: String,
    pub process: Option<String>,
    pub bytes: u64,
    pub ms: u64,
    pub error: Option<String>,
    pub finished_ms: i64,
}

pub fn hydrate_key(root_id: &str) -> String {
    format!("hydrate/{root_id}")
}

#[derive(Default)]
struct Tracker {
    next_id: u64,
    progress: HydrateProgress,
    last_publish: Option<Instant>,
}

pub struct Handler {
    ctx: Arc<RootContext>,
    rt: tokio::runtime::Handle,
    tracker: Mutex<Tracker>,
}

impl Handler {
    pub fn new(ctx: Arc<RootContext>, rt: tokio::runtime::Handle) -> Self {
        let h = Self {
            ctx,
            rt,
            tracker: Mutex::new(Tracker::default()),
        };
        // Clears whatever a previous daemon run left behind.
        h.publish(&mut h.tracker.lock().unwrap(), true);
        h
    }

    fn publish(&self, t: &mut Tracker, force: bool) {
        if !force && t.last_publish.is_some_and(|l| l.elapsed() < PUBLISH_EVERY) {
            return;
        }
        t.last_publish = Some(Instant::now());
        t.progress.updated_ms = Utc::now().timestamp_millis();
        if let Ok(json) = serde_json::to_string(&t.progress) {
            let _ = self.ctx.state.kv_set(&hydrate_key(self.ctx.id()), &json);
        }
    }

    fn start(&self, req: &HydrationRequest) -> u64 {
        let path = req
            .path
            .strip_prefix(&self.ctx.root.local_path)
            .unwrap_or(&req.path)
            .to_string_lossy()
            .replace('\\', "/");
        let process = req
            .process
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let mut t = self.tracker.lock().unwrap();
        t.next_id += 1;
        let id = t.next_id;
        t.progress.active.push(ActiveHydration {
            id,
            path,
            process,
            file_size: req.file_size,
            bytes: req.range.end - req.range.start,
            done: 0,
            started_ms: Utc::now().timestamp_millis(),
        });
        self.publish(&mut t, true);
        id
    }

    fn advance(&self, id: u64, n: u64) {
        let mut t = self.tracker.lock().unwrap();
        if let Some(a) = t.progress.active.iter_mut().find(|a| a.id == id) {
            a.done += n;
        }
        self.publish(&mut t, false);
    }

    fn finish(&self, id: u64, error: Option<String>) {
        let mut t = self.tracker.lock().unwrap();
        let Some(i) = t.progress.active.iter().position(|a| a.id == id) else {
            return;
        };
        let a = t.progress.active.remove(i);
        let now = Utc::now().timestamp_millis();
        let mut recent: VecDeque<_> = std::mem::take(&mut t.progress.recent).into();
        recent.push_front(FinishedHydration {
            path: a.path,
            process: a.process,
            bytes: a.done,
            ms: (now - a.started_ms).max(0) as u64,
            error,
            finished_ms: now,
        });
        recent.truncate(RECENT);
        t.progress.recent = recent.into();
        self.publish(&mut t, true);
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
        let track = self.start(&req);
        // Runs on a Cloud Filter worker thread (not a runtime thread), so block_on is fine.
        let result = self.rt.block_on(async {
            let mut stream = provider.download(&id, Some(range)).await.map_err(map_err)?;
            loop {
                let next = tokio::time::timeout(CHUNK_TIMEOUT, stream.next())
                    .await
                    .map_err(|_| HydrateError::Offline("download stalled".into()))?;
                match next {
                    None => break,
                    Some(chunk) => {
                        let chunk = chunk.map_err(map_err)?;
                        sink.push(&chunk)
                            .map_err(|e| HydrateError::Other(e.to_string()))?;
                        self.advance(track, chunk.len() as u64);
                    }
                }
            }
            Ok::<_, HydrateError>(())
        });
        self.finish(track, result.as_ref().err().map(|e| e.to_string()));
        result?;
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
