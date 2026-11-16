//! Thin Graph HTTP client: auth header, global concurrency limit, retry with
//! `Retry-After` and exponential backoff + jitter, error mapping.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use holo_auth::TokenManager;
use holo_core::{ProviderError, ProviderResult};
use reqwest::{Method, RequestBuilder, Response, StatusCode, redirect};
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;

use crate::dto::GraphErrorBody;

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";

#[derive(Debug, Clone)]
pub struct GraphOptions {
    /// Max requests in flight to Graph (and to the download CDN).
    pub concurrency: usize,
    pub max_retries: u32,
    pub request_timeout: Duration,
    /// Simple PUT upload threshold; above it an upload session is used.
    pub simple_upload_max: u64,
    /// Upload session chunk size; rounded down to a multiple of 320 KiB.
    pub chunk_size: u64,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            concurrency: 4,
            max_retries: 6,
            request_timeout: Duration::from_secs(120),
            simple_upload_max: 4 * 1024 * 1024,
            chunk_size: 10 * 1024 * 1024,
        }
    }
}

pub struct GraphClient {
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    tokens: Arc<TokenManager>,
    limiter: Semaphore,
    throttled: AtomicU64,
    pub opts: GraphOptions,
}

fn net(e: reqwest::Error) -> ProviderError {
    ProviderError::Network(e.to_string())
}

fn retry_after(resp: &Response) -> Option<Duration> {
    resp.headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1 << attempt.min(6));
    let jitter = rand::random::<u64>() % (base / 2 + 1);
    Duration::from_millis((base + jitter).min(60_000))
}

/// Map a non-success response to a [`ProviderError`].
pub async fn error_from(resp: Response) -> ProviderError {
    let status = resp.status();
    let ra = retry_after(&resp);
    let body = resp.text().await.unwrap_or_default();
    let (code, message) = serde_json::from_str::<GraphErrorBody>(&body)
        .map(|b| (b.error.code, b.error.message.unwrap_or_default()))
        .unwrap_or_else(|_| (String::new(), body.chars().take(300).collect()));
    match status {
        StatusCode::NOT_FOUND => ProviderError::NotFound,
        StatusCode::CONFLICT => ProviderError::NameConflict,
        StatusCode::PRECONDITION_FAILED => ProviderError::PreconditionFailed,
        StatusCode::GONE => ProviderError::ResyncRequired,
        StatusCode::UNAUTHORIZED => ProviderError::Unauthorized(message),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
            ProviderError::Throttled { retry_after: ra }
        }
        _ if code == "resyncRequired" => ProviderError::ResyncRequired,
        s => ProviderError::Http {
            status: s.as_u16(),
            message: format!("{code} {message}").trim().to_owned(),
        },
    }
}

impl GraphClient {
    pub fn new(tokens: Arc<TokenManager>, opts: GraphOptions) -> ProviderResult<Self> {
        let build = |policy| {
            reqwest::Client::builder()
                .user_agent(concat!("holo-files/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(20))
                .timeout(opts.request_timeout)
                .redirect(policy)
                .build()
                .map_err(net)
        };
        Ok(Self {
            http: build(redirect::Policy::limited(5))?,
            no_redirect: build(redirect::Policy::none())?,
            tokens,
            limiter: Semaphore::new(opts.concurrency.max(1)),
            throttled: AtomicU64::new(0),
            opts,
        })
    }

    /// 429/503 responses seen so far.
    pub fn throttle_count(&self) -> u64 {
        self.throttled.load(Ordering::Relaxed)
    }

    async fn token(&self) -> ProviderResult<String> {
        self.tokens
            .access_token()
            .await
            .map_err(|e| ProviderError::Unauthorized(e.to_string()))
    }

    /// Core retry loop. `build` is called per attempt with the client and bearer token
    /// (`None` for pre-authenticated URLs).
    async fn execute(
        &self,
        client: &reqwest::Client,
        authenticated: bool,
        build: &(dyn Fn(&reqwest::Client, Option<&str>) -> RequestBuilder + Send + Sync),
    ) -> ProviderResult<Response> {
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let token = if authenticated {
                Some(self.token().await?)
            } else {
                None
            };
            let started = std::time::Instant::now();
            let result = {
                let _permit = self
                    .limiter
                    .acquire()
                    .await
                    .expect("semaphore never closed");
                build(client, token.as_deref()).send().await
            };
            let err = match result {
                Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                    tracing::trace!(status = %resp.status(), ms = started.elapsed().as_millis() as u64, url = %resp.url().path(), "graph");
                    return Ok(resp);
                }
                Ok(resp) => {
                    tracing::debug!(status = %resp.status(), url = %resp.url().path(), "graph error");
                    error_from(resp).await
                }
                Err(e) => net(e),
            };
            if matches!(err, ProviderError::Throttled { .. }) {
                self.throttled.fetch_add(1, Ordering::Relaxed);
            }
            match &err {
                ProviderError::Unauthorized(_) if authenticated && !refreshed => {
                    refreshed = true;
                    self.tokens.invalidate().await;
                    continue;
                }
                e if e.is_transient() && attempt < self.opts.max_retries => {
                    let wait = match e {
                        ProviderError::Throttled {
                            retry_after: Some(d),
                        } => *d,
                        _ => backoff(attempt),
                    };
                    tracing::warn!(attempt, wait_ms = wait.as_millis() as u64, error = %e, "retrying graph request");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                _ => return Err(err),
            }
        }
    }

    pub async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
        if_match: Option<&str>,
    ) -> ProviderResult<Response> {
        self.execute(&self.http, true, &|c, token| {
            let mut rb = c.request(method.clone(), url);
            if let Some(t) = token {
                rb = rb.bearer_auth(t);
            }
            if let Some(b) = body {
                rb = rb.json(b);
            }
            if let Some(tag) = if_match {
                rb = rb.header("If-Match", tag);
            }
            rb
        })
        .await
    }

    pub async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
        if_match: Option<&str>,
    ) -> ProviderResult<T> {
        let resp = self.send(method, url, body, if_match).await?;
        resp.json().await.map_err(net)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> ProviderResult<T> {
        self.send_json(Method::GET, url, None, None).await
    }

    /// Authenticated PUT of raw bytes (simple upload).
    pub async fn put_bytes(
        &self,
        url: &str,
        data: bytes::Bytes,
        if_match: Option<&str>,
    ) -> ProviderResult<Response> {
        self.execute(&self.http, true, &|c, token| {
            let mut rb = c
                .put(url)
                .header("Content-Type", "application/octet-stream")
                .body(data.clone());
            if let Some(t) = token {
                rb = rb.bearer_auth(t);
            }
            if let Some(tag) = if_match {
                rb = rb.header("If-Match", tag);
            }
            rb
        })
        .await
    }

    /// Request a pre-authenticated URL (upload session, download CDN) without bearer token.
    pub async fn unauthenticated(
        &self,
        method: Method,
        url: &str,
        customize: impl Fn(RequestBuilder) -> RequestBuilder + Send + Sync,
    ) -> ProviderResult<Response> {
        self.execute(&self.http, false, &|c, _| {
            customize(c.request(method.clone(), url))
        })
        .await
    }

    /// GET an endpoint that answers with a redirect and return its `Location`.
    pub async fn redirect_location(&self, url: &str) -> ProviderResult<String> {
        let resp = self
            .execute(&self.no_redirect, true, &|c, token| {
                let rb = c.get(url);
                match token {
                    Some(t) => rb.bearer_auth(t),
                    None => rb,
                }
            })
            .await?;
        resp.headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                ProviderError::Other(format!("expected redirect, got {}", resp.status()))
            })
    }
}
