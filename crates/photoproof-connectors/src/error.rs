//! Unified connector error (spec/RUNTIME.md §4, normative).

use std::time::Duration;

/// Unified connector error. Every variant maps to a supervision or
/// degradation behavior; none of them ever surfaces as user-facing prose.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// Backing service not Ready (starting, restarting, or Failed).
    /// Callers must treat the feature as unavailable, not retry-loop.
    #[error("backend not ready: {0}")]
    NotReady(&'static str),
    /// Transport-level failure mid-call (process died, socket closed).
    /// The supervisor restarts; callers may retry exactly once (RUNTIME §13).
    #[error("backend connection lost")]
    ConnectionLost(#[source] std::io::Error),
    #[error("backend timeout after {0:?}")]
    Timeout(Duration),
    /// Non-2xx or protocol-level error from the backend.
    #[error("backend error {status}: {message}")]
    Backend { status: u16, message: String },
    /// Response arrived but could not be decoded (bad JSON, schema
    /// violation after constrained decoding — a bug, log loudly).
    #[error("malformed backend response: {0}")]
    Decode(String),
    /// Cloud backends only: key ref unresolvable, 401/403.
    /// Never contains key material.
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("cancelled")]
    Cancelled,
}

pub type ConnectorResult<T> = Result<T, ConnectorError>;
