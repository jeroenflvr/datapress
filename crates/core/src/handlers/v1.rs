//! Version 1 of the dataset HTTP API.
//!
//! Routes (relative to whichever scope the caller mounts this module
//! under — always `/api/v1`):
//!
//! | Method | Path                              | Description                          |
//! |--------|-----------------------------------|--------------------------------------|
//! | GET    | `/datasets`                       | List datasets with summaries         |
//! | POST   | `/datasets`                       | Register a new dataset (admin-only)  |
//! | POST   | `/datasets/persist`               | Append a dataset to config (admin)   |
//! | GET    | `/datasets/{name}/schema`         | Schema + rows + indexed cols + sample |
//! | POST   | `/datasets/{name}/query`          | Query (JSON or Arrow IPC)            |
//! | POST   | `/datasets/{name}/query/stream`   | Stream full query result as Arrow IPC |
//! | POST   | `/datasets/{name}/count`          | Count matching rows                  |
//! | POST   | `/datasets/{name}/reload`         | Rebuild dataset (admin-only)         |
//! | POST   | `/config/reload`                  | Register newly-added datasets (admin) |
//!
//! Handlers are plain `async fn` (not route-macro structs) so the same
//! version can be mounted under multiple scopes.

use actix_web::{HttpRequest, HttpResponse, ResponseError, web};

use crate::admin;
use crate::handlers::{
    ARROW_IPC_MIME, BackendData, PARQUET_MIME, ParquetCache, QueryLimits, SavedQueriesSettings,
    SqlSettings, serve_bytes_with_range, wants_arrow, wants_no_compression,
};
use crate::models::{
    CountRequest, CreateQueryRequest, QueryRequest, SavedQueryEntry, SavedQueryKind, SqlRequest,
};

// -------------------------------------------------------------- auth guards --

/// Enforce the configured `read` scopes when the `auth` feature is on
/// and OIDC enforcement is enabled. When disabled (either at build time
/// or in config) this is a no-op.
#[cfg(feature = "auth")]
fn require_read(req: &HttpRequest) -> Result<(), crate::errors::AppError> {
    use std::sync::Arc;
    if let Some(cfg) = req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>()
        && cfg.enabled
        && !cfg.anonymous_read
    {
        return crate::auth::require_scopes(req, &cfg.read_scopes);
    }
    Ok(())
}
#[cfg(not(feature = "auth"))]
fn require_read(_: &HttpRequest) -> Result<(), crate::errors::AppError> {
    Ok(())
}

/// Allow the request to perform a reload if EITHER the legacy admin
/// token matches OR (when `auth` is enabled) the caller holds the
/// configured reload scopes. The two paths are independent so operators
/// can migrate to OIDC without breaking existing automation.
pub(crate) fn require_reload(req: &HttpRequest) -> Result<(), crate::errors::AppError> {
    #[cfg(feature = "auth")]
    let admin_ok = admin::require_admin(req).is_ok();
    #[cfg(feature = "auth")]
    {
        use std::sync::Arc;
        if let Some(cfg) = req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>()
            && cfg.enabled
        {
            let scope_ok = crate::auth::require_scopes(req, &cfg.reload_scopes).is_ok();
            if admin_ok && cfg.admin_token_fallback {
                return Ok(());
            }
            if scope_ok {
                return Ok(());
            }
            // Neither path satisfied — surface the scope error so
            // the client gets a 401/403 with a Bearer challenge.
            return crate::auth::require_scopes(req, &cfg.reload_scopes);
        }
    }
    // No OIDC layer — fall back to the admin-token check.
    admin::require_admin(req)
}

/// Allow the request to manage datasets (create/delete saved queries).
/// Requires EITHER `X-Admin-Token` OR (when auth is on) the configured
/// `manage_scopes`. If ADMIN_TOKEN is unset AND auth is off, always `Err`
/// so the route returns 404 (R8.6).
pub(crate) fn require_manage(req: &HttpRequest) -> Result<(), crate::errors::AppError> {
    #[cfg(feature = "auth")]
    let admin_ok = admin::require_admin(req).is_ok();
    #[cfg(feature = "auth")]
    {
        use std::sync::Arc;
        if let Some(cfg) = req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>()
            && cfg.enabled
        {
            let scope_ok = crate::auth::require_scopes(req, &cfg.manage_scopes).is_ok();
            if admin_ok && cfg.admin_token_fallback {
                return Ok(());
            }
            if scope_ok {
                return Ok(());
            }
            return crate::auth::require_scopes(req, &cfg.manage_scopes);
        }
    }
    admin::require_admin(req)
}

