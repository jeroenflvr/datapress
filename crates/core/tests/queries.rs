//! Integration tests for the Phase 6 saved-queries API.
//!
//! Tests mount a controllable in-memory backend and exercise:
//! - POST /api/v1/queries (create, inference, 409 conflict, reserved name, auth)
//! - GET  /api/v1/queries (list)
//! - DELETE /api/v1/queries/{name} (delete, 403 config dataset, 409 dependents)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use actix_web::{App, http::StatusCode, test, web};
use async_trait::async_trait;
use serde_json::Value;

use datapress_core::backend::{
    Backend, DatasetStatus, DatasetStatusEntry, DatasetSummary, RefreshRecord, ReloadStats,
};
use datapress_core::config::{DatasetConfig, IndexConfig, OnStart, SourceConfig, SourceKind};
use datapress_core::errors::AppError;
use datapress_core::handlers::{self, SavedQueriesSettings};
use datapress_core::models::CountRequest;
use datapress_core::refresh::TtlHandle;
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// Mock backend for queries-API tests
// ---------------------------------------------------------------------------

/// Entry tracked per registered dataset.
#[derive(Clone)]
struct MockEntry {
    schema: Arc<DatasetSchema>,
    managed: bool,
    temp: bool,
    depends_on: Vec<String>,
    is_config: bool, // if true, NOT managed (simulate config-file dataset)
}

struct QueryMockBackend {
    datasets: RwLock<HashMap<String, MockEntry>>,
    last_registered: Mutex<Option<DatasetConfig>>,
    /// Tracks RefreshRecords so record_reload_failure / record_refresh work.
    refresh_records: RwLock<HashMap<String, RefreshRecord>>,
    /// Counts try_reload / reload calls (for async-wave tests).
    build_count: std::sync::atomic::AtomicUsize,
}

impl QueryMockBackend {
    fn new() -> Arc<Self> {
        let mut m = HashMap::new();
        // Seed with a config-defined "parquet" dataset (not managed).
        let schema = Arc::new(DatasetSchema::new(
            "base",
            vec![
                ColumnInfo {
                    name: "id".into(),
                    logical: LogicalType::Int,
                    sql_type: "BIGINT".into(),
                    nullable: false,
                },
                ColumnInfo {
                    name: "val".into(),
                    logical: LogicalType::Float,
                    sql_type: "DOUBLE".into(),
                    nullable: false,
                },
            ],
        ));
        m.insert(
            "base".into(),
            MockEntry {
                schema,
                managed: false,
                temp: false,
                depends_on: vec![],
                is_config: true,
            },
        );
        Arc::new(Self {
            datasets: RwLock::new(m),
            last_registered: Mutex::new(None),
            refresh_records: RwLock::new(HashMap::new()),
            build_count: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn schema_for(name: &str) -> Arc<DatasetSchema> {
        Arc::new(DatasetSchema::new(
            name,
            vec![ColumnInfo {
                name: "x".into(),
                logical: LogicalType::Int,
                sql_type: "BIGINT".into(),
                nullable: false,
            }],
        ))
    }
}

#[async_trait]
impl Backend for QueryMockBackend {
    fn names(&self) -> Vec<String> {
        self.datasets
            .read()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.is_config || e.managed)
            .map(|(n, _)| n.clone())
            .collect()
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        let map = self.datasets.read().unwrap();
        if map.contains_key(name) {
            Ok(DatasetSummary {
                name: name.into(),
                columns: 1,
                rows: 0,
                lazy: false,
            })
        } else {
            Err(AppError::NotFound(name.into()))
        }
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        self.datasets
            .read()
            .unwrap()
            .get(name)
            .map(|e| e.schema.clone())
            .ok_or_else(|| AppError::NotFound(name.into()))
    }

    async fn sample(&self, _: &str) -> Result<String, AppError> {
        Ok("null".into())
    }
    async fn query(
        &self,
        _: &str,
        _: &datapress_core::models::QueryRequest,
    ) -> Result<String, AppError> {
        Ok("[]".into())
    }
    async fn count(&self, _: &str, _: &CountRequest) -> Result<i64, AppError> {
        Ok(0)
    }
    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        if self.datasets.read().unwrap().contains_key(name) {
            self.build_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ReloadStats {
                rows: 0,
                elapsed_ms: 1,
                ..Default::default()
            })
        } else {
            Err(AppError::NotFound(name.into()))
        }
    }

