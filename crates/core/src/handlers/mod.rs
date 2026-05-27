//! HTTP handler surface, organised by API version.
//!
//! ## Layout
//!
//! - This module hosts **unversioned** endpoints (liveness / readiness /
//!   `/health`) plus shared utilities used by every version (content
//!   negotiation, the [`BackendData`] extractor type, the Arrow IPC MIME
//!   constant).
//! - Each API version lives in its own submodule ([`v1`], future
//!   `v2`, …). Versions expose a single [`actix_web::web::ServiceConfig`]
//!   registration function so the server can mount them under a scope:
//!
//!   ```ignore
//!   App::new()
//!       .service(web::scope("/api/v1").configure(handlers::v1::configure))
//!   ```
//!
//! ## Adding a new version
//!
//! 1. Copy `v1.rs` to `v2.rs` and adjust the request / response handlers.
//! 2. Add `pub mod v2;` below.
//! 3. Mount it in [`crate::server::serve`] under `/api/v2`.
//! 4. Decide whether `v1` should be kept (it usually is, for a deprecation
//!    window) or removed.
//!
//! Handlers inside a version module are plain `async fn` (no route
//! macros) so the same handler can be re-mounted in multiple scopes —
//! that's how the legacy un-versioned `/api/datasets/...` alias works
//! without duplicating code.

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, get, web};

use crate::backend::Backend;

pub mod v1;

/// Convenience alias — every handler extracts the backend through this.
pub type BackendData = web::Data<Arc<dyn Backend>>;

/// MIME type used for Arrow IPC stream responses.
pub const ARROW_IPC_MIME: &str = "application/vnd.apache.arrow.stream";

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

/// Liveness probe. Mounted outside the configured `prefix` at a fixed
/// path so orchestrators don't need to know how the server is exposed.
#[get("/healthz")]
pub async fn healthz() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

/// Readiness probe. Returns `200` once at least one dataset is registered
/// (i.e. the registry finished loading at startup), `503` otherwise.
#[get("/readyz")]
pub async fn readyz(backend: BackendData) -> HttpResponse {
    let names = backend.names();
    if names.is_empty() {
        HttpResponse::ServiceUnavailable()
            .content_type("application/json")
            .body(r#"{"status":"not ready","reason":"no datasets registered"}"#)
    } else {
        let body = format!(r#"{{"status":"ready","datasets":{}}}"#, names.len());
        HttpResponse::Ok().content_type("application/json").body(body)
    }
}

/// True if the caller wants Arrow IPC: either `?format=arrow` in the
/// query string, or `Accept` lists `application/vnd.apache.arrow.stream`.
/// A bare `Accept: */*` does **not** count — JSON stays the default.
pub(crate) fn wants_arrow(http: &HttpRequest) -> bool {
    let qs = http.query_string();
    if !qs.is_empty()
        && qs.split('&').any(|kv| matches!(kv.split_once('='), Some(("format", v)) if v.eq_ignore_ascii_case("arrow")))
    {
        return true;
    }
    http.headers()
        .get(actix_web::http::header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.split(',').any(|part| {
            part.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case(ARROW_IPC_MIME)
        }))
        .unwrap_or(false)
}
