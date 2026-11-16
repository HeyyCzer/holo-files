pub mod accounts;
pub mod live;
pub mod misc;
pub mod plan;
pub mod roots;
pub mod run;
pub mod service;

use std::sync::Arc;

use anyhow::{Context as _, bail};
use holo_state::{AccountRecord, RootRecord, StateStore};

use crate::config::{Config, Paths};

pub struct Ctx {
    pub cfg: Config,
    pub paths: Paths,
    pub state: Arc<StateStore>,
}

impl Ctx {
    pub fn new(cfg: Config, paths: Paths) -> anyhow::Result<Self> {
        let state = Arc::new(StateStore::open(&paths.db()).context("opening state database")?);
        Ok(Self { cfg, paths, state })
    }

    pub fn account(&self, id: Option<&str>) -> anyhow::Result<AccountRecord> {
        let accounts = self.state.accounts()?;
        match id {
            Some(id) => accounts
                .into_iter()
                .find(|a| a.id == id)
                .with_context(|| format!("no account '{id}'")),
            None => match accounts.len() {
                0 => bail!("no accounts; run `holo login` first"),
                1 => Ok(accounts.into_iter().next().expect("len 1")),
                _ => bail!("several accounts; pass --account (see `holo accounts`)"),
            },
        }
    }

    /// Find a root by id or local path.
    pub fn root(&self, key: &str) -> anyhow::Result<RootRecord> {
        if let Some(r) = self.state.root(key)? {
            return Ok(r);
        }
        let p = std::path::absolute(key).unwrap_or_else(|_| key.into());
        self.state
            .root_by_local_path(&p.to_string_lossy())?
            .with_context(|| format!("no sync root '{key}' (see `holo roots`)"))
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// One-line description of an in-flight remote pull, if the daemon is in one.
pub fn pull_line(ctx: &Ctx, root_id: &str) -> anyhow::Result<Option<String>> {
    use holo_engine::remote::{PullPhase, PullProgress, pull_key};
    let Some(p) = ctx
        .state
        .kv_get(&pull_key(root_id))?
        .and_then(|j| serde_json::from_str::<PullProgress>(&j).ok())
    else {
        return Ok(None);
    };
    let secs = (chrono::Utc::now().timestamp_millis() - p.started_ms).max(0) / 1000;
    let what = if p.full { "full listing" } else { "changes" };
    let mut est = String::new();
    if let Some(pct) = p.percent() {
        est.push_str(&format!(" · ~{pct}%"));
    }
    if let Some(eta) = p.eta_secs(secs as f64) {
        est.push_str(&format!(" · ETA ~{}", fmt_secs(eta)));
    }
    let took = fmt_secs(secs as u64);
    Ok(match p.phase {
        PullPhase::Idle => None,
        PullPhase::Listing => Some(format!(
            "cloud: {what} · {} items in {} pages{est} · {took}",
            p.items, p.pages
        )),
        PullPhase::Applying => Some(format!(
            "cloud: applying {} items locally · {took}",
            p.items
        )),
    })
}

fn fmt_secs(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s / 60 % 60),
    }
}

/// `holo run` holds the lock file exclusively while it runs.
pub fn daemon_running(ctx: &Ctx) -> bool {
    let lock = ctx.paths.lock();
    if !lock.exists() {
        return false;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(lock).is_err()
}