    fn refresh_record(&self, name: &str) -> Option<RefreshRecord> {
        self.refresh_records.read().unwrap().get(name).cloned()
    }

    fn record_refresh(&self, name: &str, record: RefreshRecord) {
        self.refresh_records
            .write()
            .unwrap()
            .insert(name.to_string(), record);
    }

    fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
        self.datasets
            .read()
            .unwrap()
            .iter()
            .map(|(n, e)| DatasetStatusEntry {
                name: n.clone(),
                status: DatasetStatus::Published,
                on_start: OnStart::Eager,
                kind: if e.managed { "query" } else { "parquet" }.into(),
                residency: "memory".into(),
                storage_bytes: None,
                generation_id: None,
                last_refresh_at: None,
                last_refresh_duration_ms: None,
                next_refresh_at: None,
                refresh_source: None,
                consecutive_failures: 0,
                last_error: None,
                columns: 1,
                rows: 0,
                lazy: false,
                depends_on: e.depends_on.clone(),
            })
            .collect()
    }

    async fn register(&self, cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        let name = cfg.name.clone();
        {
            let map = self.datasets.read().unwrap();
            if map.contains_key(&name) {
                return Err(AppError::InvalidValue(format!(
                    "dataset '{name}' already exists"
                )));
            }
        }
        let depends_on = cfg.source.depends_on.clone();
        let managed = cfg.managed;
        let temp = cfg.temp;
        *self.last_registered.lock().unwrap() = Some(cfg);
        self.datasets.write().unwrap().insert(
            name.clone(),
            MockEntry {
                schema: Self::schema_for(&name),
                managed,
                temp,
                depends_on,
                is_config: false,
            },
        );
        Ok(DatasetSummary {
            name,
            columns: 1,
            rows: 0,
            lazy: false,
        })
    }

    fn is_managed(&self, name: &str) -> bool {
        self.datasets
            .read()
            .unwrap()
            .get(name)
            .map(|e| e.managed)
            .unwrap_or(false)
    }

    fn is_temp(&self, name: &str) -> bool {
        self.datasets
            .read()
            .unwrap()
            .get(name)
            .map(|e| e.managed && e.temp)
            .unwrap_or(false)
    }

    async fn unregister(&self, name: &str) -> Result<(), AppError> {
        let mut map = self.datasets.write().unwrap();
        match map.get(name) {
            None => return Err(AppError::NotFound(name.into())),
            Some(e) if !e.managed => {
                return Err(AppError::Forbidden(format!("'{name}' is a config dataset")));
            }
            _ => {}
        }
        map.remove(name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test app factory
// ---------------------------------------------------------------------------

/// Build a test app with the queries API enabled (ADMIN_TOKEN=test).
fn make_app(
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
    let (ttl_tx, _ttl_rx) = tokio::sync::mpsc::unbounded_channel();
    let ttl_handle = TtlHandle::new(ttl_tx);

    App::new()
        .app_data(web::Data::new(backend))
        .app_data(web::Data::new(handlers::BuildInfo::new("Mock")))
        .app_data(web::Data::new(SavedQueriesSettings {
            dir: None, // no file persistence in tests
            enabled: true,
        }))
        .app_data(web::Data::new(ttl_handle))
        .service(
            web::scope("")
                .service(handlers::healthz)
                .service(web::scope("/api/v1").configure(handlers::v1::configure)),
        )
}

/// Same but with queries API disabled.
fn make_app_disabled(
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
    let (ttl_tx, _ttl_rx) = tokio::sync::mpsc::unbounded_channel();
    let ttl_handle = TtlHandle::new(ttl_tx);

    App::new()
        .app_data(web::Data::new(backend))
        .app_data(web::Data::new(handlers::BuildInfo::new("Mock")))
        .app_data(web::Data::new(SavedQueriesSettings {
            dir: None,
            enabled: false, // routes return 404
        }))
        .app_data(web::Data::new(ttl_handle))
        .service(web::scope("").service(web::scope("/api/v1").configure(handlers::v1::configure)))
}

/// Admin-token header value for tests.
const ADMIN_TOKEN: &str = "test-token";

fn with_admin(req: actix_web::test::TestRequest) -> actix_web::test::TestRequest {
    req.insert_header(("X-Admin-Token", ADMIN_TOKEN))
}

// ---------------------------------------------------------------------------
// Tests: POST /api/v1/queries
// ---------------------------------------------------------------------------

/// Querying "SELECT * FROM base" should infer depends_on = ["base"].
#[actix_web::test]
async fn create_temp_infers_depends_on() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend.clone())).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "derived",
            "sql": "SELECT id FROM base",
            "kind": "temp"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "expected 200");
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["name"], "derived");
    assert_eq!(body["kind"], "temp");
    let deps = body["depends_on"].as_array().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0], "base");
}

