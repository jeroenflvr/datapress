//! Embedded MkDocs Material documentation site.
//!
//! Compiled in only when the `docs` cargo feature is enabled. The site
//! contents live under `<workspace>/docs/site/` (built by
//! `task docs:build`); `include_dir!` embeds the whole directory tree
//! at compile time, so the resulting binary serves the docs without
//! touching the filesystem at runtime.

use actix_web::{HttpRequest, HttpResponse, http::header, web};
use include_dir::{Dir, include_dir};

/// Built MkDocs site, embedded at compile time.
///
/// `$CARGO_MANIFEST_DIR` is `crates/core/`; the docs build output lives
/// two levels up under `docs/site/`.
static SITE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../docs/site");

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
    match SITE.get_file(p) {
        Some(f) => HttpResponse::Ok()
            .content_type(mime_guess::from_path(p).first_or_octet_stream().as_ref())
            .body(f.contents()),
        None => HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("Not Found"),
    }
}
