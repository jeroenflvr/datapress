use actix_web::{get, post, web, HttpResponse, ResponseError};

use crate::models::{PaginationParams, QueryRequest};
use crate::store::Store;

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

#[get("/api/accidents")]
pub async fn get_accidents(
    state:  web::Data<Store>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let page      = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(100).clamp(1, 1000);

    match state.get_page(page, page_size, params.state.as_deref(), params.severity, params.city.as_deref()) {
        Ok(arr) => json_page(arr, page, page_size),
        Err(e)  => e.error_response(),
    }
}

#[post("/api/accidents/query")]
pub async fn query_accidents(
    state: web::Data<Store>,
    body:  web::Json<QueryRequest>,
) -> HttpResponse {
    let page      = body.page.max(1);
    let page_size = body.page_size.clamp(1, 1000);
    let req       = body.into_inner();

    match state.query(&req).await {
        Ok(arr) => json_page(arr, page, page_size),
        Err(e)  => e.error_response(),
    }
}

fn json_page(arr: String, page: u64, page_size: u64) -> HttpResponse {
    let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
    HttpResponse::Ok().content_type("application/json").body(body)
}
