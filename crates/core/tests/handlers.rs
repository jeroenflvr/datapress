//! Integration tests for the shared HTTP handlers.
//!
//! A small in-memory mock `Backend` implementation is mounted under the
//! actix-web test runtime. The tests then exercise the public route
//! surface: liveness/readiness probes, dataset listing, schema,
//! query (JSON + Arrow IPC content negotiation), count, and the
//! admin-guarded reload endpoint.

use std::sync::{Arc, Mutex};

use actix_web::{App, http::StatusCode, test, web};
use arrow::array::{Array, Int32Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use async_trait::async_trait;
use serde_json::Value;

use datapress_core::backend::{Backend, DatasetSummary, ReloadStats};
use datapress_core::errors::AppError;
use datapress_core::handlers;
use datapress_core::models::{CountRequest, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------- mock ----

#[derive(Default)]
struct Calls {
    reload: usize,
}

struct MockBackend {
    /// Empty registry simulates "no datasets loaded yet".
    empty: bool,
    calls: Mutex<Calls>,
}

impl MockBackend {
    fn new() -> Self {
        Self {
            empty: false,
            calls: Mutex::default(),
        }
    }
    fn empty() -> Self {
        Self {
            empty: true,
            calls: Mutex::default(),
        }
    }

    fn schema_obj() -> Arc<DatasetSchema> {
        Arc::new(DatasetSchema::new(
            "people",
            vec![
                ColumnInfo {
                    name: "id".into(),
                    logical: LogicalType::Int,
                    sql_type: "BIGINT".into(),
                    nullable: false,
                },
                ColumnInfo {
                    name: "name".into(),
                    logical: LogicalType::Utf8,
                    sql_type: "VARCHAR".into(),
                    nullable: false,
                },
            ],
        ))
    }
}

#[async_trait]
impl Backend for MockBackend {
    fn names(&self) -> Vec<String> {
        if self.empty {
            vec![]
        } else {
            vec!["people".into()]
        }
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        if name == "people" {
            Ok(DatasetSummary {
                name: name.into(),
                columns: 2,
                rows: 5,
            })
        } else {
            Err(AppError::NotFound(format!("dataset '{name}' not found")))
        }
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        if name == "people" {
            Ok(Self::schema_obj())
        } else {
            Err(AppError::NotFound(format!("dataset '{name}' not found")))
        }
    }

    async fn sample(&self, _name: &str) -> Result<String, AppError> {
        Ok(r#"{"id":1,"name":"Anna"}"#.into())
    }

    async fn query(&self, _name: &str, _req: &QueryRequest) -> Result<String, AppError> {
        Ok(r#"[{"id":1,"name":"Anna"},{"id":2,"name":"Bob"}]"#.into())
    }

    async fn query_arrow(&self, _name: &str, _req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        let schema = ArrowSchema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, false),
        ]);
        let ids = Int32Array::from(vec![1, 2]);
        let names = StringArray::from(vec!["Anna", "Bob"]);
        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![Arc::new(ids), Arc::new(names)],
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
        let mut buf = Vec::new();
        {
            let mut w = StreamWriter::try_new(&mut buf, &schema)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            w.write(&batch)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            w.finish().map_err(|e| AppError::Internal(e.to_string()))?;
        }
        Ok(buf)
    }

    async fn count(&self, _name: &str, req: &CountRequest) -> Result<i64, AppError> {
        // Make the test count depend on whether predicates were sent so we
        // can distinguish the two cases below.
        Ok(if req.predicates.is_empty() { 5 } else { 3 })
    }

    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        if name != "people" {
            return Err(AppError::NotFound(name.into()));
        }
        self.calls.lock().unwrap().reload += 1;
        Ok(ReloadStats {
            rows: 5,
            elapsed_ms: 1,
        })
    }
}

// --------------------------------------------------------------- helpers --

fn mount(
    backend: Arc<dyn Backend>,
) -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        .app_data(web::Data::new(backend))
        .app_data(web::Data::new(handlers::BuildInfo::new("Mock")))
        .service(handlers::healthz)
        .service(handlers::readyz)
        .service(handlers::version)
        .service(handlers::health)
        // Canonical versioned scope.
        .service(web::scope("/api/v1").configure(handlers::v1::configure))
        // Legacy alias kept for back-compat (tested below).
        .service(web::scope("/api").configure(handlers::v1::configure))
}

