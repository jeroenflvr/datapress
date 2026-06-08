//! Error types for the DataPress client.

use serde_json::Value as JsonValue;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Everything that can go wrong talking to a DataPress server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The server returned a non-2xx status.
    ///
    /// `payload` is populated when the body parsed as JSON (DataPress
    /// errors are `{"error": "..."}`), so callers can match on the
    /// structured message without re-parsing `body`.
    #[error("HTTP {status}: {message}")]
    Http {
        /// HTTP status code (e.g. `404`, `400`, `503`).
        status: u16,
        /// Best-effort human-readable message (the `error` field when the
        /// body was JSON, otherwise a truncated copy of the raw body).
        message: String,
        /// Raw response body.
        body: String,
        /// Parsed JSON body, when the response was `application/json`.
        payload: Option<JsonValue>,
    },

    /// A transport-level failure (DNS, connect, timeout, TLS, …).
    #[error("transport error: {}", transport_detail(.0))]
    Transport(#[from] reqwest::Error),

    /// The response body could not be decoded as the expected type.
    #[error("decode error: {0}")]
    Decode(String),

    /// The server answered with JSON where Arrow IPC was requested, or
    /// vice-versa.
    #[error("unexpected content type: {0}")]
    UnexpectedContentType(String),

    /// An Arrow IPC stream could not be decoded.
    #[cfg(feature = "arrow")]
    #[error("arrow decode error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// The base URL was not a valid URL.
    #[error("invalid base url: {0}")]
    InvalidBaseUrl(String),
}

/// Flatten a [`reqwest::Error`] into its full cause chain.
///
/// `reqwest::Error`'s own `Display` only prints the outermost layer (e.g.
/// "error sending request for url (…)"), hiding the actionable root cause
/// ("operation timed out", "connection closed before message completed",
/// "tcp connect error: Connection refused", …). This walks `source()` and
/// appends each distinct layer so the message is self-explanatory.
fn transport_detail(err: &reqwest::Error) -> String {
    use std::error::Error;
    let mut msg = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !msg.contains(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        source = cause.source();
    }
    msg
}

impl ClientError {
    /// Build an [`ClientError::Http`] from a status and raw body,
    /// extracting the `error` field when the body is JSON.
    pub(crate) fn from_response(status: u16, body: String) -> Self {
        let payload = serde_json::from_str::<JsonValue>(&body).ok();
        let message = payload
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| body.chars().take(200).collect());
        ClientError::Http {
            status,
            message,
            body,
            payload,
        }
    }
}
