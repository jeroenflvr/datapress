//! Integration tests for the shared HTTP handlers.
//!
//! A small in-memory mock `Backend` implementation is mounted under the
//! actix-web test runtime. The tests then exercise the public route
//! surface: liveness/readiness probes, dataset listing, schema,
//! query (JSON + Arrow IPC content negotiation), count, and the
//! admin-guarded reload endpoint.

use std::sync::{Arc, Mutex, RwLock};

use actix_web::{App, http::StatusCode, test, web};
use arrow::array::{Array, Int32Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use async_trait::async_trait;
use serde_json::Value;

use datapress_core::backend::{
    Backend, DatasetStatus, DatasetStatusEntry, DatasetSummary, RefreshRecord, RefreshSource,
    ReloadStats,
};
use datapress_core::config::OnStart;
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
    /// Per-dataset refresh records (T5.1 / T5.2).
    refresh_records: RwLock<std::collections::HashMap<String, RefreshRecord>>,
}

impl MockBackend {
    fn new() -> Self {
        let mut records = std::collections::HashMap::new();
        records.insert(
            "people".into(),
            RefreshRecord {
                last_refresh_at: Some("2024-01-01T00:00:00Z".into()),
                last_refresh_duration_ms: Some(5),
                refresh_source: Some(RefreshSource::Startup),
                consecutive_failures: 0,
                last_error: None,
                ..Default::default()
            },
        );
        Self {
            empty: false,
            calls: Mutex::default(),
            refresh_records: RwLock::new(records),
        }
    }
    fn empty() -> Self {
        Self {
            empty: true,
            calls: Mutex::default(),
            refresh_records: RwLock::new(std::collections::HashMap::new()),
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
                lazy: false,
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
        // Update the refresh record so tests can verify the header changes.
        {
            let mut recs = self.refresh_records.write().unwrap();
            let rec = recs.entry("people".into()).or_default();
            rec.last_refresh_at = Some("2024-06-01T12:00:00Z".into());
            rec.last_refresh_duration_ms = Some(1);
            rec.refresh_source = Some(RefreshSource::Manual);
        }
        Ok(ReloadStats {
            rows: 5,
            elapsed_ms: 1,
            ..Default::default()
        })
    }

    fn refresh_record(&self, name: &str) -> Option<RefreshRecord> {
        self.refresh_records.read().unwrap().get(name).cloned()
    }

    fn record_refresh(&self, name: &str, record: RefreshRecord) {
        let mut map = self.refresh_records.write().unwrap();
        let existing = map.entry(name.to_string()).or_default();
        existing.consecutive_failures = record.consecutive_failures;
        if record.last_error.is_some() {
            existing.last_error = record.last_error;
        }
        if record.next_refresh_at.is_some() {
            existing.next_refresh_at = record.next_refresh_at;
        }
        if record.refresh_source.is_some() {
            existing.refresh_source = record.refresh_source;
        }
        if record.last_refresh_at.is_some() {
            existing.last_refresh_at = record.last_refresh_at;
        }
        if record.last_refresh_duration_ms.is_some() {
            existing.last_refresh_duration_ms = record.last_refresh_duration_ms;
        }
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
        .service(
            web::scope("")
                .service(handlers::healthz)
                .service(handlers::readyz)
                .service(handlers::version)
                .service(handlers::health)
                // Canonical versioned scope — the only API mount.
                .service(web::scope("/api/v1").configure(handlers::v1::configure)),
        )
}

fn mount_prefixed(
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
        .service(
            web::scope("/pre")
                .service(handlers::healthz)
                .service(handlers::readyz)
                .service(handlers::version)
                .service(handlers::health)
                .service(web::scope("/api/v1").configure(handlers::v1::configure)),
        )
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

// -------------------------------------------------------- prefix tests --

#[actix_web::test]
async fn prefixed_probes_answer_under_prefix() {
    let app = test::init_service(mount_prefixed(Arc::new(MockBackend::new()))).await;

    // Prefixed paths respond.
    for path in ["/pre/healthz", "/pre/readyz", "/pre/version", "/pre/health"] {
        let resp = test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK, "expected 200 at {path}");
    }
}

#[actix_web::test]
async fn unprefixed_probes_404_when_prefix_set() {
    let app = test::init_service(mount_prefixed(Arc::new(MockBackend::new()))).await;

    // Bare (un-prefixed) paths must NOT be reachable.
    for path in ["/healthz", "/readyz", "/version"] {
        let resp = test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "expected 404 at bare {path} when prefix=/pre"
        );
    }
}

#[actix_web::test]
async fn prefixed_api_accessible_under_prefix() {
    let app = test::init_service(mount_prefixed(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/pre/api/v1/datasets")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[actix_web::test]
async fn list_datasets_returns_summaries() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets")
        .to_request();
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
        .uri("/api/v1/datasets/people/schema")
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
        .uri("/api/v1/datasets/nope/schema")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn query_json_envelope() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query")
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
        .uri("/api/v1/datasets/people/query")
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
async fn query_stream_returns_arrow_without_paging_envelope() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query/stream")
        .set_json(serde_json::json!({"columns": ["id", "name"]}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/vnd.apache.arrow.stream",
    );
    assert_eq!(resp.headers().get("x-query-mode").unwrap(), "stream");
    assert!(resp.headers().get("x-page").is_none());

    let bytes = test::read_body(resp).await;
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes.to_vec()), None).unwrap();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches[0].num_rows(), 2);
}

#[actix_web::test]
async fn count_with_and_without_predicates() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;

    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/count")
        .set_json(serde_json::json!({}))
        .to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(body["count"], 5);

    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/count")
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
        .uri("/api/v1/datasets/people/reload")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
async fn arbitrary_accept_does_not_force_arrow() {
    // `*/*` should still go through the JSON path.
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query")
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

// --------------------------------------------------------- 404 on removed legacy paths --

#[actix_web::test]
async fn legacy_api_datasets_returns_404() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/api/datasets").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn legacy_api_dataset_query_returns_404() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/datasets/people/query")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ----------------------------------------------------------------- Phase 5 tests --

// ---------------------------------------------------------------------------
// T5.1: GET /api/v1/datasets/{name}/status — state machine + field coverage
// ---------------------------------------------------------------------------

/// A backend whose dataset-status can be set programmatically for state-machine
/// tests.
struct StateMachineBackend {
    status: Mutex<DatasetStatus>,
    refresh_rec: Mutex<RefreshRecord>,
}

impl StateMachineBackend {
    fn with_status(status: DatasetStatus) -> Self {
        let rec = if status == DatasetStatus::Published {
            RefreshRecord {
                last_refresh_at: Some("2024-03-01T10:00:00Z".into()),
                last_refresh_duration_ms: Some(42),
                refresh_source: Some(RefreshSource::Startup),
                consecutive_failures: 0,
                last_error: None,
                ..Default::default()
            }
        } else if status == DatasetStatus::Failed {
            RefreshRecord {
                last_refresh_at: Some("2024-03-01T09:00:00Z".into()),
                consecutive_failures: 1,
                last_error: Some("build error: source not found".into()),
                ..Default::default()
            }
        } else {
            RefreshRecord::default()
        };
        Self {
            status: Mutex::new(status),
            refresh_rec: Mutex::new(rec),
        }
    }
}

#[async_trait]
impl Backend for StateMachineBackend {
    fn names(&self) -> Vec<String> {
        if *self.status.lock().unwrap() == DatasetStatus::Published {
            vec!["state_ds".into()]
        } else {
            vec![]
        }
    }

    fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
        let status = self.status.lock().unwrap().clone();
        let rec = self.refresh_rec.lock().unwrap().clone();
        vec![DatasetStatusEntry {
            name: "state_ds".into(),
            status: status.clone(),
            on_start: OnStart::Eager,
            kind: "query".into(),
            residency: "memory".into(),
            storage_bytes: None,
            generation_id: rec.generation_id,
            last_refresh_at: rec.last_refresh_at,
            last_refresh_duration_ms: rec.last_refresh_duration_ms,
            next_refresh_at: rec.next_refresh_at,
            refresh_source: rec.refresh_source,
            consecutive_failures: rec.consecutive_failures,
            last_error: rec.last_error,
            columns: if status == DatasetStatus::Published {
                2
            } else {
                0
            },
            rows: if status == DatasetStatus::Published {
                10
            } else {
                0
            },
            lazy: false,
            depends_on: vec!["upstream".into()],
        }]
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        if name == "state_ds" && *self.status.lock().unwrap() == DatasetStatus::Published {
            Ok(DatasetSummary {
                name: name.into(),
                columns: 2,
                rows: 10,
                lazy: false,
            })
        } else {
            Err(AppError::NotFound(name.into()))
        }
    }

    fn schema(&self, name: &str) -> Result<Arc<datapress_core::schema::DatasetSchema>, AppError> {
        Err(AppError::NotFound(name.into()))
    }

    async fn sample(&self, _name: &str) -> Result<String, AppError> {
        Ok("null".into())
    }

    async fn query(&self, _name: &str, _req: &QueryRequest) -> Result<String, AppError> {
        Ok("[]".into())
    }

    async fn count(&self, _name: &str, _req: &CountRequest) -> Result<i64, AppError> {
        Ok(0)
    }

    async fn reload(&self, _name: &str) -> Result<ReloadStats, AppError> {
        Err(AppError::Internal("not implemented".into()))
    }

    fn refresh_record(&self, name: &str) -> Option<RefreshRecord> {
        if name == "state_ds" {
            Some(self.refresh_rec.lock().unwrap().clone())
        } else {
            None
        }
    }
}

