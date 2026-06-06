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
pub type BackendData = web::Data<Arc<dyn Backend>>;/// Query-related limits copied from `[server]` config into Actix app data.
#[derive(Debug, Clone, Copy)]
pub struct QueryLimits {
    pub max_page_size: u64,
}

impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            max_page_size: 100_000,
        }
    }
}

/// Raw-SQL endpoint settings copied from `[sql]` config into Actix app
/// data. When `enabled` is false the `POST /api/v1/sql` handler returns
/// `404` and never touches the engine.
#[derive(Debug, Clone, Copy)]
pub struct SqlSettings {
    pub enabled: bool,
    pub max_rows: u64,
}

impl Default for SqlSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rows: 100_000,
        }
    }
}

/// MIME type used for Arrow IPC stream responses.
pub const ARROW_IPC_MIME: &str = "application/vnd.apache.arrow.stream";#[get("/health")]
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
        HttpResponse::Ok()
            .content_type("application/json")
            .body(body)
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
    pub name: &'static str,
    /// Crate version from `CARGO_PKG_VERSION` (e.g. `"0.1.17"`).
    pub version: &'static str,
    /// Human-readable backend label — `"DuckDB"` or `"DataFusion"`.
    pub backend: &'static str,
    /// Git commit SHA the binary was built from. `None` when
    /// `DATAPRESS_GIT_SHA` was not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<&'static str>,
    /// ISO-8601 build timestamp. `None` when `DATAPRESS_BUILD_TIME`
    /// was not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_time: Option<&'static str>,
    /// `"debug"` or `"release"`, derived from `cfg!(debug_assertions)`.
    pub profile: &'static str,
    /// Rust target triple the binary was built for (e.g.
    /// `"aarch64-apple-darwin"`). `None` when `DATAPRESS_TARGET` was
    /// not set at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'static str>,
}

