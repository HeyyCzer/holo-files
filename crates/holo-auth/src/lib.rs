//! Provider-agnostic OAuth 2.0 for installed apps (authorization code + PKCE with a
//! loopback redirect), token refresh, and encrypted token persistence.
//!
//! Works for Microsoft identity platform and Google alike; providers only supply an
//! [`OAuthConfig`].

mod flow;
mod manager;
mod store;

pub use flow::{interactive_login, refresh};
pub use manager::TokenManager;
pub use store::{FileTokenStore, TokenStore};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    /// Google "desktop app" clients ship a (non-confidential) secret; Microsoft public clients don't.
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    /// e.g. `prompt=select_account` (Microsoft) or `access_type=offline` (Google).
    pub extra_authorize_params: Vec<(String, String)>,
    /// Host used in the loopback redirect URI (`localhost` for Microsoft, `127.0.0.1` for Google).
    pub redirect_host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    pub fn expires_within(&self, secs: i64) -> bool {
        Utc::now() + chrono::Duration::seconds(secs) >= self.expires_at
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no stored credentials; run `holo login` first")]
    NotLoggedIn,
    #[error("authorization was denied or failed: {0}")]
    Denied(String),
    #[error("token endpoint error: {0}")]
    TokenEndpoint(String),
    #[error("refresh token rejected; log in again ({0})")]
    RefreshRejected(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("token storage: {0}")]
    Storage(String),
    #[error("timed out waiting for the browser redirect")]
    Timeout,
}

pub type AuthResult<T> = Result<T, AuthError>;
