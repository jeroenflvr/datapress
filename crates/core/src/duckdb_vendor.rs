//! Shared embedded DuckDB-WASM vendor assets.
//!
//! Both the dataset explorer (`explorer` feature) and the self-hosted docs
//! site (`docs` feature) ship the vendored DuckDB-WASM blobs. Embedding the
//! ~77 MB of `*.wasm` once per feature pushed the PyPI `datap-rs` wheel over
//! PyPI's 100 MB per-file limit, so `build.rs` stages a *single* gzip-compressed
//! copy into `$OUT_DIR/duckdb_vendor/` and both features serve it from here.
//!
//! The large `*.wasm` blobs are stored as `<name>.gz`; callers serve them with
//! `Content-Encoding: gzip` (browsers inflate transparently). Every other asset
//! (worker scripts, bundled ESM, xterm CSS) is stored verbatim.

use actix_web::{HttpResponse, http::header};
use include_dir::{Dir, include_dir};

/// Self-hosted DuckDB-WASM assets, embedded once at compile time.
static DUCKDB_VENDOR: Dir<'_> = include_dir!("$OUT_DIR/duckdb_vendor");

/// Serve a vendored DuckDB-WASM asset by relative `path` (e.g.
/// `duckdb-mvp.wasm`). Returns the verbatim file when present, otherwise the
/// pre-gzipped `<path>.gz` copy tagged `Content-Encoding: gzip`. The blobs are
/// immutable per release, so responses carry a long-lived cache header.
///
/// Returns `None` when neither the raw nor the gzipped asset exists, letting
/// the caller produce its own 404.
pub(crate) fn serve(path: &str) -> Option<HttpResponse> {
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .as_ref()
        .to_owned();

    if let Some(f) = DUCKDB_VENDOR.get_file(path) {
        return Some(
            HttpResponse::Ok()
                .content_type(content_type)
                .insert_header((header::CACHE_CONTROL, "public, max-age=86400"))
                .body(f.contents()),
        );
    }
    if let Some(f) = DUCKDB_VENDOR.get_file(format!("{path}.gz")) {
        return Some(
            HttpResponse::Ok()
                .content_type(content_type)
                .insert_header((header::CONTENT_ENCODING, "gzip"))
                .insert_header((header::CACHE_CONTROL, "public, max-age=86400"))
                .body(f.contents()),
        );
    }
    None
}