impl BuildInfo {
    /// Build a `BuildInfo` populated from compile-time constants. The
    /// caller supplies the backend `label` (the binaries know which
    /// they are; this crate doesn't).
    pub fn new(backend: &'static str) -> Self {
        Self {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            backend,
            git_sha: option_env!("DATAPRESS_GIT_SHA"),
            build_time: option_env!("DATAPRESS_BUILD_TIME"),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            target: option_env!("DATAPRESS_TARGET"),
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
        .map(|s| {
            s.split(',').any(|part| {
                part.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case(ARROW_IPC_MIME)
            })
        })
        .unwrap_or(false)
}

/// True if the caller wants to skip server-side HTTP compression for this
/// response: either `?compress=false` (also `0`/`no`/`off`) in the query
/// string, or an `X-No-Compress` request header. Browsers can't override
/// `Accept-Encoding` from `fetch`, so this gives a page-settable escape
/// hatch — handlers translate it into a `Content-Encoding: identity`
/// response header, which actix's `Compress` middleware treats as a
/// signal to leave the body untouched.
pub(crate) fn wants_no_compression(http: &HttpRequest) -> bool {
    let qs = http.query_string();
    if !qs.is_empty()
        && qs.split('&').any(|kv| {
            matches!(
                kv.split_once('='),
                Some(("compress", v))
                    if v.eq_ignore_ascii_case("false")
                        || v == "0"
                        || v.eq_ignore_ascii_case("no")
                        || v.eq_ignore_ascii_case("off")
            )
        })
    {
        return true;
    }
    http.headers().contains_key("x-no-compress")
}

/// MIME type used for Parquet export responses.
pub const PARQUET_MIME: &str = "application/vnd.apache.parquet";

/// Process-wide cache of encoded Parquet exports, keyed by dataset name.
///
/// The `/datasets/{name}/parquet` endpoint serves a complete Parquet file
/// with HTTP range support. A single client (e.g. DuckDB `httpfs`) issues
/// several requests against it — a `HEAD` for the length, then ranged
/// `GET`s for the footer and any row-group metadata — so every request
/// must observe the *same* bytes. Caching the encoded file makes those
/// requests cheap and consistent; [`crate::handlers::v1::reload_dataset`]
/// drops the entry after a successful reload so a fresh export is built
/// on next access.
#[derive(Default)]
pub struct ParquetCache {
    inner: std::sync::RwLock<std::collections::HashMap<String, Arc<bytes::Bytes>>>,
}

impl ParquetCache {
    /// Return the cached export for `name`, if one has been built.
    pub fn get(&self, name: &str) -> Option<Arc<bytes::Bytes>> {
        self.inner.read().ok()?.get(name).cloned()
    }

    /// Store `bytes` as the export for `name`, returning the stored handle.
    pub fn insert(&self, name: &str, bytes: bytes::Bytes) -> Arc<bytes::Bytes> {
        let shared = Arc::new(bytes);
        if let Ok(mut map) = self.inner.write() {
            map.insert(name.to_string(), shared.clone());
        }
        shared
    }

    /// Drop the cached export for `name` (no-op if absent).
    pub fn invalidate(&self, name: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.remove(name);
        }
    }
}

/// A single byte range resolved against a body of `total` bytes.
struct ByteRange {
    start: u64,
    /// Inclusive end offset.
    end: u64,
}

/// Parse a single-range HTTP `Range: bytes=…` header against a body of
/// `total` bytes.
///
/// Returns:
/// - `Ok(None)` when there is no (parseable) byte range — the caller
///   should serve the full body with `200`.
/// - `Ok(Some(range))` for a satisfiable single range — serve `206`.
/// - `Err(())` when the range is syntactically a `bytes=` range but
///   unsatisfiable — the caller should answer `416`.
///
/// Only the first range of a multi-range header is honoured; multi-range
/// `multipart/byteranges` responses are intentionally not implemented, so
/// such requests fall back to the full body.
fn parse_byte_range(header: &str, total: u64) -> Result<Option<ByteRange>, ()> {
    let spec = match header.trim().strip_prefix("bytes=") {
        Some(s) => s.trim(),
        None => return Ok(None),
    };
    // Take the first range only.
    let first = spec.split(',').next().unwrap_or("").trim();
    let (start_s, end_s) = match first.split_once('-') {
        Some(parts) => parts,
        None => return Ok(None),
    };

    if total == 0 {
        return Err(());
    }

    let (start, end) = if start_s.is_empty() {
        // Suffix range: `-N` → last N bytes.
        let n: u64 = end_s.trim().parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        let n = n.min(total);
        (total - n, total - 1)
    } else {
        let start: u64 = start_s.trim().parse().map_err(|_| ())?;
        let end: u64 = if end_s.trim().is_empty() {
            total - 1
        } else {
            end_s.trim().parse::<u64>().map_err(|_| ())?.min(total - 1)
        };
        (start, end)
    };

    if start > end || start >= total {
        return Err(());
    }
    Ok(Some(ByteRange { start, end }))
}

/// Serve `body` as an HTTP response with range + `HEAD` support.
///
/// Honours a single `Range: bytes=…` header (`206 Partial Content` with a
/// `Content-Range`), advertises `Accept-Ranges: bytes`, and lets actix's
/// dispatcher answer `HEAD` with the same headers (including the computed
/// `Content-Length`) but no body. This is what lets DuckDB's `httpfs` read
/// only the Parquet footer for a `count(*)` instead of the whole file.
pub fn serve_bytes_with_range(
    http: &HttpRequest,
    body: Arc<bytes::Bytes>,
    content_type: &str,
) -> HttpResponse {
    use actix_web::http::header;

    let total = body.len() as u64;

    let range = http
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    match range.map(|r| parse_byte_range(r, total)) {
        // Unsatisfiable byte range → 416 with the total size.
        Some(Err(())) => HttpResponse::RangeNotSatisfiable()
            .insert_header((header::CONTENT_RANGE, format!("bytes */{total}")))
            .insert_header((header::ACCEPT_RANGES, "bytes"))
            .finish(),
        // Satisfiable single range → 206 Partial Content. For a HEAD the
        // dispatcher drops the body bytes but keeps the Content-Length
        // derived from the slice, so range probes still see the right size.
        Some(Ok(Some(ByteRange { start, end }))) => HttpResponse::PartialContent()
            .insert_header((header::CONTENT_TYPE, content_type.to_string()))
            .insert_header((header::ACCEPT_RANGES, "bytes"))
            .insert_header((header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}")))
            .body(body.slice(start as usize..(end as usize + 1))),
        // No (parseable) range → full body with 200.
        _ => HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, content_type.to_string()))
            .insert_header((header::ACCEPT_RANGES, "bytes"))
            .body(bytes::Bytes::clone(&body)),
    }
}
