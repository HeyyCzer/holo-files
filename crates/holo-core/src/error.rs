use std::time::Duration;

/// Errors every provider maps its native failures into. The engine decides retry,
/// conflict handling and resync purely from these variants.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("item not found")]
    NotFound,
    /// The item changed remotely since the version we based our operation on
    /// (HTTP 412 / eTag mismatch).
    #[error("remote item changed (precondition failed)")]
    PreconditionFailed,
    /// A sibling with the same name already exists.
    #[error("name already exists remotely")]
    NameConflict,
    /// The change cursor is no longer valid; a full re-enumeration is required.
    #[error("change cursor expired; full resync required")]
    ResyncRequired,
    #[error("throttled by provider (retry after {retry_after:?})")]
    Throttled { retry_after: Option<Duration> },
    #[error("not authenticated: {0}")]
    Unauthorized(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("not implemented by this provider: {0}")]
    NotImplemented(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Whether retrying the same request later can reasonably succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Throttled { .. } | Self::Network(_) => true,
            Self::Http { status, .. } => *status >= 500 || *status == 408,
            _ => false,
        }
    }
}

pub type ProviderResult<T> = Result<T, ProviderError>;
