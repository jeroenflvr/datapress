use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, ResponseError, get, post, web};

use datapress_core::admin;
use crate::db::{DbPool, Registry};
use crate::repository::DatasetRepository;
use datapress_core::errors::AppError;
use datapress_core::models::{CountRequest, QueryRequest};

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

#[get("/api/datasets")]
pub async fn list_datasets(reg: web::Data<Arc<Registry>>) -> HttpResponse {
    let summaries: Vec<_> = reg.names().into_iter().filter_map(|n| {
        reg.get(&n).ok().map(|s| serde_json::json!({
            "name":    s.name,
            "columns": s.columns.len(),
        }))
    }).collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": summaries }))
}

#[get("/api/datasets/{name}/schema")]
pub async fn get_schema(
    reg:   web::Data<Arc<Registry>>,
    path:  web::Path<String>,
) -> HttpResponse {
    let name = path.into_inner();
    let schema = match reg.get(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let pool = reg.pool.clone();
    let schema_for_block = schema.clone();

    let sample = web::block(move || -> Result<String, AppError> {
        let conn = DbPool::get(&pool);
        DatasetRepository::new(&conn, &schema_for_block).sample()
    }).await;

    match sample {
        Ok(Ok(sample_json)) => {
            let body = serde_json::json!({
                "name":    schema.name,
                "columns": schema.columns,
                "sample":  serde_json::from_str::<serde_json::Value>(&sample_json)
                    .unwrap_or(serde_json::Value::Null),
            });
            HttpResponse::Ok().json(body)
        }
        Ok(Err(e)) => e.error_response(),
        Err(e) => {
            log::error!("Thread-pool error: {e}");
            HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "internal error" }))
        }
    }
}

#[post("/api/datasets/{name}/query")]
pub async fn query_dataset(
    reg:  web::Data<Arc<Registry>>,
    path: web::Path<String>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let name      = path.into_inner();
    let page      = body.page.max(1);
    let page_size = body.page_size.clamp(1, 1000);
    let req       = body.into_inner();

    let schema = match reg.get(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let pool = reg.pool.clone();

    let result = web::block(move || -> Result<String, AppError> {
        let conn = DbPool::get(&pool);
        DatasetRepository::new(&conn, &schema).query(&req)
    }).await;

    match result {
        Ok(Ok(arr)) => {
            let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
            HttpResponse::Ok().content_type("application/json").body(body)
        }
        Ok(Err(e)) => e.error_response(),
        Err(e) => {
            log::error!("Thread-pool error: {e}");
            HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "internal error" }))
        }
    }
}

#[post("/api/datasets/{name}/count")]
pub async fn count_dataset(
    reg:  web::Data<Arc<Registry>>,
    path: web::Path<String>,
    body: Option<web::Json<CountRequest>>,
) -> HttpResponse {
    let name = path.into_inner();
    let req  = body.map(|b| b.into_inner()).unwrap_or_default();

    let schema = match reg.get(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let pool = reg.pool.clone();

    let result = web::block(move || -> Result<i64, AppError> {
        let conn = DbPool::get(&pool);
        DatasetRepository::new(&conn, &schema).count(&req.predicates)
    }).await;

    match result {
        Ok(Ok(n)) => HttpResponse::Ok().json(serde_json::json!({ "count": n })),
        Ok(Err(e)) => e.error_response(),
        Err(e) => {
            log::error!("Thread-pool error: {e}");
            HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "internal error" }))
        }
    }
}

/// Admin endpoint: rebuild a dataset from disk and atomically swap it in.
/// Requires `X-Admin-Token` matching `$ADMIN_TOKEN`. Disabled if the env var
/// is unset.
#[post("/api/datasets/{name}/reload")]
pub async fn reload_dataset(
    req:  HttpRequest,
    reg:  web::Data<Arc<Registry>>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = admin::require_admin(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    match reg.reload(&name).await {
        Ok(stats) => HttpResponse::Ok().json(serde_json::json!({
            "dataset":    name,
            "rows":       stats.rows,
            "elapsed_ms": stats.elapsed_ms,
        })),
        Err(e) => e.error_response(),
    }
}