#[actix_web::test]
async fn status_endpoint_published() {
    let backend = StateMachineBackend::with_status(DatasetStatus::Published);
    let app = test::init_service(mount(Arc::new(backend))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets/state_ds/status")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["state"], "published");
    assert_eq!(body["kind"], "query");
    assert_eq!(body["residency"], "memory");
    assert_eq!(body["consecutive_failures"], 0);
    assert_eq!(body["rows"], 10);
    assert_eq!(body["last_refresh_duration_ms"], 42);
    assert_eq!(body["refresh_source"], "startup");
    assert_eq!(body["depends_on"], serde_json::json!(["upstream"]));
    assert!(body["last_refresh_at"].is_string());
}

#[actix_web::test]
async fn status_endpoint_pending() {
    let backend = StateMachineBackend::with_status(DatasetStatus::Pending);
    let app = test::init_service(mount(Arc::new(backend))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets/state_ds/status")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["state"], "pending");
    assert_eq!(body["rows"], 0);
}

#[actix_web::test]
async fn status_endpoint_failed_includes_last_error() {
    let backend = StateMachineBackend::with_status(DatasetStatus::Failed);
    let app = test::init_service(mount(Arc::new(backend))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets/state_ds/status")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["state"], "failed");
    assert_eq!(body["consecutive_failures"], 1);
    assert!(body["last_error"].is_string());
}

#[actix_web::test]
async fn status_endpoint_not_found() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get()
        .uri("/api/v1/datasets/nonexistent/status")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// T5.2: X-Dataset-Refreshed-At header on /query and /count
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn query_includes_refreshed_at_header() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/query")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let header = resp.headers().get("x-dataset-refreshed-at");
    assert!(header.is_some(), "X-Dataset-Refreshed-At header missing");
    assert_eq!(header.unwrap().to_str().unwrap(), "2024-01-01T00:00:00Z");
}

#[actix_web::test]
async fn count_includes_refreshed_at_header() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/people/count")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("x-dataset-refreshed-at"));
}

#[actix_web::test]
async fn refreshed_at_header_absent_for_dataset_without_record() {
    // A dataset backend with no refresh records (fresh empty backend) should
    // not emit the header even if the query succeeds.
    struct NoRecordBackend;
    #[async_trait::async_trait]
    impl Backend for NoRecordBackend {
        fn names(&self) -> Vec<String> {
            vec!["ds".into()]
        }
        fn summary(&self, _: &str) -> Result<DatasetSummary, AppError> {
            Ok(DatasetSummary {
                name: "ds".into(),
                columns: 1,
                rows: 0,
                lazy: false,
            })
        }
        fn schema(
            &self,
            name: &str,
        ) -> Result<Arc<datapress_core::schema::DatasetSchema>, AppError> {
            Err(AppError::NotFound(name.into()))
        }
        async fn sample(&self, _: &str) -> Result<String, AppError> {
            Ok("null".into())
        }
        async fn query(&self, _: &str, _: &QueryRequest) -> Result<String, AppError> {
            Ok("[]".into())
        }
        async fn count(&self, _: &str, _: &CountRequest) -> Result<i64, AppError> {
            Ok(0)
        }
        async fn reload(&self, _: &str) -> Result<ReloadStats, AppError> {
            Err(AppError::Internal("no".into()))
        }
        // No refresh_record override → default returns None.
    }

    let app = test::init_service(mount(Arc::new(NoRecordBackend))).await;
    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/ds/query")
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // No refresh record → header absent.
    assert!(!resp.headers().contains_key("x-dataset-refreshed-at"));
}

// ---------------------------------------------------------------------------
// Tests: storage_backend in BuildInfo (R8.10 — version endpoint extension)
// ---------------------------------------------------------------------------

/// The `/version` endpoint does NOT include `storage_backend` when `None`.
#[actix_web::test]
async fn version_no_storage_backend_absent() {
    let app = test::init_service(mount(Arc::new(MockBackend::new()))).await;
    let req = test::TestRequest::get().uri("/version").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    // storage_backend is skip_serializing_if = None → absent from JSON.
    assert!(body.get("storage_backend").is_none());
}

/// BuildInfo::with_storage_backend sets the field, which is then serialised.
#[actix_web::test]
async fn build_info_with_storage_backend() {
    use datapress_core::handlers::BuildInfo;
    let info = BuildInfo::new("Test").with_storage_backend(Some("local".into()));
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["storage_backend"], "local");
}

/// BuildInfo::with_storage_backend(None) omits the field.
#[actix_web::test]
async fn build_info_no_storage_backend_omitted() {
    use datapress_core::handlers::BuildInfo;
    let info = BuildInfo::new("Test").with_storage_backend(None);
    let json = serde_json::to_value(&info).unwrap();
    assert!(json.get("storage_backend").is_none());
}
