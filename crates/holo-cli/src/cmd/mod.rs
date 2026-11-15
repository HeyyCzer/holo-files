pub mod accounts;
pub mod misc;
pub mod plan;
pub mod roots;
pub mod run;

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
