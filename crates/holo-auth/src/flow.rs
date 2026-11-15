use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

use crate::{AuthError, AuthResult, OAuthConfig, TokenSet};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

fn random_urlsafe(bytes: usize) -> String {
    let buf: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(buf)
}

/// Run the authorization-code + PKCE flow. `open_browser` receives the URL to open
/// (callers typically also print it, in case no browser can be launched).
pub async fn interactive_login(
    cfg: &OAuthConfig,
    http: &reqwest::Client,
    open_browser: impl FnOnce(&str),
) -> AuthResult<TokenSet> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://{}:{port}", cfg.redirect_host);

    let verifier = random_urlsafe(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(16);

    let mut url = Url::parse(&cfg.authorize_url).map_err(|e| AuthError::Denied(e.to_string()))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &cfg.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", &cfg.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        for (k, v) in &cfg.extra_authorize_params {
            q.append_pair(k, v);
        }
    }
    open_browser(url.as_str());

    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| AuthError::Timeout)??;

    let scope = cfg.scopes.join(" ");
    let mut form = vec![
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier.as_str()),
        ("scope", scope.as_str()),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_secret", secret));
    }
    token_request(cfg, http, &form, None).await
}

/// Exchange a refresh token for a fresh access token. Keeps the old refresh token if
/// the provider doesn't rotate it.
pub async fn refresh(
    cfg: &OAuthConfig,
    http: &reqwest::Client,
    refresh_token: &str,
) -> AuthResult<TokenSet> {
    let scope = cfg.scopes.join(" ");
    let mut form = vec![
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", scope.as_str()),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_secret", secret));
    }
    token_request(cfg, http, &form, Some(refresh_token))
        .await
        .map_err(|e| match e {
            AuthError::TokenEndpoint(m) if m.contains("invalid_grant") => {
                AuthError::RefreshRejected(m)
            }
            other => other,
        })
}

async fn token_request(
    cfg: &OAuthConfig,
    http: &reqwest::Client,
    form: &[(&str, &str)],
    previous_refresh: Option<&str>,
) -> AuthResult<TokenSet> {
    let resp = http.post(&cfg.token_url).form(form).send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        let msg = serde_json::from_str::<TokenErrorResponse>(&body)
            .map(|e| format!("{}: {}", e.error, e.error_description.unwrap_or_default()))
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(AuthError::TokenEndpoint(msg));
    }
    let t: TokenResponse =
        serde_json::from_str(&body).map_err(|e| AuthError::TokenEndpoint(e.to_string()))?;
    Ok(TokenSet {
        access_token: t.access_token,
        refresh_token: t
            .refresh_token
            .or_else(|| previous_refresh.map(str::to_owned)),
        expires_at: Utc::now() + chrono::Duration::seconds(t.expires_in.unwrap_or(3600)),
        scope: t.scope,
    })
}

/// Accept loopback connections until one carries the OAuth redirect.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> AuthResult<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = req.lines().next().and_then(|l| l.split_whitespace().nth(1)) else {
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
            continue;
        };
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        if params.is_empty() {
            // favicon or similar probe
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }

        let (ok, result) = if let Some(err) = params.get("error") {
            let desc = params.get("error_description").cloned().unwrap_or_default();
            (false, Err(AuthError::Denied(format!("{err}: {desc}"))))
        } else if params.get("state").map(String::as_str) != Some(expected_state) {
            (false, Err(AuthError::Denied("state mismatch".into())))
        } else if let Some(code) = params.get("code") {
            (true, Ok(code.clone()))
        } else {
            (false, Err(AuthError::Denied("redirect had no code".into())))
        };

        let html = if ok {
            "<h2>holo-files: signed in.</h2><p>You can close this tab.</p>"
        } else {
            "<h2>holo-files: sign-in failed.</h2><p>Check the terminal.</p>"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        return result;
    }
}
