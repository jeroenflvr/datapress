//! Generic actix-web handlers shared by every backend.
//!
//! These wire up the HTTP surface against the [`Backend`] trait, so adding
//! a new backend is just a matter of implementing the trait and calling
//! [`crate::server::serve`].

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, ResponseError, get, post, web};

use crate::admin;
use crate::backend::Backend;
use crate::models::{CountRequest, QueryRequest};

/// Convenience alias — every handler extracts the backend through this.
pub type BackendData = web::Data<Arc<dyn Backend>>;

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

#[get("/api/datasets")]
pub async fn list_datasets(backend: BackendData) -> HttpResponse {
    let summaries: Vec<_> = backend
        .names()
        .into_iter()
        .filter_map(|n| backend.summary(&n).ok())
        .collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": summaries }))
}

#[get("/api/datasets/{name}/schema")]
pub async fn get_schema(
    backend: BackendData,
    path:    web::Path<String>,
) -> HttpResponse {
    let name = path.into_inner();
    let schema = match backend.schema(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let sample = match backend.sample(&name).await {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let body = format!(
        r#"{{"name":{name_lit},"columns":{cols},"sample":{sample}}}"#,
        name_lit = serde_json::to_string(&schema.name).unwrap(),
        cols     = serde_json::to_string(&schema.columns).unwrap(),
    );
    HttpResponse::Ok().content_type("application/json").body(body)
}

#[post("/api/datasets/{name}/query")]
pub async fn query_dataset(
    backend: BackendData,
    path:    web::Path<String>,
    body:    web::Json<QueryRequest>,
) -> HttpResponse {
    let name      = path.into_inner();
    let page      = body.page.max(1);
    let page_size = body.page_size.clamp(1, 1000);
    let req       = body.into_inner();

    match backend.query(&name, &req).await {
        Ok(arr) => {
            let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
            HttpResponse::Ok().content_type("application/json").body(body)
        }
        Err(e) => e.error_response(),
    }
}

#[post("/api/datasets/{name}/count")]
pub async fn count_dataset(
    backend: BackendData,
    path:    web::Path<String>,
    body:    Option<web::Json<CountRequest>>,
) -> HttpResponse {
    let name = path.into_inner();
    let req  = body.map(|b| b.into_inner()).unwrap_or_default();

    match backend.count(&name, &req).await {
        Ok(n)  => HttpResponse::Ok().json(serde_json::json!({ "count": n })),
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: rebuild a dataset from disk and atomically swap it in.
/// Requires `X-Admin-Token` matching `$ADMIN_TOKEN`. Disabled if the env
/// var is unset.
#[post("/api/datasets/{name}/reload")]
pub async fn reload_dataset(
    req:     HttpRequest,
    backend: BackendData,
    path:    web::Path<String>,
) -> HttpResponse {
    if let Err(e) = admin::require_admin(&req) {
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
