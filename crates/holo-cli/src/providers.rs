//! Provider factory: the only place that knows concrete provider types.

use std::sync::Arc;

use anyhow::{Context as _, bail};
use holo_auth::{FileTokenStore, OAuthConfig, TokenManager, TokenStore};
use holo_core::{CloudProvider, ProviderKind};
use holo_onedrive::{GraphOptions, OneDriveProvider};
use holo_state::AccountRecord;

use crate::config::{Config, Paths};

pub fn oauth_config(cfg: &Config, kind: ProviderKind) -> anyhow::Result<OAuthConfig> {
    match kind {
        ProviderKind::OneDrive => {
            if cfg.onedrive.client_id.trim().is_empty() {
                bail!(
                    "set [onedrive].client_id in the config file first (`holo config` shows where it is)"
                );
            }
            Ok(holo_onedrive::oauth_config(
                &cfg.onedrive.client_id,
                cfg.onedrive.tenant.as_deref(),
            ))
        }
        ProviderKind::GoogleDrive => {
            if cfg.gdrive.client_id.trim().is_empty() {
                bail!("set [gdrive].client_id in the config file first");
            }
            Ok(holo_gdrive::oauth_config(
                &cfg.gdrive.client_id,
                cfg.gdrive.client_secret.as_deref(),
            ))
        }
    }
}

pub fn token_store(paths: &Paths) -> anyhow::Result<Arc<dyn TokenStore>> {
    Ok(Arc::new(FileTokenStore::new(paths.tokens())?))
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("holo-files/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

pub fn token_manager(
    cfg: &Config,
    paths: &Paths,
    kind: ProviderKind,
    key: &str,
) -> anyhow::Result<Arc<TokenManager>> {
    Ok(Arc::new(TokenManager::new(
        oauth_config(cfg, kind)?,
        http(),
        token_store(paths)?,
        key,
    )))
}

pub async fn connect(
    cfg: &Config,
    kind: ProviderKind,
    tokens: Arc<TokenManager>,
) -> anyhow::Result<Arc<dyn CloudProvider>> {
    match kind {
        ProviderKind::OneDrive => {
            let opts = GraphOptions {
                concurrency: cfg.sync.concurrency.max(1) + 4,
                ..GraphOptions::default()
            };
            Ok(Arc::new(
                OneDriveProvider::connect(tokens, opts)
                    .await
                    .context("connecting to OneDrive")?,
            ))
        }
        ProviderKind::GoogleDrive => {
            bail!("Google Drive is not implemented yet (see crates/holo-gdrive)")
        }
    }
}

pub async fn for_account(
    cfg: &Config,
    paths: &Paths,
    account: &AccountRecord,
) -> anyhow::Result<Arc<dyn CloudProvider>> {
    let tokens = token_manager(cfg, paths, account.provider, &account.id)?;
    connect(cfg, account.provider, tokens).await
}
