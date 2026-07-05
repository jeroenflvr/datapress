//! Version 1 of the dataset HTTP API.
//!
//! Routes (relative to whichever scope the caller mounts this module
//! under — typically `/api/v1`):
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
//! version can be mounted under multiple scopes — see
//! [`crate::server::serve`] for the canonical `/api/v1` mount and the
//! legacy `/api` alias.

use actix_web::{HttpRequest, HttpResponse, ResponseError, web};

use crate::admin;
use crate::handlers::{
    ARROW_IPC_MIME, BackendData, PARQUET_MIME, ParquetCache, QueryLimits, SqlSettings,
    serve_bytes_with_range, wants_arrow, wants_no_compression,
};
use crate::models::{CountRequest, QueryRequest, SqlRequest};

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

/// Register every v1 route on the provided actix [`web::ServiceConfig`].
///
/// Call this inside a [`web::scope`] — usually `/api/v1` — so paths come
/// out as `/api/v1/datasets/...`.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/datasets", web::get().to(list_datasets))
        .route("/datasets", web::post().to(register_dataset))
        .route("/datasets/persist", web::post().to(persist_dataset))
        .route("/datasets/{name}/schema", web::get().to(get_schema))
        .route("/datasets/{name}/query", web::post().to(query_dataset))
        .route("/sql", web::post().to(sql_query))
        .route(
            "/datasets/{name}/query/stream",
            web::post().to(stream_dataset),
        )
        .route("/datasets/{name}/count", web::post().to(count_dataset))
        .route("/datasets/{name}/parquet", web::get().to(parquet_dataset))
        .route("/datasets/{name}/parquet", web::head().to(parquet_dataset))
        // `.parquet`-suffixed alias so a DuckDB client can use the bare
        // `FROM 'http://host/.../all.parquet'` form — DuckDB sniffs the
        // file type from the URL extension, so it must end in `.parquet`.
        .route(
            "/datasets/{name}/all.parquet",
            web::get().to(parquet_dataset),
        )
        .route(
            "/datasets/{name}/all.parquet",
            web::head().to(parquet_dataset),
        )
        .route("/datasets/{name}/reload", web::post().to(reload_dataset))
        .route("/config/reload", web::post().to(reload_config));
}

/// Route table for log_routes-style introspection. Each entry is
/// `(method, path-suffix)` relative to the version's mount scope.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/datasets"),
    ("POST", "/datasets"),
    ("POST", "/datasets/persist"),
    ("GET", "/datasets/{name}/schema"),
    ("POST", "/datasets/{name}/query"),
    ("POST", "/sql"),
    ("POST", "/datasets/{name}/query/stream"),
    ("POST", "/datasets/{name}/count"),
    ("GET", "/datasets/{name}/parquet"),
    ("GET", "/datasets/{name}/all.parquet"),
    ("POST", "/datasets/{name}/reload"),
    ("POST", "/config/reload"),
];

// ---------------------------------------------------------------- handlers --

pub async fn list_datasets(req: HttpRequest, backend: BackendData) -> HttpResponse {
    if let Err(e) = require_read(&req) {
        return e.error_response();
    }
    let summaries: Vec<_> = backend
        .names()
        .into_iter()
        .filter_map(|n| {
            let mut summary = backend.summary(&n).ok()?;
            // Report only the visible column count when a projection filter
            // hides some columns.
            if let Ok(schema) = backend.schema(&n)
                && schema.projection_filter.is_active()
            {
                summary.columns = schema.visible_columns().len();
            }
            Some(summary)
        })
        .collect();
    HttpResponse::Ok().json(serde_json::json!({ "datasets": summaries }))
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
    match backend.schema(&name) {
        Ok(schema) => {
            if let Err(e) = req.enforce_column_filters(&schema) {
                return e.error_response();
            }
        }
        Err(e) => return e.error_response(),
    }

    // Content negotiation: clients opt into Arrow IPC via the `Accept`
    // header or `?format=arrow`. Anything else (including no header)
    // gets the historical JSON envelope.
    if wants_arrow(&http) {
        return match backend.query_arrow_stream(&name, &req).await {
            Ok(stream) => HttpResponse::Ok()
                .content_type(ARROW_IPC_MIME)
                // Arrow IPC is compact binary; HTTP compression (esp. brotli
                // while streaming) costs far more CPU than it saves and shows
                // up as slow "content download". Opt out so Compress skips it.
                .insert_header((actix_web::http::header::CONTENT_ENCODING, "identity"))
                .insert_header(("X-Page", page.to_string()))
                .insert_header(("X-Page-Size", page_size.to_string()))
                .streaming(stream),
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
/// Phase 1 is scoped to a single dataset per query: the statement is
/// parsed and validated by [`crate::sql::validate`], which rejects
/// anything that is not a single read-only query or `DESCRIBE`, references
/// an unknown table / file function, or touches more than one registered
/// dataset. The result is hard-capped at `[sql].max_rows` rows.
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

    let validated = match crate::sql::validate(&body.sql, &allowed, 1) {
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
            .query_sql_arrow_stream(&validated.sql, max_rows)
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

    match backend.query_sql(&validated.sql, max_rows).await {
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
        Ok(n) => HttpResponse::Ok().json(serde_json::json!({ "count": n })),
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
) -> HttpResponse {
    if let Err(e) = require_reload(&req) {
        return e.error_response();
    }
    let name = path.into_inner();
    match backend.reload(&name).await {
        Ok(stats) => {
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
