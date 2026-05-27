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

/// Build / version metadata published by [`version`] at `/version`.
///
/// Populated once by [`crate::server::serve`] from compile-time
/// constants (`CARGO_PKG_*`) and optional build-time env vars
/// (`DATAPRESS_GIT_SHA`, `DATAPRESS_BUILD_TIME`), and stored in actix
/// app data. The handler just serialises it to JSON.
#[derive(Clone, Debug, serde::Serialize)]
pub struct BuildInfo {
    /// Crate name (e.g. `"datapress-core"`).
    pub name:        &'static str,
    /// Crate version from `CARGO_PKG_VERSION` (e.g. `"0.1.17"`).
    pub version:     &'static str,
    /// Human-readable backend label — `"DuckDB"` or `"DataFusion"`.
    pub backend:     &'static str,
    /// Git commit SHA the binary was built from. `None` when
    /// `DATAPRESS_GIT_SHA` was not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha:     Option<&'static str>,
    /// ISO-8601 build timestamp. `None` when `DATAPRESS_BUILD_TIME`
    /// was not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_time:  Option<&'static str>,
    /// `"debug"` or `"release"`, derived from `cfg!(debug_assertions)`.
    pub profile:     &'static str,
    /// Rust target triple the binary was built for (e.g.
    /// `"aarch64-apple-darwin"`). `None` when `DATAPRESS_TARGET` was
    /// not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target:      Option<&'static str>,
}

impl BuildInfo {
    /// Build a `BuildInfo` populated from compile-time constants. The
    /// caller supplies the backend `label` (the binaries know which
    /// they are; this crate doesn't).
    pub fn new(backend: &'static str) -> Self {
        Self {
            name:       env!("CARGO_PKG_NAME"),
            version:    env!("CARGO_PKG_VERSION"),
            backend,
            git_sha:    option_env!("DATAPRESS_GIT_SHA"),
            build_time: option_env!("DATAPRESS_BUILD_TIME"),
            profile:    if cfg!(debug_assertions) { "debug" } else { "release" },
            target:     option_env!("DATAPRESS_TARGET"),
        }
    }
}

/// Build / version info. Mounted unprefixed so orchestrators and
/// release-tracking tools can hit it without knowing how the server
/// is exposed. Always returns `200` with a JSON object.
#[get("/version")]
pub async fn version(info: web::Data<BuildInfo>) -> HttpResponse {
    HttpResponse::Ok().json(info.get_ref())
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
