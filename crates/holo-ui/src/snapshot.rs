//! What the tray and the window show, read from the daemon's state store (the same
//! data `holo status --stream` uses). Never talks to the daemon directly.

use holo_cli::cmd::{Ctx, daemon_running, pull_line};
use holo_engine::{
    context::is_paused,
    ops::{ExecProgress, LocalOp, progress_key},
    remote::{PullPhase, PullProgress, pull_key},
};
use holo_state::{OpCounts, OpRecord, RootRecord};

/// Overall state, worst first: drives the tray icon colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The daemon isn't running.
    Stopped,
    /// Ops failed for good or are held for confirmation.
    Attention,
    Paused,
    /// Uploading, listing or applying cloud changes.
    Syncing,
    UpToDate,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Daemon parado",
            Self::Attention => "Precisa de atenção",
            Self::Paused => "Pausado",
            Self::Syncing => "Sincronizando",
            Self::UpToDate => "Tudo sincronizado",
        }
    }
}

pub struct RootView {
    pub root: RootRecord,
    pub items: u64,
    pub counts: OpCounts,
    pub progress: ExecProgress,
    /// Current cloud pull, if one is running (`None` when idle).
    pub pull: Option<String>,
    pub pulling: bool,
}

pub struct Snapshot {
    pub daemon: bool,
    pub paused: bool,
    pub roots: Vec<RootView>,
}

impl Snapshot {
    pub fn load(ctx: &Ctx) -> anyhow::Result<Self> {
        let daemon = daemon_running(ctx);
        let mut roots = Vec::new();
        for root in ctx.state.roots()? {
            let progress: ExecProgress = ctx
                .state
                .kv_get(&progress_key(&root.id))?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();
            let pulling = ctx
                .state
                .kv_get(&pull_key(&root.id))?
                .and_then(|j| serde_json::from_str::<PullProgress>(&j).ok())
                .is_some_and(|p| p.phase != PullPhase::Idle);
            roots.push(RootView {
                items: ctx.state.item_count(&root.id)?,
                counts: ctx.state.op_counts(&root.id)?,
                pull: if daemon {
                    pull_line(ctx, &root.id)?
                } else {
                    None
                },
                pulling: daemon && pulling,
                progress,
                root,
            });
        }
        Ok(Self {
            daemon,
            paused: is_paused(&ctx.state),
            roots,
        })
    }

    pub fn totals(&self) -> OpCounts {
        let mut t = OpCounts::default();
        for r in &self.roots {
            t.pending += r.counts.pending;
            t.held += r.counts.held;
            t.failed += r.counts.failed;
        }
        t
    }

    pub fn running(&self) -> usize {
        self.roots.iter().map(|r| r.progress.running.len()).sum()
    }

    pub fn status(&self) -> Status {
        let t = self.totals();
        if !self.daemon {
            Status::Stopped
        } else if t.failed > 0 || t.held > 0 {
            Status::Attention
        } else if self.paused {
            Status::Paused
        } else if self.running() > 0 || self.roots.iter().any(|r| r.pulling) {
            Status::Syncing
        } else {
            Status::UpToDate
        }
    }

    /// One-line summary for the tray tooltip (Windows caps it at 127 chars).
    pub fn tooltip(&self) -> String {
        let t = self.totals();
        let mut s = format!("holo · {}", self.status().label());
        let running = self.running();
        if self.daemon && running > 0 {
            s.push_str(&format!(" · {running} enviando"));
        }
        if t.pending > 0 {
            s.push_str(&format!(" · {} na fila", t.pending));
        }
        if t.held > 0 {
            s.push_str(&format!(" · {} retidas", t.held));
        }
        if t.failed > 0 {
            s.push_str(&format!(" · {} falhas", t.failed));
        }
        s.chars().take(127).collect()
    }
}

/// The path an op acts on, for display.
pub fn op_target(o: &OpRecord) -> String {
    match serde_json::from_str::<LocalOp>(&o.payload) {
        Ok(LocalOp::Upload { path } | LocalOp::CreateFolder { path }) => path.to_string(),
        Ok(LocalOp::Move { to, .. }) => format!("→ {to}"),
        Ok(LocalOp::Delete { path, .. }) => path.to_string(),
        Err(_) => o.payload.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(daemon: bool, paused: bool, counts: OpCounts, running: usize) -> Snapshot {
        let mut progress = ExecProgress::default();
        for i in 0..running {
            progress.running.push(holo_engine::ops::RunningOp {
                id: i as i64,
                kind: "upload".into(),
                path: "a".into(),
                started_ms: 0,
            });
        }
        Snapshot {
            daemon,
            paused,
            roots: vec![RootView {
                root: RootRecord {
                    id: "r".into(),
                    account_id: "a".into(),
                    local_path: r"C:\x".into(),
                    remote_path: "/".into(),
                    remote_root_id: holo_core::RemoteItemId::new("R"),
                    enabled: true,
                    cursor: None,
                    created_at: chrono::Utc::now(),
                },
                items: 0,
                counts,
                progress,
                pull: None,
                pulling: false,
            }],
        }
    }

    fn counts(pending: u64, held: u64, failed: u64) -> OpCounts {
        OpCounts {
            pending,
            held,
            failed,
        }
    }

    #[test]
    fn status_priorities() {
        assert_eq!(
            snap(false, false, counts(0, 0, 0), 0).status(),
            Status::Stopped
        );
        assert_eq!(
            snap(true, true, counts(0, 0, 1), 0).status(),
            Status::Attention
        );
        assert_eq!(
            snap(true, true, counts(5, 0, 0), 2).status(),
            Status::Paused
        );
        assert_eq!(
            snap(true, false, counts(5, 0, 0), 2).status(),
            Status::Syncing
        );
        assert_eq!(
            snap(true, false, counts(3, 0, 0), 0).status(),
            Status::UpToDate
        );
    }

    #[test]
    fn tooltip_summarises_queue() {
        let s = snap(true, false, counts(12, 0, 1), 3);
        assert_eq!(
            s.tooltip(),
            "holo · Precisa de atenção · 3 enviando · 12 na fila · 1 falhas"
        );
    }
}
