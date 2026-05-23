use std::sync::Arc;

use actix_web::{HttpResponse, ResponseError, get, post, web};

use crate::duckdb_backend::db::{DbPool, Registry};
use crate::duckdb_backend::repository::DatasetRepository;
use crate::errors::AppError;
use crate::models::QueryRequest;

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

#[get("/api/datasets")]
pub async fn list_datasets(reg: web::Data<Arc<Registry>>) -> HttpResponse {
    let summaries: Vec<_> = reg.names().into_iter().map(|n| {
        let s = &reg.datasets[n];
        serde_json::json!({
            "name":    s.name,
            "columns": s.columns.len(),
        })
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
        Ok(s)  => s.clone(),
        Err(e) => return e.error_response(),
    };
    let pool = reg.pool.clone();

    let sample = web::block(move || -> Result<String, AppError> {
        let conn = DbPool::get(&pool);
        DatasetRepository::new(&conn, &schema).sample()
    }).await;

    match sample {
        Ok(Ok(sample_json)) => {
            let s = reg.get(&name).unwrap();
            let body = serde_json::json!({
                "name":    s.name,
                "columns": s.columns,
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
        Ok(s)  => s.clone(),
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
