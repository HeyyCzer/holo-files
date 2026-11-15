use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{AuthError, AuthResult, OAuthConfig, TokenSet, TokenStore, flow};

/// Hands out valid access tokens, refreshing (and persisting) them transparently.
pub struct TokenManager {
    cfg: OAuthConfig,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    key: String,
    cached: Mutex<Option<TokenSet>>,
}

impl TokenManager {
    pub fn new(
        cfg: OAuthConfig,
        http: reqwest::Client,
        store: Arc<dyn TokenStore>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            cfg,
            http,
            store,
            key: key.into(),
            cached: Mutex::new(None),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Store a freshly obtained token set (after interactive login).
    pub async fn set(&self, tokens: TokenSet) -> AuthResult<()> {
        self.store.save(&self.key, &tokens)?;
        *self.cached.lock().await = Some(tokens);
        Ok(())
    }

    /// A token valid for at least the next few minutes.
    pub async fn access_token(&self) -> AuthResult<String> {
        let mut guard = self.cached.lock().await;
        if guard.is_none() {
            *guard = Some(self.store.load(&self.key)?.ok_or(AuthError::NotLoggedIn)?);
        }
        let current = guard.as_ref().expect("loaded above");
        if !current.expires_within(300) {
            return Ok(current.access_token.clone());
        }
        let rt = current
            .refresh_token
            .clone()
            .ok_or(AuthError::NotLoggedIn)?;
        tracing::debug!(key = %self.key, "refreshing access token");
        let fresh = flow::refresh(&self.cfg, &self.http, &rt).await?;
        self.store.save(&self.key, &fresh)?;
        let token = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(token)
    }

    /// Force the next `access_token()` to refresh (call after an HTTP 401).
    pub async fn invalidate(&self) {
        if let Some(t) = self.cached.lock().await.as_mut() {
            t.expires_at = chrono::Utc::now();
        }
    }

    pub async fn logout(&self) -> AuthResult<()> {
        *self.cached.lock().await = None;
        self.store.delete(&self.key)
    }
}