/// Creating a dataset with an unknown table in SQL → 400.
#[actix_web::test]
async fn create_unknown_table_returns_400() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "bad",
            "sql": "SELECT * FROM nonexistent_table",
            "kind": "temp"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Creating a dataset with the reserved name "reload-all" → 400.
#[actix_web::test]
async fn create_reserved_name_rejected() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "reload-all",
            "sql": "SELECT id FROM base",
            "kind": "temp"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Conflict when a dataset of the same name already exists.
#[actix_web::test]
async fn create_conflict_returns_400() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    // First creation
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(
            serde_json::json!({ "name": "derived", "sql": "SELECT id FROM base", "kind": "temp" }),
        )
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Second creation with same name
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(
            serde_json::json!({ "name": "derived", "sql": "SELECT id FROM base", "kind": "temp" }),
        )
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "expected 400 on conflict"
    );
}

/// Conflict when trying to create a dataset whose name matches a config-defined one.
#[actix_web::test]
async fn create_conflict_with_config_dataset_returns_400() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(
            serde_json::json!({ "name": "base", "sql": "SELECT id FROM base", "kind": "temp" }),
        )
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "expected 400 conflict with existing"
    );
}

/// Missing / wrong admin token → 403.
#[actix_web::test]
async fn create_without_token_returns_403() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = test::TestRequest::post()
        .uri("/api/v1/queries")
        .set_json(serde_json::json!({ "name": "x", "sql": "SELECT id FROM base", "kind": "temp" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Routes return 404 when queries API is disabled.
#[actix_web::test]
async fn routes_404_when_disabled() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app_disabled(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({ "name": "x", "sql": "SELECT id FROM base" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let req = with_admin(test::TestRequest::get().uri("/api/v1/queries")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let req = with_admin(test::TestRequest::delete().uri("/api/v1/queries/x")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Tests: GET /api/v1/queries
// ---------------------------------------------------------------------------

/// List shows only managed datasets.
#[actix_web::test]
async fn list_shows_only_managed() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend.clone())).await;

    // Create a managed dataset
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({ "name": "managed_one", "sql": "SELECT id FROM base", "kind": "temp" }))
        .to_request();
    test::call_service(&app, req).await;

    let req = with_admin(test::TestRequest::get().uri("/api/v1/queries")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let arr = body.as_array().unwrap();
    // "base" (config) should NOT appear; "managed_one" should.
    assert!(
        arr.iter().any(|e| e["name"] == "managed_one"),
        "expected managed_one in list"
    );
    assert!(
        arr.iter().all(|e| e["name"] != "base"),
        "config dataset 'base' should not appear"
    );
}

// ---------------------------------------------------------------------------
// Tests: DELETE /api/v1/queries/{name}
// ---------------------------------------------------------------------------

/// Delete a managed dataset succeeds.
#[actix_web::test]
async fn delete_managed_dataset_succeeds() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend.clone())).await;

    // Create
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({ "name": "to_delete", "sql": "SELECT id FROM base", "kind": "temp" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Delete
    let req = with_admin(test::TestRequest::delete().uri("/api/v1/queries/to_delete")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Should be gone
    assert!(!backend.datasets.read().unwrap().contains_key("to_delete"));
}

/// Delete a config-defined dataset returns 403.
#[actix_web::test]
async fn delete_config_dataset_returns_403() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::delete().uri("/api/v1/queries/base")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Delete a dataset that has dependents returns 409.
#[actix_web::test]
async fn delete_with_dependents_returns_409() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend.clone())).await;

    // Create upstream
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(
            serde_json::json!({ "name": "upstream", "sql": "SELECT id FROM base", "kind": "temp" }),
        )
        .to_request();
    test::call_service(&app, req).await;

    // Manually insert a dependent that lists "upstream" in depends_on
    backend.datasets.write().unwrap().insert(
        "downstream".into(),
        MockEntry {
            schema: QueryMockBackend::schema_for("downstream"),
            managed: true,
            temp: true,
            depends_on: vec!["upstream".into()],
            is_config: false,
        },
    );

    // Try to delete upstream → 409
    let req = with_admin(test::TestRequest::delete().uri("/api/v1/queries/upstream")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let body: Value = test::read_body_json(resp).await;
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("downstream"),
        "error should name the dependent"
    );
}

/// Delete unknown dataset returns 404.
#[actix_web::test]
async fn delete_unknown_returns_404() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req =
        with_admin(test::TestRequest::delete().uri("/api/v1/queries/nonexistent")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Delete without admin token → 403.
#[actix_web::test]
async fn delete_without_token_returns_403() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = test::TestRequest::delete()
        .uri("/api/v1/queries/base")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Tests: TTL scheduling
// ---------------------------------------------------------------------------

/// A `temp` dataset with a TTL schedules expiry. We test that the TtlHandle
/// receives the schedule call (we can't easily wait for actual expiry in
/// unit tests; end-to-end TTL is covered by the scheduler tests).
#[actix_web::test]
async fn create_with_ttl_schedules_expiry() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();

    // Capture TTL events
    let (ttl_tx, mut ttl_rx) =
        tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, String)>();
    let ttl_handle = TtlHandle::new(ttl_tx);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(backend.clone() as Arc<dyn Backend>))
            .app_data(web::Data::new(handlers::BuildInfo::new("Mock")))
            .app_data(web::Data::new(SavedQueriesSettings {
                dir: None,
                enabled: true,
            }))
            .app_data(web::Data::new(ttl_handle))
            .service(
                web::scope("").service(web::scope("/api/v1").configure(handlers::v1::configure)),
            ),
    )
    .await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "ephemeral",
            "sql": "SELECT id FROM base",
            "kind": "temp",
            "ttl": "1h"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A TTL event should have been sent to the channel.
    let evt = ttl_rx.try_recv();
    assert!(evt.is_ok(), "expected a TTL event to be sent");
    let (_, name) = evt.unwrap();
    assert_eq!(name, "ephemeral");
}

