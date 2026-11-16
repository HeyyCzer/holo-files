//! User configuration (`%APPDATA%\holo\config.toml`) and on-disk locations.

use std::{path::PathBuf, time::Duration};

use anyhow::Context as _;
use holo_engine::{EngineConfig, adaptive::Concurrency};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub onedrive: OneDriveConfig,
    pub gdrive: GoogleDriveConfig,
    pub sync: SyncConfig,
    pub update: UpdateConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OneDriveConfig {
    /// Application (client) ID from the Entra ID app registration.
    pub client_id: String,
    /// `common` (personal + work), `consumers`, `organizations` or a tenant id.
    pub tenant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GoogleDriveConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    pub poll_interval_secs: u64,
    /// Uploads/journal ops in parallel: `"auto"` (adapts to queue size, PC use and
    /// server throttling, between `concurrency_min` and `concurrency_max`) or a
    /// fixed number. The HTTP limit is the maximum + 4, so opening files
    /// (hydration) never waits behind a full upload queue.
    pub concurrency: ConcurrencySetting,
    pub concurrency_min: usize,
    pub concurrency_max: usize,
    pub mass_delete_threshold: usize,
    /// Minimum MiB fetched per hydration request.
    pub read_ahead_mib: u64,
    pub full_scan_interval_mins: u64,
    pub use_default_ignores: bool,
    /// Extra `.gitignore`-style patterns applied to every root.
    pub ignore: Vec<String>,
    pub blocked_hydration_processes: Option<Vec<String>>,
}

/// What `holo-ui` does when a newer release is published on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Download in the background and offer it in the tray menu (default).
    #[default]
    Notify,
    /// Download and install as soon as nothing is syncing.
    Auto,
    /// Never check.
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    pub mode: UpdateMode,
    pub check_interval_hours: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            mode: UpdateMode::Notify,
            check_interval_hours: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConcurrencySetting {
    Fixed(usize),
    /// Only `"auto"` is valid (checked on load).
    Mode(String),
}

impl SyncConfig {
    pub fn concurrency(&self) -> Concurrency {
        match &self.concurrency {
            ConcurrencySetting::Fixed(n) => Concurrency::Fixed((*n).max(1)),
            ConcurrencySetting::Mode(_) => Concurrency::Auto {
                min: self.concurrency_min.max(1),
                max: self.concurrency_max.max(self.concurrency_min).max(1),
            },
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            concurrency: ConcurrencySetting::Mode("auto".into()),
            concurrency_min: 1,
            concurrency_max: 16,
            mass_delete_threshold: 200,
            read_ahead_mib: 4,
            full_scan_interval_mins: 60,
            use_default_ignores: true,
            ignore: Vec::new(),
            blocked_hydration_processes: None,
        }
    }
}

pub struct Paths {
    pub config_file: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn resolve() -> anyhow::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "holo")
            .context("cannot resolve user directories")?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            data: dirs.data_local_dir().to_path_buf(),
        })
    }
    pub fn db(&self) -> PathBuf {
        self.data.join("state.db")
    }
    pub fn tokens(&self) -> PathBuf {
        self.data.join("tokens")
    }
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }
    pub fn trash(&self) -> PathBuf {
        self.data.join("trash")
    }
    pub fn lock(&self) -> PathBuf {
        self.data.join("daemon.lock")
    }
}

const TEMPLATE: &str = r#"# holo-files configuration

[onedrive]
# Application (client) ID of your Entra ID app registration (see README).
client_id = ""
# tenant = "common"

[gdrive]
# Google Drive support is not implemented yet.
client_id = ""

[sync]
poll_interval_secs = 30
concurrency = "auto"   # or a fixed number of parallel uploads
concurrency_min = 1
concurrency_max = 16
mass_delete_threshold = 200
read_ahead_mib = 4
full_scan_interval_mins = 60
use_default_ignores = true
ignore = []

[update]
# Updates of the installed version (portable copies only get a notice):
# "notify" (download and offer in the tray menu), "auto" (install when idle) or "off".
mode = "notify"
check_interval_hours = 24
"#;

