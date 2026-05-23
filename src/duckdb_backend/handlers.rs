use actix_web::{get, post, web, HttpResponse, ResponseError};

use crate::duckdb_backend::db::{DbPool, DbPoolRef};
use crate::duckdb_backend::repository::AccidentsRepository;
use crate::models::{PaginationParams, QueryRequest};

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

#[get("/api/accidents")]
pub async fn get_accidents(
    db: web::Data<DbPoolRef>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let page      = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(100).clamp(1, 1000);
    let state     = params.state.clone();
    let severity  = params.severity;
    let city      = params.city.clone();
    let pool      = db.as_ref().clone();

    let result = web::block(move || {
        let conn = DbPool::get(&pool);
        AccidentsRepository::new(&conn).get_page(
            page,
            page_size,
            state.as_deref(),
            severity,
            city.as_deref(),
        )
    })
    .await;

    respond(result, page, page_size)
}

#[post("/api/accidents/query")]
pub async fn query_accidents(
    db: web::Data<DbPoolRef>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let page      = body.page.max(1);
    let page_size = body.page_size.clamp(1, 1000);
    let req       = body.into_inner();
    let pool      = db.as_ref().clone();

    let result = web::block(move || {
        let conn = DbPool::get(&pool);
        AccidentsRepository::new(&conn).query(&req)
    })
    .await;

    respond(result, page, page_size)
}

// ---------------------------------------------------------------------------
// Shared response builder
// ---------------------------------------------------------------------------

fn respond(
    result: Result<Result<String, crate::errors::AppError>, actix_web::error::BlockingError>,
    page: u64,
    page_size: u64,
) -> HttpResponse {
    match result {
        Ok(Ok(arr)) => {
            let body =
                format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
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