// ----------------------------------------------------------------- tests --

#[actix_web::test]
async fn healthz_always_ok() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/healthz").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[actix_web::test]
async fn readyz_503_when_no_datasets() {
    let app = test::init_service(mount(Arc::new(MockBackend::empty()))).await;
    let req = test::TestRequest::get().uri("/readyz").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[actix_web::test]
async fn readyz_200_with_dataset_count() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/readyz").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["datasets"], 1);
}

#[actix_web::test]
async fn version_returns_build_info() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/version").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["name"], "datapress-core");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["backend"], "Mock");
    // `profile` is "debug" under `cargo test` but assert it's set.
    assert!(body["profile"].is_string());
}

#[actix_web::test]
async fn list_datasets_returns_summaries() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/api/datasets").to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    let ds = &body["datasets"];
    assert_eq!(ds[0]["name"], "people");
    assert_eq!(ds[0]["columns"], 2);
    assert_eq!(ds[0]["rows"], 5);
}

#[actix_web::test]
async fn schema_returns_columns_and_sample() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/datasets/people/schema")
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["name"], "people");
    assert_eq!(body["rows"], 5);
    assert_eq!(body["columns"][0]["name"], "id");
    // Default Backend::indexed_columns impl returns an empty list.
    assert_eq!(body["indexed"], serde_json::json!([]));
    assert_eq!(body["sample"]["id"], 1);
    assert_eq!(body["sample"]["name"], "Anna");
}

#[actix_web::test]
async fn schema_unknown_dataset_returns_404() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/datasets/nope/schema")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn query_json_envelope() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/query")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("application/json"));
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 1000);
    assert_eq!(body["data"][0]["name"], "Anna");
}

#[actix_web::test]
async fn query_arrow_via_accept_header() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/query")
        .insert_header(("Accept", "application/vnd.apache.arrow.stream"))
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/vnd.apache.arrow.stream",
    );
    let bytes = test::read_body(resp).await;
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes.to_vec()), None).unwrap();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let ids = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap();
    assert_eq!(ids.values(), &[1, 2]);
}

#[actix_web::test]
async fn query_arrow_via_format_query_param() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/query?format=arrow")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/vnd.apache.arrow.stream",
    );
}

#[actix_web::test]
async fn count_with_and_without_predicates() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;

    let req = test::TestRequest::post()
        .uri("/api/datasets/people/count")
        .set_json(serde_json::json!({}))
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["count"], 5);

    let req = test::TestRequest::post()
        .uri("/api/datasets/people/count")
        .set_json(serde_json::json!({
            "predicates": [{"col": "name", "op": "eq", "value": "Anna"}],
        }))
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["count"], 3);
}

#[actix_web::test]
async fn reload_requires_admin_token() {
    // ADMIN_TOKEN unset (default in test process) → admin endpoints are
    // disabled and return 403 regardless of headers.
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/reload")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
async fn arbitrary_accept_does_not_force_arrow() {
    // `*/*` should still go through the JSON path.
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/query")
        .insert_header(("Accept", "*/*"))
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("application/json"));
}

// ------------------------------------------------------------- v1 routing --
//
// Every route above is also reachable under the canonical `/api/v1/...`
// scope. The existing tests target the legacy `/api/...` alias so we
// keep regression coverage on both mount points.

#[actix_web::test]
async fn v1_list_datasets() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets")
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["datasets"][0]["name"], "people");
}

#[actix_web::test]
async fn v1_schema() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets/people/schema")
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["name"], "people");
}

#[actix_web::test]
async fn v1_query_json_and_arrow() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;

    // JSON envelope.
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query")
        .set_json(serde_json::json!({}))
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["data"][0]["name"], "Anna");

    // Arrow IPC via query param.
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query?format=arrow")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/vnd.apache.arrow.stream",
    );
}

#[actix_web::test]
async fn v1_count_and_reload_guard() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;

    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/count")
        .set_json(serde_json::json!({}))
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["count"], 5);

    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/reload")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
