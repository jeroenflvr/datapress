//! Integration tests for the explorer UI: exercises `GET /explore/` to
//! assert that runtime-created datasets (managed = true) appear in the
//! discovery list and carry the correct `data-managed` markup, and disappear
//! after `DELETE /api/v1/queries/{name}`.
//!
//! Requires the `explorer` cargo feature.  The test is compiled only when
//! that feature is active (see `#[cfg(feature = "explorer")]` below).

#![cfg(feature = "explorer")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use actix_web::{App, http::StatusCode, test, web};
use async_trait::async_trait;

use datapress_core::backend::{
    Backend, DatasetStatus, DatasetStatusEntry, DatasetSummary, RefreshRecord, ReloadStats,
};
use datapress_core::config::{DatasetConfig, IndexConfig, OnStart, SourceConfig, SourceKind};
use datapress_core::errors::AppError;
use datapress_core::explorer::ExplorerState;
use datapress_core::handlers::{self, SavedQueriesSettings};
use datapress_core::models::CountRequest;
use datapress_core::refresh::TtlHandle;
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// Minimal mock backend
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ExplorerEntry {
    managed: bool,
    temp: bool,
}

struct ExplorerMockBackend {
    datasets: RwLock<HashMap<String, ExplorerEntry>>,
    refresh_records: RwLock<HashMap<String, RefreshRecord>>,
}