/// Register every v1 route on the provided actix [`web::ServiceConfig`].
///
/// Call this inside a [`web::scope`] — usually `/api/v1` — so paths come
/// out as `/api/v1/datasets/...`.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/datasets", web::get().to(list_datasets))
        .route("/datasets", web::post().to(register_dataset))
        .route("/datasets/persist", web::post().to(persist_dataset))
        .route("/datasets/{name}/schema", web::get().to(get_schema))
        .route("/datasets/{name}/status", web::get().to(get_dataset_status))
        .route("/datasets/{name}/query", web::post().to(query_dataset))
        .route("/sql", web::post().to(sql_query))
        .route(
            "/datasets/{name}/query/stream",
            web::post().to(stream_dataset),
        )
        .route("/datasets/{name}/count", web::post().to(count_dataset))
        .route("/datasets/{name}/parquet", web::get().to(parquet_dataset))
        .route("/datasets/{name}/parquet", web::head().to(parquet_dataset))
        .route(
            "/datasets/{name}/all.parquet",
            web::get().to(parquet_dataset),
        )
        .route(
            "/datasets/{name}/all.parquet",
            web::head().to(parquet_dataset),
        )
        .route("/datasets/{name}/reload", web::post().to(reload_dataset))
        .route("/datasets/reload-all", web::post().to(reload_all_datasets))
        .route("/config/reload", web::post().to(reload_config))
        // Phase 6: saved-queries API
        .route("/queries", web::post().to(create_query))
        .route("/queries", web::get().to(list_queries))
        .route("/queries/{name}", web::delete().to(delete_query));
}

/// Route table for log_routes-style introspection. Each entry is
/// `(method, path-suffix)` relative to the version's mount scope.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/datasets"),
    ("POST", "/datasets"),
    ("POST", "/datasets/persist"),
    ("GET", "/datasets/{name}/schema"),
    ("GET", "/datasets/{name}/status"),
    ("POST", "/datasets/{name}/query"),
    ("POST", "/sql"),
    ("POST", "/datasets/{name}/query/stream"),
    ("POST", "/datasets/{name}/count"),
    ("GET", "/datasets/{name}/parquet"),
    ("GET", "/datasets/{name}/all.parquet"),
    ("POST", "/datasets/{name}/reload"),
    ("POST", "/datasets/reload-all"),
    ("POST", "/config/reload"),
    ("POST", "/queries"),
    ("GET", "/queries"),
    ("DELETE", "/queries/{name}"),
];

// ---------------------------------------------------------------- handlers --

pub async fn list_datasets(req: HttpRequest, backend: BackendData) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    // Use dataset_statuses() to include all datasets (pending/building/failed
    // as well as published). For published datasets apply the projection-filter
    // column-count adjustment; for others return the placeholder zeroes.
    use crate::backend::DatasetStatus;
    let entries: Vec<_> = backend
        .dataset_statuses()
        .into_iter()
        .map(|mut entry| {
            if entry.status == DatasetStatus::Published {
                // Adjust columns for projection filter.
                if let Ok(schema) = backend.schema(&entry.name)
                    && schema.projection_filter.is_active()
                {
                    entry.columns = schema.visible_columns().len();
                }
            }
            entry
        })
        .collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": entries }))
}

pub async fn get_schema(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    let schema = match backend.schema(&name) {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    let summary = match backend.summary(&name) {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    let indexed = match backend.indexed_columns(&name) {
        Ok(i) => i,
        Err(e) => return e.error_response(),
    };
    let sample = match backend.sample(&name).await {
        Ok(s) => s,
        Err(e) => return e.error_response(),
    };
    // Never reveal projection-hidden columns: filter the schema listing and
    // the indexed-column set, and strip hidden keys out of the row sample.
    let visible = schema.projection_filter.is_active();
    let columns: Vec<_> = schema.visible_columns();
    let indexed: Vec<_> = if visible {
        indexed
            .into_iter()
            .filter(|c| schema.is_visible(c))
            .collect()
    } else {
        indexed
    };
    let sample = if visible {
        strip_hidden_sample(&sample, &schema)
    } else {
        sample
    };
    let body = format!(
        r#"{{"name":{name_lit},"rows":{rows},"columns":{cols},"indexed":{indexed},"sample":{sample}}}"#,
        name_lit = serde_json::to_string(&schema.name).unwrap(),
        rows = summary.rows,
        cols = serde_json::to_string(&columns).unwrap(),
        indexed = serde_json::to_string(&indexed).unwrap(),
    );
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body)
}

/// Remove projection-hidden keys from a `/schema` row sample. The sample is
/// backend-rendered JSON (`"null"` when the dataset is empty); on any parse
/// failure the original string is returned unchanged.
fn strip_hidden_sample(sample: &str, schema: &crate::schema::DatasetSchema) -> String {
    match serde_json::from_str::<serde_json::Value>(sample) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.retain(|k, _| schema.is_visible(k));
            serde_json::Value::Object(map).to_string()
        }
        _ => sample.to_string(),
    }
}

