//! Version 1 of the dataset HTTP API.
//!
//! Routes (relative to whichever scope the caller mounts this module
//! under — typically `/api/v1`):
//!
//! | Method | Path                              | Description                          |
//! |--------|-----------------------------------|--------------------------------------|
//! | GET    | `/datasets`                       | List datasets with summaries         |
//! | GET    | `/datasets/{name}/schema`         | Schema + rows + indexed cols + sample |
//! | POST   | `/datasets/{name}/query`          | Query (JSON or Arrow IPC)            |
//! | POST   | `/datasets/{name}/count`          | Count matching rows                  |
//! | POST   | `/datasets/{name}/reload`         | Rebuild dataset (admin-only)         |
//!
//! Handlers are plain `async fn` (not route-macro structs) so the same
//! version can be mounted under multiple scopes — see
//! [`crate::server::serve`] for the canonical `/api/v1` mount and the
//! legacy `/api` alias.

use actix_web::{HttpRequest, HttpResponse, ResponseError, web};

use crate::admin;
use crate::handlers::{ARROW_IPC_MIME, BackendData, QueryLimits, wants_arrow};
use crate::models::{CountRequest, QueryRequest};

// -------------------------------------------------------------- auth guards --

/// Enforce the configured `read` scopes when the `auth` feature is on
/// and OIDC enforcement is enabled. When disabled (either at build time
/// or in config) this is a no-op.
#[cfg(feature = "auth")]
fn require_read(req: &HttpRequest) -> Result<(), crate::errors::AppError> {
    use std::sync::Arc;
    if let Some(cfg) = req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>()
        && cfg.enabled
        && !cfg.anonymous_read
    {
        return crate::auth::require_scopes(req, &cfg.read_scopes);
    }
    Ok(())
}
#[cfg(not(feature = "auth"))]
fn require_read(_: &HttpRequest) -> Result<(), crate::errors::AppError> {
    Ok(())
}

/// Allow the request to perform a reload if EITHER the legacy admin
/// token matches OR (when `auth` is enabled) the caller holds the
/// configured reload scopes. The two paths are independent so operators
/// can migrate to OIDC without breaking existing automation.
fn require_reload(req: &HttpRequest) -> Result<(), crate::errors::AppError> {
    #[cfg(feature = "auth")]
    let admin_ok = admin::require_admin(req).is_ok();
    #[cfg(feature = "auth")]
    {
        use std::sync::Arc;
        if let Some(cfg) = req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>()
            && cfg.enabled
        {
            let scope_ok = crate::auth::require_scopes(req, &cfg.reload_scopes).is_ok();
            if admin_ok && cfg.admin_token_fallback {
                return Ok(());
            }
            if scope_ok {
                return Ok(());
            }
            // Neither path satisfied — surface the scope error so
            // the client gets a 401/403 with a Bearer challenge.
            return crate::auth::require_scopes(req, &cfg.reload_scopes);
        }
    }
    // No OIDC layer — fall back to the admin-token check.
    admin::require_admin(req)
}

/// Register every v1 route on the provided actix [`web::ServiceConfig`].
///
/// Call this inside a [`web::scope`] — usually `/api/v1` — so paths come
/// out as `/api/v1/datasets/...`.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/datasets", web::get().to(list_datasets))
        .route("/datasets/{name}/schema", web::get().to(get_schema))
        .route("/datasets/{name}/query", web::post().to(query_dataset))
        .route("/datasets/{name}/count", web::post().to(count_dataset))
        .route("/datasets/{name}/reload", web::post().to(reload_dataset));
}

/// Route table for log_routes-style introspection. Each entry is
/// `(method, path-suffix)` relative to the version's mount scope.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/datasets"),
    ("GET", "/datasets/{name}/schema"),
    ("POST", "/datasets/{name}/query"),
    ("POST", "/datasets/{name}/count"),
    ("POST", "/datasets/{name}/reload"),
];

// ---------------------------------------------------------------- handlers --

pub async fn list_datasets(req: HttpRequest, backend: BackendData) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let summaries: Vec<_> = backend
        .names()
        .into_iter()
        .filter_map(|n| backend.summary(&n).ok())
        .collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": summaries }))
}

pub async fn get_schema(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    let schema = match backend.schema(&name) {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    let summary = match backend.summary(&name) {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    let indexed = match backend.indexed_columns(&name) {
        Ok(i) => i,
        Err(e) => return e.error_response(),
    };
    let sample = match backend.sample(&name).await {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    let body = format!(
        r#"{{"name":{name_lit},"rows":{rows},"columns":{cols},"indexed":{indexed},"sample":{sample}}}"#,
        name_lit = serde_json::to_string(&schema.name).unwrap(),
        rows = summary.rows,
        cols = serde_json::to_string(&schema.columns).unwrap(),
        indexed = serde_json::to_string(&indexed).unwrap(),
    );
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body)
}

pub async fn query_dataset(
    http: HttpRequest,
    backend: BackendData,
    limits: Option<web::Data<QueryLimits>>,
    path: web::Path<String>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    if let Err(e) = require_read(&http) {
        return e.error_response();
    }
    let name = path.into_inner();
    let max_page_size = limits
        .as_ref()
        .map(|l| l.max_page_size)
        .unwrap_or_else(|| QueryLimits::default().max_page_size)
        .max(1);
    let page = body.page.max(1);
    let page_size = body.page_size.clamp(1, max_page_size);
    let mut req = body.into_inner();
    req.page = page;
    req.page_size = page_size;

    // Content negotiation: clients opt into Arrow IPC via the `Accept`
    // header or `?format=arrow`. Anything else (including no header)
    // gets the historical JSON envelope.
    if wants_arrow(&http) {
        return match backend.query_arrow(&name, &req).await {
            Ok(bytes) => HttpResponse::Ok()
                .content_type(ARROW_IPC_MIME)
                .insert_header(("X-Page", page.to_string()))
                .insert_header(("X-Page-Size", page_size.to_string()))
                .body(bytes),
            Err(e) => e.error_response(),
        };
    }

    match backend.query(&name, &req).await {
        Ok(arr) => {
            let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
            HttpResponse::Ok()
                .content_type("application/json")
                .body(body)
        }
        Err(e) => e.error_response(),
    }
}

pub async fn count_dataset(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
    body: Option<web::Json<CountRequest>>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    let req = body.map(|b| b.into_inner()).unwrap_or_default();

    match backend.count(&name, &req).await {
        Ok(n) => HttpResponse::Ok().json(serde_json::json!({ "count": n })),
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: rebuild a dataset from disk and atomically swap it in.
/// Requires `X-Admin-Token` matching `$ADMIN_TOKEN`. Disabled if the env
/// var is unset.
pub async fn reload_dataset(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    match backend.reload(&name).await {
        Ok(stats) => HttpResponse::Ok().json(serde_json::json!({
            "dataset":    name,
            "rows":       stats.rows,
            "elapsed_ms": stats.elapsed_ms,
        })),
        Err(e) => e.error_response(),
    }
}
