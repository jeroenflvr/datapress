//! Embedded MkDocs Material documentation site.
//!
//! Compiled in only when the `docs` cargo feature is enabled. The site
//! contents live under `<workspace>/docs/site/` (built by
//! `task docs:build`). `build.rs` stages a copy into `$OUT_DIR/docs_site/`
//! with the large vendored DuckDB-WASM `*.wasm` blobs stripped out (they are
//! served from the shared gzipped store instead — see [`crate::duckdb_vendor`]
//! — to avoid embedding ~77 MB of wasm twice and blowing past PyPI's wheel
//! size limit). `include_dir!` embeds the staged tree at compile time, so the
//! binary serves the docs without touching the filesystem at runtime.

use actix_web::{HttpRequest, HttpResponse, http::header, web};
use include_dir::{Dir, include_dir};

/// Built MkDocs site (minus the vendored DuckDB-WASM blobs), embedded at
/// compile time. `build.rs` stages it into `$OUT_DIR/docs_site/`.
static SITE: Dir<'_> = include_dir!("$OUT_DIR/docs_site");

/// Path prefix (relative to the docs mount) under which the vendored
/// DuckDB-WASM assets are served. The `*.wasm` blobs are stripped from the
/// embedded site and resolved from the shared gzipped store instead.
const DUCKDB_VENDOR_PREFIX: &str = "assets/vendor/duckdb/";

/// Mount the documentation site under `mount` (e.g. `/mkdocs`).
pub fn configure(mount: &str, cfg: &mut web::ServiceConfig) {
    // Redirect the bare mount (no trailing slash) so the browser's
    // base URL ends in `/` and the page's relative asset references
    // (`assets/...`) resolve under the mount instead of the site root.
    let redirect_target = format!("{mount}/");
    cfg.service(
        web::resource(mount.to_string()).route(web::get().to(move || {
            let to = redirect_target.clone();
            async move {
                HttpResponse::MovedPermanently()
                    .insert_header((header::LOCATION, to))
                    .finish()
            }
        })),
    )
    .service(
        web::scope(mount)
            .route("/", web::get().to(serve_index))
            .route("/{tail:.*}", web::get().to(serve)),
    );
}

async fn serve_index() -> HttpResponse {
    serve_path("index.html")
}

async fn serve(req: HttpRequest) -> HttpResponse {
    let tail: String = req.match_info().query("tail").into();
    // Map "foo/" → "foo/index.html" (MkDocs directory URLs).
    let path = if tail.is_empty() || tail.ends_with('/') {
        format!("{tail}index.html")
    } else {
        tail
    };
    serve_path(&path)
}

fn serve_path(p: &str) -> HttpResponse {
    if let Some(f) = SITE.get_file(p) {
        return HttpResponse::Ok()
            .content_type(mime_guess::from_path(p).first_or_octet_stream().as_ref())
            .body(f.contents());
    }
    // The large vendored DuckDB-WASM `*.wasm` blobs are stripped from the
    // embedded site to avoid duplicating them; serve those from the shared
    // gzipped store.
    if let Some(name) = p.strip_prefix(DUCKDB_VENDOR_PREFIX)
        && let Some(resp) = crate::duckdb_vendor::serve(name)
    {
        return resp;
    }
    HttpResponse::NotFound()
        .content_type("text/plain; charset=utf-8")
        .body("Not Found")
}