// ---------------------------------------------------------------------------
// Tests: async=true mode
// ---------------------------------------------------------------------------

/// `?async=true` returns 202 immediately with state=building.
#[actix_web::test]
async fn create_async_returns_202() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries?async=true"))
        .set_json(serde_json::json!({
            "name": "async_ds",
            "sql": "SELECT id FROM base",
            "kind": "temp"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["state"], "building");
}

// ---------------------------------------------------------------------------
// Tests: POST /api/v1/datasets/reload-all  (R8.11)
// ---------------------------------------------------------------------------

/// reload-all returns 202 with enqueued list containing all published datasets.
#[actix_web::test]
async fn reload_all_returns_202_with_enqueued() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    // Seed a second config dataset
    backend.datasets.write().unwrap().insert(
        "other".into(),
        MockEntry {
            schema: QueryMockBackend::schema_for("other"),
            managed: false,
            temp: false,
            depends_on: vec![],
            is_config: true,
        },
    );
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    assert!(body["enqueued"].is_array());
    assert!(body["skipped"].is_array());
    let enqueued: Vec<String> = body["enqueued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(enqueued.contains(&"base".to_string()));
    assert!(enqueued.contains(&"other".to_string()));
}

/// reload-all without admin token returns 403.
#[actix_web::test]
async fn reload_all_requires_auth() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let req = test::TestRequest::post()
        .uri("/api/v1/datasets/reload-all")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// The reserved name "reload-all" cannot be created as a dataset.
/// (The name is already rejected by config validation; here we verify the
/// route itself is not ambiguous with a dataset named "reload-all".)
#[actix_web::test]
async fn reload_all_route_is_not_ambiguous_with_dataset_name() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    // Manually insert "reload-all" as a dataset (bypassing config validation)
    // to prove the route still disambiguates correctly.
    backend.datasets.write().unwrap().insert(
        "reload-all".into(),
        MockEntry {
            schema: QueryMockBackend::schema_for("reload-all"),
            managed: false,
            temp: false,
            depends_on: vec![],
            is_config: true,
        },
    );
    let app = test::init_service(make_app(backend)).await;

    // POST /datasets/reload-all MUST hit the reload-all handler (202),
    // not the per-dataset reload handler.
    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    // 202 Accepted = reload-all handler was reached, not the per-dataset reload
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

/// reload-all respects topological order: a dependent dataset is placed
/// after its dependency in the enqueued list (simulated via depends_on).
#[actix_web::test]
async fn reload_all_topological_order() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = QueryMockBackend::new();
    // Add a query dataset "derived" that depends on "base"
    backend.datasets.write().unwrap().insert(
        "derived".into(),
        MockEntry {
            schema: QueryMockBackend::schema_for("derived"),
            managed: true,
            temp: false,
            depends_on: vec!["base".into()],
            is_config: false,
        },
    );
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    let enqueued: Vec<String> = body["enqueued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // "base" must appear before "derived" in the enqueued list.
    let base_idx = enqueued.iter().position(|n| n == "base");
    let derived_idx = enqueued.iter().position(|n| n == "derived");
    assert!(base_idx.is_some(), "expected 'base' in enqueued");
    assert!(derived_idx.is_some(), "expected 'derived' in enqueued");
    assert!(
        base_idx.unwrap() < derived_idx.unwrap(),
        "'base' must come before 'derived' in the enqueued list"
    );
}

/// A building dataset lands in skipped, not enqueued.
#[actix_web::test]
async fn reload_all_skips_building_datasets() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    // Use a specialised mock that reports "base" as Building.
    struct BuildingMock(Arc<QueryMockBackend>);
    #[async_trait::async_trait]
    impl Backend for BuildingMock {
        fn names(&self) -> Vec<String> {
            self.0.names()
        }
        fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
            self.0.summary(name)
        }
        fn schema(
            &self,
            name: &str,
        ) -> Result<Arc<datapress_core::schema::DatasetSchema>, AppError> {
            self.0.schema(name)
        }
        async fn sample(&self, name: &str) -> Result<String, AppError> {
            self.0.sample(name).await
        }
        async fn query(
            &self,
            name: &str,
            req: &datapress_core::models::QueryRequest,
        ) -> Result<String, AppError> {
            self.0.query(name, req).await
        }
        async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
            self.0.reload(name).await
        }
        async fn count(
            &self,
            name: &str,
            req: &datapress_core::models::CountRequest,
        ) -> Result<i64, AppError> {
            self.0.count(name, req).await
        }
        fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
            self.0
                .dataset_statuses()
                .into_iter()
                .map(|mut e| {
                    if e.name == "base" {
                        e.status = DatasetStatus::Building;
                    }
                    e
                })
                .collect()
        }
    }
    let inner = QueryMockBackend::new();
    let backend: Arc<dyn Backend> = Arc::new(BuildingMock(inner));
    let app = test::init_service(make_app(backend)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    let skipped: Vec<String> = body["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        skipped.contains(&"base".to_string()),
        "building dataset must be skipped"
    );
}