/// `GET /api/v1/datasets/{name}/status` — full per-dataset status entry
/// including refresh observability fields (T5.1).
pub async fn get_dataset_status(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    // Find the entry in the full status list (includes pending/building/failed).
    let entry = backend
        .dataset_statuses()
        .into_iter()
        .find(|e| e.name == name);
    match entry {
        Some(e) => HttpResponse::Ok().json(e),
        None => crate::errors::AppError::NotFound(format!("dataset '{name}' not found"))
            .error_response(),
    }
}

pub async fn query_dataset(
    http: HttpRequest,
    backend: BackendData,
    limits: Option<web::Data<QueryLimits>>,
    path: web::Path<String>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    if let Err(e) = require_read(&http) {
        return e.error_response();
    }
    let name = path.into_inner();
    let max_page_size = limits
        .as_ref()
        .map(|l| l.max_page_size)
        .unwrap_or_else(|| QueryLimits::default().max_page_size)
        .max(1);
    let page = body.page.max(1);
    let page_size = body.page_size.clamp(1, max_page_size);
    let mut req = body.into_inner();
    req.page = page;
    req.page_size = page_size;

    // Apply the dataset's column-level access filters (hidden columns,
    // predicate restrictions) before the backend sees the request. This is
    // the single choke point for every backend and response format.
    // For lazy/pending datasets the schema isn't available until the first
    // build completes — skip access filtering in that case and let the
    // backend's query() handle the lazy first-touch build.
    use crate::errors::AppError;
    match backend.schema(&name) {
        Ok(schema) => {
            if let Err(e) = req.enforce_column_filters(&schema) {
                return e.error_response();
            }
        }
        Err(AppError::NotReady { .. }) | Err(AppError::NotFound(_)) => {
            // Dataset pending or not yet discovered — proceed; the backend
            // will trigger a lazy build or return 503 as appropriate.
        }
        Err(e) => return e.error_response(),
    }

    // Content negotiation: clients opt into Arrow IPC via the `Accept`
    // header or `?format=arrow`. Anything else (including no header)
    // gets the historical JSON envelope.
    if wants_arrow(&http) {
        return match backend.query_arrow_stream(&name, &req).await {
            Ok(stream) => {
                let mut resp = HttpResponse::Ok();
                resp.content_type(ARROW_IPC_MIME)
                    .insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"))
                    .insert_header(("X-Page", page.to_string()))
                    .insert_header(("X-Page-Size", page_size.to_string()));
                if let Some(ts) = backend.refreshed_at(&name) {
                    resp.insert_header(("X-Dataset-Refreshed-At", ts));
                }
                resp.streaming(stream)
            }
            Err(e) => e.error_response(),
        };
    }

    match backend.query(&name, &req).await {
        Ok(arr) => {
            let body = format!(r#"{{"data":{arr},"page":{page},"page_size":{page_size}}}"#);
            let mut resp = HttpResponse::Ok();
            resp.content_type("application/json");
            if wants_no_compression(&http) {
                resp.insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"));
            }
            if let Some(ts) = backend.refreshed_at(&name) {
                resp.insert_header(("X-Dataset-Refreshed-At", ts));
            }
            resp.body(body)
        }
        Err(e) => e.error_response(),
    }
}

/// Raw-SQL endpoint: `POST /api/v1/sql`.
///
/// Accepts a read-only `SELECT` / `WITH … SELECT` or a `DESCRIBE`/`DESC
/// <table>` statement in the request body and runs it against the engine.
/// Disabled unless `[sql].enabled = true`; when off, returns `404` so the
/// endpoint is invisible.
///
/// The statement is parsed and validated by [`crate::sql::validate`], which
/// rejects anything that is not a single read-only query or `DESCRIBE`,
/// references an unknown table or file function, or touches datasets not in
/// the registered allowlist. The result is hard-capped at `[sql].max_rows`
/// rows.
///
/// Like the dataset query endpoint, the response is content-negotiated:
/// clients that send `Accept: application/vnd.apache.arrow.stream` (or
/// `?format=arrow`) get an Arrow IPC stream; everything else gets the
/// JSON `{"data": …, "max_rows": …}` envelope.
pub async fn sql_query(
    http: HttpRequest,
    backend: BackendData,
    settings: Option<web::Data<SqlSettings>>,
    body: web::Json<SqlRequest>,
) -> HttpResponse {
    let settings = settings.as_ref().map(|s| *s.get_ref()).unwrap_or_default();
    // When the endpoint is disabled, behave as if the route does not
    // exist — don't leak its presence or run the auth challenge.
    if !settings.enabled {
        return crate::errors::AppError::NotFound("sql endpoint".into()).error_response();
    }
    if let Err(e) = require_read(&http) {
        return e.error_response();
    }

    // Build the case-insensitive allowlist of registered datasets. Phase 1
    // permits at most one distinct dataset per statement.
    let allowed: std::collections::HashSet<String> = backend
        .names()
        .into_iter()
        .map(|n| n.to_lowercase())
        .collect();

    let validated = match crate::sql::validate(&body.sql, &allowed, allowed.len().max(1)) {
        Ok(v) => v,
        Err(e) => return e.error_response(),
    };

    // Apply each referenced dataset's column-level access filters. Datasets
    // with no active filters are a no-op, so this only costs a schema lookup
    // in the common case.
    for ds in &validated.datasets {
        if let Ok(schema) = backend.schema(ds)
            && let Err(e) = crate::sql::enforce_column_access(&validated.sql, &schema)
        {
            return e.error_response();
        }
    }

    // The effective row cap is the server limit, optionally lowered (never
    // raised) by the request's `max_rows`.
    let max_rows = match body.max_rows {
        Some(req_cap) => req_cap.clamp(1, settings.max_rows),
        None => settings.max_rows,
    };

    // Content negotiation: clients opt into Arrow IPC via the `Accept`
    // header or `?format=arrow`. Anything else (including no header) gets
    // the historical JSON envelope. The Arrow body is itself streamed
    // (schema message + batches + EOS), capped at `max_rows`.
    if wants_arrow(&http) {
        return match backend
            .query_sql_arrow_stream(&validated.sql, &validated.datasets, max_rows)
            .await
        {
            Ok(stream) => HttpResponse::Ok()
                .content_type(ARROW_IPC_MIME)
                .insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"))
                .insert_header(("X-Max-Rows", max_rows.to_string()))
                .streaming(stream),
            Err(e) => e.error_response(),
        };
    }

    match backend
        .query_sql(&validated.sql, &validated.datasets, max_rows)
        .await
    {
        Ok(arr) => {
            let body = format!(r#"{{"data":{arr},"max_rows":{max_rows}}}"#);
            let mut resp = HttpResponse::Ok();
            resp.content_type("application/json");
            if wants_no_compression(&http) {
                resp.insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"));
            }
            resp.body(body)
        }
        Err(e) => e.error_response(),
    }
}

pub async fn stream_dataset(
    http: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    if let Err(e) = require_read(&http) {
        return e.error_response();
    }
    let name = path.into_inner();
    let mut req = body.into_inner();

    if let Ok(schema) = backend.schema(&name)
        && let Err(e) = req.enforce_column_filters(&schema)
    {
        return e.error_response();
    }

    match backend.query_arrow_stream_all(&name, &req).await {
        Ok(stream) => HttpResponse::Ok()
            .content_type(ARROW_IPC_MIME)
            .insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"))
            .insert_header(("X-Query-Mode", "stream"))
            .streaming(stream),
        Err(e) => e.error_response(),
    }
}

pub async fn count_dataset(
    req: HttpRequest,
    backend: BackendData,
    path: web::Path<String>,
    body: Option<web::Json<CountRequest>>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    let req = body.map(|b| b.into_inner()).unwrap_or_default();

    if let Ok(schema) = backend.schema(&name)
        && let Err(e) = req.enforce_column_filters(&schema)
    {
        return e.error_response();
    }

    match backend.count(&name, &req).await {
        Ok(n) => {
            let mut resp = HttpResponse::Ok();
            if let Some(ts) = backend.refreshed_at(&name) {
                resp.insert_header(("X-Dataset-Refreshed-At", ts));
            }
            resp.json(serde_json::json!({ "count": n }))
        }
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: register a brand-new dataset at runtime from a JSON
/// [`crate::config::DatasetConfig`] body and make it immediately queryable —
/// no server restart. The dataset lives in memory only; use
/// `POST /datasets/persist` to also append it to the on-disk config.
///
/// Requires the same reload/admin permission as `/reload`. The backend
/// validates the config and opens the source, so an unreachable source or a
/// duplicate name surfaces as a `400`.
pub async fn register_dataset(
    req: HttpRequest,
    backend: BackendData,
    body: web::Json<crate::config::DatasetConfig>,
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    match backend.register(body.into_inner()).await {
        Ok(summary) => HttpResponse::Ok().json(summary),
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: append a dataset's `[[dataset]]` block to the server's
/// on-disk config file so a runtime-registered dataset survives a restart.
///
/// Takes the same JSON [`crate::config::DatasetConfig`] body as
/// `POST /datasets` and requires the reload/admin permission. Only works
/// when the server was loaded from a config file; otherwise returns `400`.
pub async fn persist_dataset(
    req: HttpRequest,
    body: web::Json<crate::config::DatasetConfig>,
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    match body.persist_to_source_config() {
        Ok(path) => HttpResponse::Ok().json(serde_json::json!({
            "persisted": true,
            "path":      path.display().to_string(),
        })),
        Err(e) => e.error_response(),
    }
}

/// Admin endpoint: re-read the server's on-disk `datasets.toml` and register
/// any datasets added since startup (or the previous config reload).
///
/// This is a *hot* config reload: the file is re-read and validated, then
/// every `[[dataset]]` whose name is not already registered is opened and
/// registered live. Datasets that already exist are left untouched (use
/// `/datasets/{name}/reload` to rebuild one), and server-level settings
/// (port, workers, …) are not re-applied — those still require a restart.
///
/// Requires the reload/admin permission and only works when the server was
/// started from a config file. Returns the names that were registered,
/// those skipped as already-present, and any per-dataset errors (a bad
/// dataset does not abort the others).
pub async fn reload_config(req: HttpRequest, backend: BackendData) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    let Some(path) = crate::config::source_config_path() else {
        return crate::errors::AppError::InvalidValue(
            "server has no on-disk config file to reload".into(),
        )
        .error_response();
    };
    let cfg = match crate::config::AppConfig::load(&path.to_string_lossy()) {
        Ok(c) => c,
        Err(e) => return e.error_response(),
    };

    let existing: std::collections::HashSet<String> = backend.names().into_iter().collect();

    let mut registered: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();
    for ds in cfg.datasets {
        if existing.contains(&ds.name) {
            skipped.push(ds.name);
            continue;
        }
        let name = ds.name.clone();
        match backend.register(ds).await {
            Ok(_) => registered.push(name),
            Err(e) => errors.push(serde_json::json!({ "dataset": name, "error": e.to_string() })),
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "registered": registered,
        "skipped":    skipped,
        "errors":     errors,
    }))
}

/// Admin endpoint: rebuild a dataset from disk and atomically swap it in.
/// Requires `X-Admin-Token` matching `$ADMIN_TOKEN`. Disabled if the env
/// var is unset.
pub async fn reload_dataset(
    req: HttpRequest,
    backend: BackendData,
    cache: Option<web::Data<ParquetCache>>,
    path: web::Path<String>,
    #[cfg(feature = "metrics")] metrics: Option<
        web::Data<std::sync::Arc<crate::metrics::DatapressMetrics>>,
    >,
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    match backend.reload(&name).await {
        Ok(stats) => {
            // Increment materialization-specific metrics from the build flags.
            #[cfg(feature = "metrics")]
            if let Some(m) = metrics.as_ref() {
                if stats.demoted_to_storage {
                    crate::metrics::record_spill(m, &name);
                }
                if stats.memory_override_exceeded {
                    crate::metrics::record_memory_override(m, &name);
                }
            }
            // The cached Parquet export is now stale — drop it so the next
            // `/parquet` request rebuilds from the freshly reloaded data.
            if let Some(cache) = cache {
                cache.invalidate(&name);
            }
            HttpResponse::Ok().json(serde_json::json!({
                "dataset":    name,
                "rows":       stats.rows,
                "elapsed_ms": stats.elapsed_ms,
            }))
        }
        Err(e) => e.error_response(),
    }
}

/// `POST /datasets/reload-all` — rebuild every reloadable dataset as one
/// wave in topological order (R8.11).
///
/// Admin-gated identically to per-dataset reload. Datasets that are
/// currently `building` or are `pending` with `lazy`/`skip` on_start are
/// placed in `skipped` in the snapshot. The remaining datasets are
/// **enqueued**: a detached `tokio::spawn` wave task runs their builds in
/// topological order via `try_reload` (dependencies before dependents).
///
/// # Returns immediately (202 Accepted)
///
/// The response is sent before any builds complete.  `enqueued` = datasets
/// handed to the wave task at snapshot time; `skipped` = datasets excluded at
/// snapshot time.  `try_reload` returning `Ok(None)` (per-dataset mutex already
/// held) inside the wave is a coalesce — logged at DEBUG but not reflected in
/// the 202 body since the response has already been sent.
///
/// # Exactly-once per wave
///
/// The wave processes datasets in strict topological order (dependency before
/// dependent).  While the wave is actively building dataset `d` it holds `d`'s
/// per-dataset reload mutex.  Any concurrent cascade `try_reload` for the same
/// dataset during that window coalesces to `Ok(None)` (mutex held).  After the
/// wave releases the mutex and before the cascade debounce window (default 5 s)
/// fires, `d` is already freshly built; the cascade `try_reload` would then
/// start a new build.  This is identical to the natural cascade behavior for
/// any manual reload and is intentionally not suppressed: the debounce limits
/// it to at most one extra build per upstream publish, consistent with the
/// cascade contract.  Tests use a mock backend without a live cascade engine so
/// build counts are stable after the wave task completes.
///
/// # Failure recording
///
/// Build failures inside the wave call `backend.record_reload_failure()` so
/// that `consecutive_failures` and `last_error` are reflected in `/status`
/// immediately after the failure, without waiting for the next scheduler tick.
///
/// # Shutdown
///
/// The wave task is a detached tokio task; it is NOT attached to actix's
/// worker pool.  Each individual `try_reload` is bounded by the per-dataset
/// reload timeout.  Graceful shutdown waits for in-flight actix requests via
/// `shutdown_timeout_secs`; the wave task may outlive actix's drain window but
/// is bounded by per-dataset timeout and will be abandoned on process exit.
pub async fn reload_all_datasets(
    req: HttpRequest,
    backend: BackendData,
    cache: Option<web::Data<ParquetCache>>,
    #[cfg(feature = "metrics")] metrics: Option<
        web::Data<std::sync::Arc<crate::metrics::DatapressMetrics>>,
    >,
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }

    // ------------------------------------------------------------------ //
    // Snapshot — classify datasets at request time.
    // ------------------------------------------------------------------ //
    use crate::backend::DatasetStatus;
    use crate::config::OnStart;
    let statuses = backend.dataset_statuses();

    // Build a dependency map for Kahn's topological sort.
    let names: Vec<String> = statuses.iter().map(|s| s.name.clone()).collect();
    let name_idx: std::collections::HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();

    let n = names.len();
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n]; // adj[dep] -> [dependents]
    let mut in_deg: Vec<usize> = vec![0; n];

    for s in &statuses {
        if let Some(&i) = name_idx.get(s.name.as_str()) {
            for dep in &s.depends_on {
                if let Some(&j) = name_idx.get(dep.as_str()) {
                    adj[j].push(i);
                    in_deg[i] += 1;
                }
            }
        }
    }

    let mut queue: std::collections::VecDeque<usize> = (0..n).filter(|&i| in_deg[i] == 0).collect();
    let mut topo_order: Vec<usize> = Vec::with_capacity(n);
    while let Some(i) = queue.pop_front() {
        topo_order.push(i);
        for &j in &adj[i] {
            in_deg[j] -= 1;
            if in_deg[j] == 0 {
                queue.push_back(j);
            }
        }
    }
    // Datasets not reachable by Kahn are in a cycle (config validation
    // already rejects cycles — be defensive).
    let in_cycle: std::collections::HashSet<usize> =
        (0..n).filter(|i| !topo_order.contains(i)).collect();

    // Classify into enqueued / skipped at snapshot time.
    let mut enqueued: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for &idx in &topo_order {
        if in_cycle.contains(&idx) {
            skipped.push(names[idx].clone());
            continue;
        }
        let s = &statuses[idx];
        if s.status == DatasetStatus::Building {
            skipped.push(s.name.clone());
            continue;
        }
        if s.status == DatasetStatus::Pending
            && (s.on_start == OnStart::Lazy || s.on_start == OnStart::Skip)
        {
            skipped.push(s.name.clone());
            continue;
        }
        enqueued.push(s.name.clone());
    }

    // ------------------------------------------------------------------ //
    // Spawn the wave task — detached; response returns immediately.
    // ------------------------------------------------------------------ //
    let wave_backend: std::sync::Arc<dyn crate::backend::Backend> =
        std::sync::Arc::clone(&*backend);
    let wave_cache = cache.map(|c| c.into_inner());
    let wave_names = enqueued.clone();
    #[cfg(feature = "metrics")]
    let wave_metrics = metrics;

    tokio::spawn(async move {
        for name in wave_names {
            match wave_backend.try_reload(&name).await {
                Ok(Some(stats)) => {
                    #[cfg(feature = "metrics")]
                    if let Some(ref m) = wave_metrics {
                        if stats.demoted_to_storage {
                            crate::metrics::record_spill(m, &name);
                        }
                        if stats.memory_override_exceeded {
                            crate::metrics::record_memory_override(m, &name);
                        }
                    }
                    #[cfg(not(feature = "metrics"))]
                    let _ = stats;
                    if let Some(ref c) = wave_cache {
                        c.invalidate(&name);
                    }
                }
                Ok(None) => {
                    // Per-dataset mutex already held — coalesced.
                    log::debug!("[reload-all] dataset='{}' coalesced (mutex held)", name);
                }
                Err(e) => {
                    // Build failed; keep-last-good (G3).  Record the failure
                    // so /status reflects consecutive_failures + last_error.
                    log::warn!("[reload-all] dataset='{}' build failed: {}", name, e);
                    wave_backend.record_reload_failure(&name, &e.to_string());
                }
            }
        }
    });

    HttpResponse::Accepted().json(serde_json::json!({
        "enqueued": enqueued,
        "skipped":  skipped,
    }))
}

/// Serve the whole dataset as a single Parquet file with HTTP range +
/// `HEAD` support, so external tools can read it over HTTP — e.g.
/// `SELECT count(*) FROM 'http://host/api/v1/datasets/accidents/parquet'`
/// from a DuckDB client with `httpfs` loaded.
///
/// The encoded file is cached per dataset (see [`ParquetCache`]) and
/// invalidated on reload, so the multiple range requests a Parquet reader
/// makes all observe identical bytes.
pub async fn parquet_dataset(
    req: HttpRequest,
    backend: BackendData,
    cache: Option<web::Data<ParquetCache>>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let name = path.into_inner();

    // The parquet export streams the raw source, which would bypass a
    // projection filter and leak hidden columns. Refuse it for datasets that
    // hide columns.
    match backend.schema(&name) {
        Ok(schema) if schema.projection_filter.is_active() => {
            return crate::errors::AppError::Forbidden(format!(
                "parquet export is disabled for dataset '{name}' because it hides columns"
            ))
            .error_response();
        }
        Ok(_) => {}
        Err(e) => return e.error_response(),
    }

    let body = match cache.as_ref().and_then(|c| c.get(&name)) {
        Some(cached) => cached,
        None => match backend.parquet(&name).await {
            Ok(bytes) => match cache.as_ref() {
                Some(c) => c.insert(&name, bytes),
                None => std::sync::Arc::new(bytes),
            },
            Err(e) => return e.error_response(),
        },
    };

    serve_bytes_with_range(&req, body, PARQUET_MIME)
}

// ---------------------------------------------------------------------------
// Phase 6 — Saved Queries API
// ---------------------------------------------------------------------------

/// `POST /api/v1/queries` — create a runtime-managed dataset from SQL.
///
/// Auth: `X-Admin-Token` or `datasets:manage` scope (R8.6). Routes return
/// 404 when neither the admin token nor auth is configured.
pub async fn create_query(
    req: HttpRequest,
    backend: BackendData,
    settings: web::Data<SavedQueriesSettings>,
    ttl_handle: web::Data<crate::refresh::TtlHandle>,
    body: web::Json<CreateQueryRequest>,
) -> HttpResponse {
    use crate::config::{DatasetConfig, SourceConfig, SourceKind};
    use crate::errors::AppError;

    if !settings.enabled {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "queries API is disabled — set ADMIN_TOKEN or configure auth"
        }));
    }
    if let Err(e) = require_manage(&req) {
        return e.error_response();
    }

    let body = body.into_inner();

    // Reserved-name guard.
    if crate::config::RESERVED_DATASET_NAMES.contains(&body.name.as_str()) {
        return AppError::InvalidValue(format!(
            "dataset name '{}' is reserved and cannot be used",
            body.name
        ))
        .error_response();
    }

    // Conflict check (name already exists in any form).
    if backend.names().contains(&body.name)
        || backend
            .dataset_statuses()
            .iter()
            .any(|s| s.name == body.name)
    {
        return AppError::InvalidValue(format!("dataset '{}' already exists", body.name))
            .error_response();
    }

    // --- Infer depends_on from the SQL (R8, inference is allowed here). ---
    // Build the set of registered dataset names as the allowed set.
    let allowed: std::collections::HashSet<String> = backend
        .names()
        .into_iter()
        .map(|n| n.to_lowercase())
        .collect();

    let validated = match crate::sql::validate(&body.sql, &allowed, usize::MAX) {
        Ok(v) => v,
        Err(e) => {
            return AppError::InvalidValue(format!("sql is invalid: {e}")).error_response();
        }
    };
    let depends_on: Vec<String> = validated.datasets.clone();

    // Build a DatasetConfig for the backend.
    let source = SourceConfig {
        kind: SourceKind::Query,
        sql: Some(body.sql.clone()),
        depends_on: depends_on.clone(),
        location: String::new(),
    };
    // For query kind, location must be empty; managed=true, temp per kind.
    let is_temp = body.kind == SavedQueryKind::Temp;

    let dataset_cfg = DatasetConfig {
        name: body.name.clone(),
        source,
        managed: true,
        temp: is_temp,
        refresh: if is_temp { None } else { body.refresh.clone() },
        materialize: body.materialize.clone(),
        index: body.index.clone().unwrap_or_default(),
        s3: None,
        columns: Vec::new(),
        dict_encode: true,
        lazy: false,
        predicate_filter: Default::default(),
        projection_filter: Default::default(),
        on_start: crate::config::OnStart::Eager,
    };

    // Validate the config (name format, reserved names, index constraints).
    if let Err(e) = dataset_cfg.validate_for_register() {
        return e.error_response();
    }

    // Async build.
    let async_mode = req
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .any(|p| p == "async=true");

    let mut managed_file = None;

    // For `kind = "query"`, persist to datasets.d/. Do this before async
    // registration too; the Explorer save flow uses `?async=true`, and the
    // persisted definition must survive a process restart even while the
    // current process finishes building it in the background.
    if !is_temp {
        match settings.dir.as_ref() {
            None => {
                return AppError::InvalidValue(
                    "kind = \"query\" requires a server config file (cannot persist)".into(),
                )
                .error_response();
            }
            Some(dir) => match dataset_cfg.persist_to_managed_dir(dir) {
                Ok(path) => {
                    managed_file = Some(crate::config::display_path_relative_to_config(&path));
                }
                Err(e) => return e.error_response(),
            },
        }
    }

    if async_mode {
        // Register asynchronously: spawn build in background.
        let backend_clone = backend.clone();
        let cfg_clone = dataset_cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = backend_clone.register(cfg_clone).await {
                log::warn!("[queries] async register failed: {e}");
            }
        });

        // Return 202 immediately.
        let resp = SavedQueryEntry {
            name: body.name.clone(),
            kind: body.kind,
            depends_on,
            state: "building".into(),
            managed_file,
        };
        return HttpResponse::Accepted().json(resp);
    }

    // Synchronous build.
    if let Err(e) = backend.register(dataset_cfg.clone()).await {
        return e.error_response();
    }

    // For `kind = "temp"` with a TTL, schedule deletion.
    if is_temp && let Some(ttl) = body.ttl {
        let fire_at = tokio::time::Instant::now() + ttl;
        ttl_handle.schedule(body.name.clone(), fire_at);
    }

    let state = backend
        .dataset_statuses()
        .into_iter()
        .find(|s| s.name == body.name)
        .map(|s| format!("{:?}", s.status).to_lowercase())
        .unwrap_or_else(|| "published".into());

    let resp = SavedQueryEntry {
        name: body.name,
        kind: body.kind,
        depends_on,
        state,
        managed_file,
    };
    HttpResponse::Ok().json(resp)
}