impl ExplorerMockBackend {
    fn new() -> Arc<Self> {
        let mut m = HashMap::new();
        // One config-file dataset (not managed).
        m.insert(
            "base".into(),
            ExplorerEntry {
                managed: false,
                temp: false,
            },
        );
        Arc::new(Self {
            datasets: RwLock::new(m),
            refresh_records: RwLock::new(HashMap::new()),
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
impl Backend for ExplorerMockBackend {
    fn names(&self) -> Vec<String> {
        self.datasets.read().unwrap().keys().cloned().collect()
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
        let map = self.datasets.read().unwrap();
        if map.contains_key(name) {
            Ok(Self::schema_for(name))
        } else {
            Err(AppError::NotFound(name.into()))
        }
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
                depends_on: vec![],
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
        self.datasets.write().unwrap().insert(
            name.clone(),
            ExplorerEntry {
                managed: cfg.managed,
                temp: cfg.temp,
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

const ADMIN_TOKEN: &str = "explorer-test-token";

fn with_admin(req: actix_web::test::TestRequest) -> actix_web::test::TestRequest {
    req.insert_header(("X-Admin-Token", ADMIN_TOKEN))
}

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

    let explorer_state = web::Data::new(ExplorerState {
        backend: backend.clone(),
        // Config snapshot is initially empty — runtime datasets have no config entry.
        datasets: RwLock::new(vec![]),
        explorer_base: "/explore".into(),
        api_base: "/api/v1".into(),
        backend_label: "Mock".into(),
        sql_enabled: false,
        docs_url: "https://docs.datap-rs.org".into(),
        swagger_url: None,
        oauth2: None,
        environment: None,
        environment_color: None,
        queries_enabled: true,
        storage_backend: None,
    });

    App::new()
        .app_data(web::Data::new(backend))
        .app_data(web::Data::new(handlers::BuildInfo::new("Mock")))
        .app_data(web::Data::new(SavedQueriesSettings {
            dir: None,
            enabled: true,
        }))
        .app_data(web::Data::new(ttl_handle))
        .service(
            web::scope("")
                .service(handlers::healthz)
                .service(web::scope("/api/v1").configure(handlers::v1::configure))
                .configure(|c| datapress_core::explorer::configure(explorer_state.clone(), c)),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// After a `POST /api/v1/queries` succeeds, the explorer index page HTML
/// must contain the dataset name and `data-managed="true"` markup.
#[actix_web::test]
async fn explorer_shows_runtime_dataset_after_create() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = ExplorerMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    // Create a runtime dataset.
    let req = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "runtime_ds",
            "sql": "SELECT x FROM base",
            "kind": "temp"
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "create should succeed");

    // Fetch the explorer index page.
    let index_req = test::TestRequest::get().uri("/explore/").to_request();
    let index_resp = test::call_service(&app, index_req).await;
    assert_eq!(index_resp.status(), StatusCode::OK);
    let body = test::read_body(index_resp).await;
    let html = std::str::from_utf8(&body).expect("UTF-8 body");

    // The dataset name must appear as a data-name attribute in a list row.
    assert!(
        html.contains("data-name=\"runtime_ds\""),
        "explorer index must contain runtime dataset list row; got:\n{html}"
    );
    // The row must carry data-managed="true" so delete actions are wired up.
    assert!(
        html.contains("data-managed=\"true\""),
        "explorer index must contain data-managed=\"true\" for managed dataset; got:\n{html}"
    );
}

/// After `DELETE /api/v1/queries/{name}`, the explorer index page must NOT
/// contain the dataset name or its `data-managed` markup.
#[actix_web::test]
async fn explorer_hides_runtime_dataset_after_delete() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = ExplorerMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    // Create and then delete.
    let create = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "ephemeral",
            "sql": "SELECT x FROM base",
            "kind": "temp"
        }))
        .to_request();
    let cr = test::call_service(&app, create).await;
    assert_eq!(cr.status(), StatusCode::OK);

    let delete =
        with_admin(test::TestRequest::delete().uri("/api/v1/queries/ephemeral")).to_request();
    let dr = test::call_service(&app, delete).await;
    assert_eq!(dr.status(), StatusCode::OK);

    // Fetch the explorer index.
    let index_req = test::TestRequest::get().uri("/explore/").to_request();
    let index_resp = test::call_service(&app, index_req).await;
    assert_eq!(index_resp.status(), StatusCode::OK);
    let body = test::read_body(index_resp).await;
    let html = std::str::from_utf8(&body).expect("UTF-8 body");

    // "ephemeral" must not appear in a dataset row (data-name attribute).
    // Note: the string "ephemeral" also appears in the template as the text
    // "(ephemeral)" in the kind-selector option, so we check for the
    // data-name attribute form which is dataset-specific.
    assert!(
        !html.contains("data-name=\"ephemeral\""),
        "explorer index must NOT contain deleted dataset row; got:\n{html}"
    );
}

/// The config-file dataset `base` (not managed) must appear in the list
/// without `data-managed="true"`.
#[actix_web::test]
async fn explorer_shows_config_dataset_not_managed() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = ExplorerMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    let index_req = test::TestRequest::get().uri("/explore/").to_request();
    let index_resp = test::call_service(&app, index_req).await;
    assert_eq!(index_resp.status(), StatusCode::OK);
    let body = test::read_body(index_resp).await;
    let html = std::str::from_utf8(&body).expect("UTF-8 body");

    // "base" must appear as a list row (data-name attribute).
    assert!(
        html.contains("data-name=\"base\""),
        "config dataset must appear in explorer list"
    );
    // Its row must carry data-managed="false" (not "true").
    // The template emits `data-managed="{{ d.is_managed }}"` which is
    // "false" for config datasets.
    assert!(
        !html.contains("data-managed=\"true\""),
        "config dataset must NOT have data-managed=\"true\"; got:\n{html}"
    );
}

/// The detail partial for a runtime-created dataset must return 200 and
/// contain the schema table markup (the column `x`).
#[actix_web::test]
async fn explorer_detail_returns_200_for_runtime_dataset() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = ExplorerMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    // Create a runtime dataset via the saved-queries API.
    let create = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "detail_ds",
            "sql": "SELECT x FROM base",
            "kind": "temp"
        }))
        .to_request();
    let cr = test::call_service(&app, create).await;
    assert_eq!(cr.status(), StatusCode::OK, "create should succeed");

    // Fetch the detail partial.
    let detail_req = test::TestRequest::get()
        .uri("/explore/datasets/detail_ds")
        .to_request();
    let detail_resp = test::call_service(&app, detail_req).await;
    assert_eq!(
        detail_resp.status(),
        StatusCode::OK,
        "detail route must return 200 for a runtime dataset"
    );
    let body = test::read_body(detail_resp).await;
    let html = std::str::from_utf8(&body).expect("UTF-8 body");

    // Schema table must be present (the column name `x`).
    assert!(
        html.contains(">x<"),
        "detail partial must include column name 'x' in schema table; got:\n{html}"
    );
    // The delete button must be present (is_managed = true).
    assert!(
        html.contains("dpDeleteDataset"),
        "detail partial must include delete button for managed dataset; got:\n{html}"
    );
}

/// After DELETE the detail route must return 404 (the dataset no longer
/// exists in the backend).
#[actix_web::test]
async fn explorer_detail_returns_404_after_delete() {
    datapress_core::admin::init(Some(ADMIN_TOKEN));
    let backend = ExplorerMockBackend::new();
    let app = test::init_service(make_app(backend)).await;

    // Create then delete.
    let create = with_admin(test::TestRequest::post().uri("/api/v1/queries"))
        .set_json(serde_json::json!({
            "name": "gone_ds",
            "sql": "SELECT x FROM base",
            "kind": "temp"
        }))
        .to_request();
    test::call_service(&app, create).await;

    let delete =
        with_admin(test::TestRequest::delete().uri("/api/v1/queries/gone_ds")).to_request();
    let dr = test::call_service(&app, delete).await;
    assert_eq!(dr.status(), StatusCode::OK);

    // Detail must now 404.
    let detail_req = test::TestRequest::get()
        .uri("/explore/datasets/gone_ds")
        .to_request();
    let detail_resp = test::call_service(&app, detail_req).await;
    assert_eq!(
        detail_resp.status(),
        StatusCode::NOT_FOUND,
        "detail route must return 404 after dataset is deleted"
    );
}
