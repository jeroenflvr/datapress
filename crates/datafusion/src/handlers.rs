use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, ResponseError, get, post, web};

use datapress_core::admin;
use crate::store::Store;
use datapress_core::models::QueryRequest;

#[get("/health")]
pub async fn health() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

#[get("/api/datasets")]
pub async fn list_datasets(state: web::Data<Arc<Store>>) -> HttpResponse {
    let summaries: Vec<_> = state.names().into_iter().filter_map(|n| {
        state.dataset(&n).ok().map(|st| serde_json::json!({
            "name":    st.schema.name,
            "columns": st.schema.columns.len(),
            "rows":    st.data.num_rows(),
        }))
    }).collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": summaries }))
}

#[get("/api/datasets/{name}/schema")]
pub async fn get_schema(
    state: web::Data<Arc<Store>>,
    path:  web::Path<String>,
) -> HttpResponse {
    let name = path.into_inner();
    let st = match state.dataset(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let sample = match state.sample(&name) {
        Ok(s)  => s,
        Err(e) => return e.error_response(),
    };
    let body = format!(
        r#"{{"name":{name_lit},"columns":{cols},"sample":{sample}}}"#,
        name_lit = serde_json::to_string(&st.schema.name).unwrap(),
        cols     = serde_json::to_string(&st.schema.columns).unwrap(),
    );
    HttpResponse::Ok().content_type("application/json").body(body)
}

#[post("/api/datasets/{name}/query")]
pub async fn query_dataset(
    state: web::Data<Arc<Store>>,
    path:  web::Path<String>,
    body:  web::Json<QueryRequest>,
) -> HttpResponse {
    let name      = path.into_inner();
    let page      = body.page.max(1);
    let page_size = body.page_size.clamp(1, 1000);
    let req       = body.into_inner();

    match state.query(&name, &req).await {
        Ok(arr) => {
            let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
            HttpResponse::Ok().content_type("application/json").body(body)
        }
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: rebuild a dataset from disk and atomically swap it in.
/// Requires `X-Admin-Token` matching `$ADMIN_TOKEN`. Disabled if the env var
/// is unset.
#[post("/api/datasets/{name}/reload")]
pub async fn reload_dataset(
    req:   HttpRequest,
    state: web::Data<Arc<Store>>,
    path:  web::Path<String>,
) -> HttpResponse {
    if let Err(e) = admin::require_admin(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    match state.reload(&name).await {
        Ok(stats) => HttpResponse::Ok().json(serde_json::json!({
            "dataset":    name,
            "rows":       stats.rows,
            "elapsed_ms": stats.elapsed_ms,
        })),
        Err(e) => e.error_response(),
    }
}