/// `GET /api/v1/queries` — list all runtime-managed datasets.
pub async fn list_queries(
    req: HttpRequest,
    backend: BackendData,
    settings: web::Data<SavedQueriesSettings>,
) -> HttpResponse {
    if !settings.enabled {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "queries API is disabled — set ADMIN_TOKEN or configure auth"
        }));
    }
    if let Err(e) = require_manage(&req) {
        return e.error_response();
    }

    let statuses = backend.dataset_statuses();
    let entries: Vec<SavedQueryEntry> = statuses
        .into_iter()
        .filter(|s| backend.is_managed(&s.name))
        .map(|s| {
            let kind = if backend.is_temp(&s.name) {
                SavedQueryKind::Temp
            } else {
                SavedQueryKind::Query
            };
            SavedQueryEntry {
                depends_on: s.depends_on.clone(),
                state: format!("{:?}", s.status).to_lowercase(),
                name: s.name,
                kind,
                managed_file: None,
            }
        })
        .collect();

    HttpResponse::Ok().json(entries)
}

/// `DELETE /api/v1/queries/{name}` — unregister a managed dataset.
///
/// Returns `403` for config-defined datasets, `409` when dependents exist.
pub async fn delete_query(
    req: HttpRequest,
    backend: BackendData,
    settings: web::Data<SavedQueriesSettings>,
    path: web::Path<String>,
) -> HttpResponse {
    use crate::errors::AppError;

    if !settings.enabled {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "queries API is disabled — set ADMIN_TOKEN or configure auth"
        }));
    }
    if let Err(e) = require_manage(&req) {
        return e.error_response();
    }

    let name = path.into_inner();

    // Check managed — config datasets return 403.
    if !backend.is_managed(&name) {
        // Distinguish "not found" from "exists but not managed".
        let exists = backend.dataset_statuses().iter().any(|s| s.name == name);
        if !exists {
            return AppError::NotFound(format!("dataset '{name}' not found")).error_response();
        }
        return AppError::Forbidden(format!(
            "dataset '{name}' is defined in the server config and cannot be deleted via the API"
        ))
        .error_response();
    }

    // Check for dependents — find all datasets whose depends_on includes `name`.
    let statuses = backend.dataset_statuses();
    let dependents: Vec<&str> = statuses
        .iter()
        .filter(|s| s.depends_on.iter().any(|d| d == &name))
        .map(|s| s.name.as_str())
        .collect();
    if !dependents.is_empty() {
        return AppError::Conflict(format!(
            "dataset '{name}' has dependents: {}; delete them first",
            dependents.join(", ")
        ))
        .error_response();
    }

    // Unregister from backend.
    if let Err(e) = backend.unregister(&name).await {
        return e.error_response();
    }

    // Delete persisted file if it was a `kind = "query"` dataset.
    if let Some(dir) = settings.dir.as_ref()
        && let Err(e) = crate::config::DatasetConfig::remove_from_managed_dir(&name, dir)
    {
        log::warn!(
            "[queries] failed to remove managed TOML for '{}': {e}",
            name
        );
        // Non-fatal: dataset is already unregistered from the engine.
    }

    HttpResponse::Ok().json(serde_json::json!({
        "deleted": name,
    }))
}