impl Config {
    /// Load, creating a commented template on first run.
    pub fn load(paths: &Paths) -> anyhow::Result<Self> {
        if !paths.config_file.exists() {
            if let Some(dir) = paths.config_file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&paths.config_file, TEMPLATE)?;
        }
        let text = std::fs::read_to_string(&paths.config_file)?;
        Self::parse(&text)
            .with_context(|| format!("invalid config {}", paths.config_file.display()))
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(text)?;
        if let ConcurrencySetting::Mode(m) = &cfg.sync.concurrency
            && !m.eq_ignore_ascii_case("auto")
        {
            anyhow::bail!("sync.concurrency must be \"auto\" or a number, got {m:?}");
        }
        Ok(cfg)
    }

    /// Write back to `config.toml`, keeping the file's comments, key order and layout:
    /// only values that changed are rewritten.
    pub fn save(&self, paths: &Paths) -> anyhow::Result<()> {
        let existing =
            std::fs::read_to_string(&paths.config_file).unwrap_or_else(|_| TEMPLATE.into());
        let out = self.merged_into(&existing)?;
        if let Some(dir) = paths.config_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = paths.config_file.with_extension("toml.tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, &paths.config_file)
            .with_context(|| format!("writing {}", paths.config_file.display()))
    }

    /// `existing` (a config file's text) with this config's values written into it.
    fn merged_into(&self, existing: &str) -> anyhow::Result<String> {
        let fresh: toml_edit::DocumentMut = toml::to_string(self)?.parse()?;
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap_or_else(|_| fresh.clone());
        merge(doc.as_table_mut(), fresh.as_table());
        let out = doc.to_string();
        Self::parse(&out).context("the saved config would not load")?;
        Ok(out)
    }

    pub fn engine(&self, paths: &Paths) -> EngineConfig {
        let s = &self.sync;
        let mut e = EngineConfig {
            poll_interval: Duration::from_secs(s.poll_interval_secs.max(5)),
            min_fetch: s.read_ahead_mib * 1024 * 1024,
            mass_delete_threshold: s.mass_delete_threshold,
            full_scan_interval: Duration::from_secs(s.full_scan_interval_mins.max(1) * 60),
            use_default_ignores: s.use_default_ignores,
            ignore: s.ignore.clone(),
            trash_dir: paths.trash(),
            concurrency: s.concurrency(),
            ..EngineConfig::default()
        };
        if let Some(b) = &s.blocked_hydration_processes {
            e.blocked_hydration_processes = b.clone();
        }
        e
    }
}

/// Make `old` hold `new`'s values: keys missing from `new` are dropped, changed values
/// replaced in place (keeping their comments), new keys appended.
fn merge(old: &mut toml_edit::Table, new: &toml_edit::Table) {
    use toml_edit::Item;
    let stale: Vec<String> = old
        .iter()
        .map(|(k, _)| k.to_owned())
        .filter(|k| !new.contains_key(k))
        .collect();
    for k in stale {
        old.remove(&k);
    }
    for (k, v) in new.iter() {
        match (old.get_mut(k), v) {
            (Some(Item::Table(o)), Item::Table(n)) => merge(o, n),
            (Some(Item::Value(o)), Item::Value(n)) => {
                let bare = |v: &toml_edit::Value| v.clone().decorated("", "").to_string();
                if bare(o) != bare(n) {
                    let decor = o.decor().clone();
                    *o = n.clone();
                    *o.decor_mut() = decor;
                }
            }
            _ => {
                old.insert(k, v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_keeps_comments_and_only_changes_values() {
        let mut cfg = Config::parse(TEMPLATE).unwrap();
        cfg.onedrive.client_id = "abc".into();
        cfg.onedrive.tenant = Some("consumers".into());
        cfg.sync.concurrency = ConcurrencySetting::Fixed(4);
        cfg.sync.ignore = vec!["*.tmp".into()];
        let out = cfg.merged_into(TEMPLATE).unwrap();
        assert!(out.contains("# Application (client) ID of your Entra ID"));
        assert!(out.contains("client_id = \"abc\""));
        assert!(out.contains("tenant = \"consumers\""));
        assert!(out.contains("concurrency = 4   # or a fixed number"));
        assert!(out.contains("ignore = [\"*.tmp\"]"));
        let back = Config::parse(&out).unwrap();
        assert_eq!(back.sync.concurrency(), Concurrency::Fixed(4));
        // Unchanged config: byte-identical file.
        let same = Config::parse(TEMPLATE)
            .unwrap()
            .merged_into(TEMPLATE)
            .unwrap();
        assert_eq!(same, TEMPLATE);
    }

    fn sync(toml_text: &str) -> SyncConfig {
        toml::from_str::<Config>(toml_text).unwrap().sync
    }

    #[test]
    fn concurrency_number_is_fixed_and_auto_uses_bounds() {
        assert_eq!(
            sync("[sync]\nconcurrency = 4").concurrency(),
            Concurrency::Fixed(4)
        );
        assert_eq!(
            sync("[sync]\nconcurrency = \"auto\"\nconcurrency_max = 12").concurrency(),
            Concurrency::Auto { min: 1, max: 12 }
        );
        assert_eq!(
            SyncConfig::default().concurrency(),
            Concurrency::Auto { min: 1, max: 16 }
        );
    }

    #[test]
    fn template_parses() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(matches!(cfg.sync.concurrency(), Concurrency::Auto { .. }));
        assert_eq!(cfg.update.mode, UpdateMode::Notify);
    }
}