// ---------------------------------------------------------------------------
// Tests: R8.11 async wave contract + failure recording
// ---------------------------------------------------------------------------

/// Verify the wave response returns BEFORE builds complete.
///
/// Uses a semaphore-gated mock: try_reload waits for a permit so the wave task
/// blocks after returning 202. We assert build_count == 0 immediately after
/// the 202, then release the semaphore and verify the build runs.
#[actix_web::test]
async fn reload_all_response_before_builds_complete() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));

    // Semaphore starts at 0 — wave task will block waiting for a permit.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate_for_mock = Arc::clone(&gate);
    let build_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let build_count_for_mock = Arc::clone(&build_count);

    struct GatedMock {
        inner: Arc<QueryMockBackend>,
        gate: Arc<tokio::sync::Semaphore>,
        build_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Backend for GatedMock {
        fn names(&self) -> Vec<String> {
            self.inner.names()
        }
        fn summary(&self, n: &str) -> Result<DatasetSummary, AppError> {
            self.inner.summary(n)
        }
        fn schema(&self, n: &str) -> Result<Arc<DatasetSchema>, AppError> {
            self.inner.schema(n)
        }
        async fn sample(&self, n: &str) -> Result<String, AppError> {
            self.inner.sample(n).await
        }
        async fn query(
            &self,
            n: &str,
            r: &datapress_core::models::QueryRequest,
        ) -> Result<String, AppError> {
            self.inner.query(n, r).await
        }
        async fn count(&self, n: &str, r: &CountRequest) -> Result<i64, AppError> {
            self.inner.count(n, r).await
        }
        async fn reload(&self, n: &str) -> Result<ReloadStats, AppError> {
            self.inner.reload(n).await
        }
        async fn try_reload(&self, n: &str) -> Result<Option<ReloadStats>, AppError> {
            // Block until the test releases the gate.
            let _permit = self.gate.acquire().await.unwrap();
            self.build_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(ReloadStats {
                rows: 0,
                elapsed_ms: 1,
                ..Default::default()
            }))
        }
        fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
            self.inner.dataset_statuses()
        }
    }

    let inner = QueryMockBackend::new();
    let gated: Arc<dyn Backend> = Arc::new(GatedMock {
        inner,
        gate: gate_for_mock,
        build_count: build_count_for_mock,
    });

    let app = test::init_service(make_app(gated)).await;

    // Fire reload-all — should return 202 immediately even though gate is 0.
    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // Build has NOT happened yet (gate is still 0).
    assert_eq!(
        build_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "build must not have run before 202 returned"
    );

    // Release the gate — allow the wave task to proceed.
    gate.add_permits(10);
    // Yield to the tokio runtime so the spawned wave task runs.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    assert!(
        build_count.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "build must have run after gate released"
    );
}

