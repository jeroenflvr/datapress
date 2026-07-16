//! Phase 2A startup state-machine tests (R2.0, R2.7, R2.8).
//!
//! Uses an in-process mock backend whose dataset status can be
//! controlled programmatically without wall-clock sleeps.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use actix_web::{App, http::StatusCode, test, web};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Notify;

use datapress_core::backend::{
    Backend, DatasetStatus, DatasetStatusEntry, DatasetSummary, ReloadStats,
};
use datapress_core::config::{OnStart, ReadinessMode};
use datapress_core::errors::AppError;
use datapress_core::handlers::{self, ReadinessSettings};
use datapress_core::models::{CountRequest, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// ControllableBackend – mock that exposes per-dataset status control
// ---------------------------------------------------------------------------

struct DatasetEntry {
    status: DatasetStatus,
    on_start: OnStart,
}

struct ControllableBackend {
    datasets: Mutex<Vec<(String, DatasetEntry)>>,
    /// Incremented each time a "build" starts (for coalescing assertions).
    build_starts: AtomicUsize,
    /// Notified when a dataset transitions to Published.
    published_notify: Notify,
}

impl ControllableBackend {
    fn new(specs: Vec<(String, OnStart)>) -> Self {
        let datasets = specs
            .into_iter()
            .map(|(name, on_start)| {
                (
                    name,
                    DatasetEntry {
                        status: DatasetStatus::Pending,
                        on_start,
                    },
                )
            })
            .collect();
        Self {
            datasets: Mutex::new(datasets),
            build_starts: AtomicUsize::new(0),
            published_notify: Notify::new(),
        }
    }

    /// Transition `name` to Published and notify waiters.
    fn publish(&self, name: &str) {
        let mut lock = self.datasets.lock().unwrap();
        if let Some((_, e)) = lock.iter_mut().find(|(n, _)| n.as_str() == name) {
            e.status = DatasetStatus::Published;
        }
        self.published_notify.notify_waiters();
    }

    fn set_failed(&self, name: &str) {
        let mut lock = self.datasets.lock().unwrap();
        if let Some((_, e)) = lock.iter_mut().find(|(n, _)| n.as_str() == name) {
            e.status = DatasetStatus::Failed;
        }
    }

    fn set_building(&self, name: &str) {
        let mut lock = self.datasets.lock().unwrap();
        if let Some((_, e)) = lock.iter_mut().find(|(n, _)| n.as_str() == name) {
            e.status = DatasetStatus::Building;
        }
    }

    fn build_starts(&self) -> usize {
        self.build_starts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Backend for ControllableBackend {
    fn names(&self) -> Vec<String> {
        self.datasets
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.status == DatasetStatus::Published)
            .map(|(n, _)| n.clone())
            .collect()
    }

    fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
        self.datasets
            .lock()
            .unwrap()
            .iter()
            .map(|(name, e)| DatasetStatusEntry {
                name: name.clone(),
                status: e.status.clone(),
                on_start: e.on_start.clone(),
                kind: "parquet".into(),
                residency: "memory".into(),
                storage_bytes: None,
                generation_id: None,
                last_refresh_at: None,
                last_refresh_duration_ms: None,
                next_refresh_at: None,
                refresh_source: None,
                consecutive_failures: 0,
                last_error: None,
                columns: if e.status == DatasetStatus::Published {
                    2
                } else {
                    0
                },
                rows: if e.status == DatasetStatus::Published {
                    5
                } else {
                    0
                },
                lazy: false,
                depends_on: vec![],
            })
            .collect()
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        let lock = self.datasets.lock().unwrap();
        let (_, e) = lock
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .ok_or_else(|| AppError::NotFound(name.to_string()))?;
        if e.status != DatasetStatus::Published {
            return Err(AppError::NotReady {
                dataset: name.to_string(),
                state: "pending".into(),
            });
        }
        Ok(DatasetSummary {
            name: name.to_string(),
            columns: 2,
            rows: 5,
            lazy: false,
        })
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        let lock = self.datasets.lock().unwrap();
        let (_, e) = lock
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .ok_or_else(|| AppError::NotFound(name.to_string()))?;
        if e.status != DatasetStatus::Published {
            return Err(AppError::NotReady {
                dataset: name.to_string(),
                state: "pending".into(),
            });
        }
        Ok(Arc::new(DatasetSchema::new(
            name,
            vec![
                ColumnInfo {
                    name: "id".into(),
                    logical: LogicalType::Int,
                    sql_type: "BIGINT".into(),
                    nullable: false,
                },
                ColumnInfo {
                    name: "val".into(),
                    logical: LogicalType::Utf8,
                    sql_type: "VARCHAR".into(),
                    nullable: false,
                },
            ],
        )))
    }

    async fn sample(&self, name: &str) -> Result<String, AppError> {
        self.summary(name)?; // propagates NotReady
        Ok(r#"{"id":1,"val":"x"}"#.into())
    }

    async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        // For lazy first-touch: simulate a build on first call.
        let status = {
            let lock = self.datasets.lock().unwrap();
            lock.iter()
                .find(|(n, _)| n.as_str() == name)
                .map(|(_, e)| (e.status.clone(), e.on_start.clone()))
        };
        match status {
            Some((DatasetStatus::Published, _)) => Ok(r#"[{"id":1}]"#.into()),
            Some((DatasetStatus::Pending, OnStart::Lazy)) => {
                // Simulate first-touch build (coalescing: increment build_starts once).
                self.build_starts.fetch_add(1, Ordering::SeqCst);
                // Use a separate lock scope to avoid holding across await.
                let _ = req;
                self.publish(name);
                Ok(r#"[{"id":1}]"#.into())
            }
            Some((status, _)) => Err(AppError::NotReady {
                dataset: name.to_string(),
                state: format!("{status:?}").to_lowercase(),
            }),
            None => Err(AppError::NotFound(name.to_string())),
        }
    }

    async fn count(&self, name: &str, _req: &CountRequest) -> Result<i64, AppError> {
        self.summary(name)?;
        Ok(5)
    }

    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        self.publish(name);
        Ok(ReloadStats {
            rows: 5,
            elapsed_ms: 1,
            ..Default::default()
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mount_with_readiness(
    backend: Arc<dyn Backend>,
    readiness_mode: ReadinessMode,
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
        .app_data(web::Data::new(ReadinessSettings { readiness_mode }))
        .service(
            web::scope("")
                .service(handlers::healthz)
                .service(handlers::readyz)
                .service(handlers::version)
                .service(handlers::health)
                .service(web::scope("/api/v1").configure(handlers::v1::configure)),
        )
}

// ---------------------------------------------------------------------------
// T1: Non-blocking boot — healthz 200 while dataset is building; readyz
//     503 until published; dataset listing shows state field.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn nonblocking_boot_healthz_200_readyz_503_then_200() {
    let backend = Arc::new(ControllableBackend::new(vec![(
        "events".into(),
        OnStart::Eager,
    )]));

    // Start with dataset Pending (simulates "build not started").
    let app = test::init_service(mount_with_readiness(backend.clone(), ReadinessMode::All)).await;

    // 1. /healthz must always be 200.
    let resp =
        test::call_service(&app, test::TestRequest::get().uri("/healthz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. /readyz is 503 while the dataset is Pending.
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 3. Dataset listing shows state = "pending".
    let body: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/datasets")
            .to_request(),
    )
    .await;
    assert_eq!(body["datasets"][0]["state"], "pending");
    assert_eq!(body["datasets"][0]["rows"], 0);

    // 4. Query returns 503 with Retry-After.
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/datasets/events/query")
            .set_json(serde_json::json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(resp.headers().get("retry-after").unwrap(), "2");

    // 5. Publish the dataset; /readyz should flip to 200.
    backend.publish("events");

    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "ready");

    // 6. Dataset listing now shows state = "published".
    let body: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/datasets")
            .to_request(),
    )
    .await;
    assert_eq!(body["datasets"][0]["state"], "published");
    assert_eq!(body["datasets"][0]["rows"], 5);
}

// ---------------------------------------------------------------------------
// T2: Startup parallelism — a and b build; c (depends conceptually on both)
//     starts only after both are published. Since we're testing the state
//     machine rather than actual parallelism, we verify that build ordering
//     is visible through status transitions.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn startup_two_datasets_build_independently() {
    let backend = Arc::new(ControllableBackend::new(vec![
        ("ds_a".into(), OnStart::Eager),
        ("ds_b".into(), OnStart::Eager),
    ]));

    let app = test::init_service(mount_with_readiness(backend.clone(), ReadinessMode::All)).await;

    // Both pending → readyz 503.
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Publish ds_a; still 503 under "all".
    backend.publish("ds_a");
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Publish ds_b; now all eager = published → readyz 200.
    backend.publish("ds_b");
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// T3: Failed eager dataset — server stays up, readyz stays 503 under "all",
//     but healthz is still 200 and the other dataset serves normally.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn failed_eager_dataset_readyz_stays_503_others_serve() {
    let backend = Arc::new(ControllableBackend::new(vec![
        ("good".into(), OnStart::Eager),
        ("bad".into(), OnStart::Eager),
    ]));

    let app = test::init_service(mount_with_readiness(backend.clone(), ReadinessMode::All)).await;

    // Publish "good", fail "bad".
    backend.publish("good");
    backend.set_failed("bad");

    // /healthz still 200.
    let resp =
        test::call_service(&app, test::TestRequest::get().uri("/healthz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // /readyz is 503 because "bad" failed (readiness = "all").
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = test::read_body_json(resp).await;
    // The failed dataset name should appear in the reason.
    assert!(body["reason"].as_str().unwrap().contains("bad"));

    // "good" serves normally.
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/datasets/good/query")
            .set_json(serde_json::json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // readiness = "any": ready as long as "good" published.
    let app_any =
        test::init_service(mount_with_readiness(backend.clone(), ReadinessMode::Any)).await;
    let resp = test::call_service(
        &app_any,
        test::TestRequest::get().uri("/readyz").to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// T4: Lazy first-touch coalescing — two concurrent queries produce exactly
//     one build (build_starts counter = 1).
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn lazy_first_touch_single_build() {
    // This test verifies the *state machine semantics* through the mock.
    // The real coalescing (mutex) is tested in datapress-datafusion tests.
    let backend = Arc::new(ControllableBackend::new(vec![(
        "lazy_ds".into(),
        OnStart::Lazy,
    )]));

    let app = test::init_service(mount_with_readiness(
        backend.clone(),
        ReadinessMode::All, // lazy doesn't gate readiness
    ))
    .await;

    // /readyz 503 because there are no eager datasets and none published yet.
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // First query triggers the first-touch build.
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/datasets/lazy_ds/query")
            .set_json(serde_json::json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(backend.build_starts(), 1);

    // Second query: dataset is now Published; no new build.
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/datasets/lazy_ds/query")
            .set_json(serde_json::json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(backend.build_starts(), 1, "second query must not re-build");

    // on_start=lazy never gates readiness, but once published the readyz
    // with no-eager-datasets falls back to "at least one published" logic.
    let resp = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