/// Wave task respects topological order: 'derived' is built after 'base'.
/// After the wave completes, build count does not increase further
/// (no cascade engine in mock backend, so debounce cannot fire extra builds).
#[actix_web::test]
async fn reload_all_wave_build_order_and_no_extra_builds() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));

    let build_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let build_order_for_mock = Arc::clone(&build_order);

    struct OrderMock {
        inner: Arc<QueryMockBackend>,
        order: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Backend for OrderMock {
        fn names(&self) -> Vec<String> {
            self.inner.names()
        }
        fn summary(&self, n: &str) -> Result<DatasetSummary, AppError> {
            self.inner.summary(n)
        }
        fn schema(&self, n: &str) -> Result<Arc<DatasetSchema>, AppError> {
            self.inner.schema(n)
        }
        async fn sample(&self, n: &str) -> Result<String, AppError> {
            self.inner.sample(n).await
        }
        async fn query(
            &self,
            n: &str,
            r: &datapress_core::models::QueryRequest,
        ) -> Result<String, AppError> {
            self.inner.query(n, r).await
        }
        async fn count(&self, n: &str, r: &CountRequest) -> Result<i64, AppError> {
            self.inner.count(n, r).await
        }
        async fn reload(&self, n: &str) -> Result<ReloadStats, AppError> {
            self.inner.reload(n).await
        }
        async fn try_reload(&self, n: &str) -> Result<Option<ReloadStats>, AppError> {
            self.order.lock().unwrap().push(n.to_string());
            Ok(Some(ReloadStats {
                rows: 0,
                elapsed_ms: 1,
                ..Default::default()
            }))
        }
        fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
            self.inner.dataset_statuses()
        }
    }

    let inner = QueryMockBackend::new();
    // Add "derived" which depends on "base"
    inner.datasets.write().unwrap().insert(
        "derived".into(),
        MockEntry {
            schema: QueryMockBackend::schema_for("derived"),
            managed: true,
            temp: false,
            depends_on: vec!["base".into()],
            is_config: false,
        },
    );

    let om: Arc<dyn Backend> = Arc::new(OrderMock {
        inner,
        order: build_order_for_mock,
    });
    let app = test::init_service(make_app(om)).await;

    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // Yield until the wave task completes.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }

    let order = build_order.lock().unwrap().clone();
    let base_pos = order.iter().position(|n| n == "base");
    let derived_pos = order.iter().position(|n| n == "derived");
    assert!(base_pos.is_some(), "base must be built");
    assert!(derived_pos.is_some(), "derived must be built");
    assert!(
        base_pos.unwrap() < derived_pos.unwrap(),
        "base must be built before derived"
    );
    let total = order.len();

    // Advance paused time well past the debounce window (5 s).
    // With no cascade engine in the mock, the build count must not grow.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    // Yield again — no extra builds should be scheduled.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();

    assert_eq!(
        build_order.lock().unwrap().len(),
        total,
        "no extra builds after debounce window (mock has no cascade engine)"
    );
}

/// Failed builds inside the wave are reflected in /status:
/// consecutive_failures and last_error must be set.
#[actix_web::test]
async fn reload_all_wave_failure_reflected_in_status() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));

    struct FailMock {
        inner: Arc<QueryMockBackend>,
        fail_name: &'static str,
    }

    #[async_trait::async_trait]
    impl Backend for FailMock {
        fn names(&self) -> Vec<String> {
            self.inner.names()
        }
        fn summary(&self, n: &str) -> Result<DatasetSummary, AppError> {
            self.inner.summary(n)
        }
        fn schema(&self, n: &str) -> Result<Arc<DatasetSchema>, AppError> {
            self.inner.schema(n)
        }
        async fn sample(&self, n: &str) -> Result<String, AppError> {
            self.inner.sample(n).await
        }
        async fn query(
            &self,
            n: &str,
            r: &datapress_core::models::QueryRequest,
        ) -> Result<String, AppError> {
            self.inner.query(n, r).await
        }
        async fn count(&self, n: &str, r: &CountRequest) -> Result<i64, AppError> {
            self.inner.count(n, r).await
        }
        async fn reload(&self, n: &str) -> Result<ReloadStats, AppError> {
            self.inner.reload(n).await
        }
        async fn try_reload(&self, n: &str) -> Result<Option<ReloadStats>, AppError> {
            if n == self.fail_name {
                Err(AppError::Internal(format!("synthetic failure for {n}")))
            } else {
                Ok(Some(ReloadStats {
                    rows: 0,
                    elapsed_ms: 1,
                    ..Default::default()
                }))
            }
        }
        fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
            self.inner.dataset_statuses()
        }
        fn refresh_record(&self, name: &str) -> Option<RefreshRecord> {
            self.inner.refresh_record(name)
        }
        fn record_refresh(&self, name: &str, record: RefreshRecord) {
            self.inner.record_refresh(name, record);
        }
    }

    let inner = QueryMockBackend::new();
    let fail_backend = Arc::new(FailMock {
        inner: Arc::clone(&inner),
        fail_name: "base",
    });
    let fail_backend_arc: Arc<dyn Backend> = fail_backend;

    let app = test::init_service(make_app(Arc::clone(&fail_backend_arc))).await;

    // Fire reload-all; "base" will fail inside the wave task.
    let req = with_admin(test::TestRequest::post().uri("/api/v1/datasets/reload-all")).to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // "base" appears in enqueued (classified at snapshot, before builds).
    let body: Value = test::read_body_json(resp).await;
    let enqueued: Vec<String> = body["enqueued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        enqueued.contains(&"base".to_string()),
        "failed dataset appears in enqueued (classification is pre-build)"
    );

    // Yield for the wave task to run and call record_reload_failure.
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }

    // Check the refresh record via the inner QueryMockBackend.
    let rec = inner.refresh_record("base");
    assert!(rec.is_some(), "refresh record must exist after failure");
    let rec = rec.unwrap();
    assert_eq!(
        rec.consecutive_failures, 1,
        "consecutive_failures must be 1"
    );
    assert!(rec.last_error.is_some(), "last_error must be set");
    assert!(
        rec.last_error
            .as_deref()
            .unwrap_or("")
            .contains("synthetic failure"),
        "last_error must contain the error message"
    );
}
