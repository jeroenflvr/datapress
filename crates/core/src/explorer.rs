//! Embedded dataset explorer UI.
//!
//! Compiled in only when the `explorer` cargo feature is enabled. A
//! server-rendered web app (Actix + Askama templates + htmx + Bootstrap)
//! served from `[explorer].path` (default `/explore`). It offers:
//!
//! * a **Discovery** view — per-dataset stats, schema, index and source
//!   configuration, rendered server-side and swapped in via htmx; and
//! * a **DuckDB** console — DuckDB-WASM running entirely in the browser,
//!   querying each dataset's Parquet export
//!   (`{api_base}/datasets/{name}/all.parquet`) directly; and
//! * a **Terminal** — a full DuckDB-WASM shell (xterm) with every dataset
//!   pre-registered as a view, embedded inline and openable in its own tab
//!   at `{explorer_base}/terminal`.
//!
//! Templates live under `crates/core/templates/explorer/` and are compiled
//! into the binary by Askama, so nothing is read from disk at runtime.

use std::sync::{Arc, RwLock};

use actix_web::{HttpRequest, HttpResponse, http::header, web};
use askama::Template;
use include_dir::{Dir, include_dir};

use crate::backend::Backend;
use crate::config::{
    AddressingStyle, DatasetConfig, IndexConfig, IndexMode, Partitioning, S3Config, SourceConfig,
    SourceKind,
};
use crate::schema::LogicalType;

/// Self-hosted Apache Arrow (UMD) bundle, embedded at compile time. Used by
/// the API Query tab to decode Arrow IPC responses in the browser without a
/// CDN. Refreshed by `task docs:vendor-arrow`.
static ARROW_VENDOR: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../docs/src/assets/vendor/arrow");

/// Shared state handed to the explorer handlers.
pub struct ExplorerState {
    pub backend: Arc<dyn Backend>,
    /// Dataset configs shown in the UI. Behind an `RwLock` so datasets
    /// registered live through the explorer appear without a restart.
    pub datasets: RwLock<Vec<DatasetConfig>>,
    /// Absolute mount path of the explorer UI, e.g. `/explore`.
    pub explorer_base: String,
    /// Absolute base path of the versioned API, e.g. `/api/v1` (or
    /// `{prefix}/api/v1` behind a reverse proxy).
    pub api_base: String,
    /// Human-readable backend name shown in the navbar (e.g. `DuckDB`).
    pub backend_label: String,
    /// Whether the raw-SQL endpoint (`POST {api_base}/sql`) is enabled.
    /// Drives the API Query tab's SQL mode.
    pub sql_enabled: bool,
    /// Target of the navbar "Docs" link: the locally-mounted MkDocs path
    /// when the docs site is enabled on this server, otherwise the public
    /// documentation URL.
    pub docs_url: String,
    /// Target of the navbar "API" (Swagger UI) link when the Swagger UI is
    /// enabled on this server; `None` hides the link.
    pub swagger_url: Option<String>,
}

#[derive(Template)]
#[template(path = "explorer/index.html")]
struct IndexTemplate {
    backend_label: String,
    explorer_base: String,
    api_base: String,
    asset_version: &'static str,
    sql_enabled: bool,
    docs_url: String,
    swagger_url: Option<String>,
    datasets: Vec<DatasetListItem>,
    datasets_json: String,
    /// Whether the server can append newly-registered datasets back to its
    /// on-disk config file (only when it was loaded from one).
    can_persist: bool,
    /// Display path of that config file (empty when `can_persist` is false).
    config_path: String,
}

struct DatasetListItem {
    name: String,
    rows: usize,
    columns: usize,
    kind: String,
}

#[derive(Template)]
#[template(path = "explorer/terminal.html")]
struct TerminalTemplate {
    backend_label: String,
    explorer_base: String,
    asset_version: &'static str,
}

#[derive(Template)]
#[template(path = "explorer/dataset.html")]
struct DatasetTemplate {
    name: String,
    rows: usize,
    column_count: usize,
    indexed_count: usize,
    nullable_count: usize,
    source_kind: String,
    source_location: String,
    index_mode: String,
    index_columns: String,
    projection: String,
    dict_encode: bool,
    lazy: bool,
    parquet_url: String,
    schema_url: String,
    datasets_url: String,
    columns: Vec<ColumnView>,
    sample_pretty: String,
    has_s3: bool,
    s3_region: String,
    s3_endpoint: String,
    s3_addressing: String,
    s3_partitioning: String,
    s3_creds: String,
}

struct ColumnView {
    name: String,
    logical: &'static str,
    sql_type: String,
    nullable: bool,
    indexed: bool,
}

fn logical_str(t: LogicalType) -> &'static str {
    match t {
        LogicalType::Bool => "bool",
        LogicalType::Int => "int",
        LogicalType::Float => "float",
        LogicalType::Utf8 => "utf8",
        LogicalType::Temporal => "temporal",
        LogicalType::Other => "other",
    }
}

/// Mount the explorer under `state.explorer_base` (e.g. `/explore`).
pub fn configure(state: web::Data<ExplorerState>, cfg: &mut web::ServiceConfig) {
    let mount = state.explorer_base.clone();
    // Redirect the bare mount (no trailing slash) so relative asset and
    // htmx URLs resolve under the mount.
    let redirect_target = format!("{mount}/");
    cfg.app_data(state)
        .service(
            web::resource(mount.clone()).route(web::get().to(move || {
                let to = redirect_target.clone();
                async move {
                    HttpResponse::MovedPermanently()
                        .insert_header((header::LOCATION, to))
                        .finish()
                }
            })),
        )
        .service(
            web::scope(&mount)
                .route("/", web::get().to(index))
                .route("/terminal", web::get().to(terminal))
                .route("/assets/explorer.css", web::get().to(asset_explorer_css))
                .route("/assets/explorer.js", web::get().to(asset_explorer_js))
                .route("/assets/query-api.js", web::get().to(asset_query_api_js))
                .route("/assets/terminal.css", web::get().to(asset_terminal_css))
                .route("/assets/terminal.js", web::get().to(asset_terminal_js))
                .route("/assets/pypi.svg", web::get().to(asset_pypi_icon))
                .route("/assets/book.svg", web::get().to(asset_book_icon))
                .route("/assets/swagger.svg", web::get().to(asset_swagger_icon))
                // Single-segment `{path}` (not `{path:.*}`): the vendored
                // dirs are flat, and a tail regex makes the prometheus
                // middleware's strfmt label substitution fail and warn on
                // every asset request ("mixed cardinality pattern").
                .route(
                    "/assets/vendor/duckdb/{path}",
                    web::get().to(asset_duckdb_vendor),
                )
                .route(
                    "/assets/vendor/arrow/{path}",
                    web::get().to(asset_arrow_vendor),
                )
                .route("/datasets/{name}", web::get().to(dataset_detail))
                .route("/datasets", web::post().to(register_dataset))
                .route(
                    "/datasets/{name}/persist",
                    web::post().to(persist_dataset),
                ),
        );
}

fn render<T: Template>(tpl: &T) -> HttpResponse {
    match tpl.render() {
        Ok(body) => HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(body),
        Err(e) => HttpResponse::InternalServerError()
            .content_type("text/plain; charset=utf-8")
            .body(format!("template error: {e}")),
    }
}

/// Build the `[{name, rows, parquet}]` payload consumed by the DuckDB-WASM
/// console and shell terminal, alongside the discovery list items.
fn collect_datasets(state: &ExplorerState) -> (Vec<DatasetListItem>, String) {
    let datasets = state.datasets.read().unwrap();
    let mut items = Vec::with_capacity(datasets.len());
    let mut json_items = Vec::with_capacity(datasets.len());
    for ds in datasets.iter() {
        let (rows, columns) = match state.backend.summary(&ds.name) {
            Ok(s) => (s.rows, s.columns),
            Err(_) => (0, 0),
        };
        items.push(DatasetListItem {
            name: ds.name.clone(),
            rows,
            columns,
            kind: ds.source.kind.as_str().to_string(),
        });
        json_items.push(serde_json::json!({
            "name": ds.name,
            "rows": rows,
            "parquet": format!("{}/datasets/{}/all.parquet", state.api_base, ds.name),
        }));
    }
    let datasets_json = serde_json::to_string(&json_items).unwrap_or_else(|_| "[]".into());
    (items, datasets_json)
}

async fn index(state: web::Data<ExplorerState>) -> HttpResponse {
    let (items, datasets_json) = collect_datasets(&state);
    let config_path = crate::config::source_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let tpl = IndexTemplate {
        backend_label: state.backend_label.clone(),
        explorer_base: state.explorer_base.clone(),
        api_base: state.api_base.clone(),
        asset_version: env!("CARGO_PKG_VERSION"),
        sql_enabled: state.sql_enabled,
        docs_url: state.docs_url.clone(),
        swagger_url: state.swagger_url.clone(),
        datasets: items,
        datasets_json,
        can_persist: !config_path.is_empty(),
        config_path,
    };
    render(&tpl)
}

async fn terminal(state: web::Data<ExplorerState>) -> HttpResponse {
    let tpl = TerminalTemplate {
        backend_label: state.backend_label.clone(),
        explorer_base: state.explorer_base.clone(),
        asset_version: env!("CARGO_PKG_VERSION"),
    };
    render(&tpl)
}

// Static assets are embedded into the binary at compile time and served with
// long-lived cache headers; they carry no per-request state.
const EXPLORER_CSS: &str = include_str!("../assets/explorer/explorer.css");
const EXPLORER_JS: &str = include_str!("../assets/explorer/explorer.js");
const QUERY_API_JS: &str = include_str!("../assets/explorer/query-api.js");
const TERMINAL_CSS: &str = include_str!("../assets/explorer/terminal.css");
const TERMINAL_JS: &str = include_str!("../assets/explorer/terminal.js");
// Navbar link icons (PyPI / Docs), embedded from the docs asset tree.
const PYPI_ICON_SVG: &str =
    include_str!("../../../docs/src/assets/images/python-logo-only.svg");
const BOOK_ICON_SVG: &str = include_str!("../../../docs/src/assets/images/book.svg");
const SWAGGER_ICON_SVG: &str = include_str!("../../../docs/src/assets/images/swagger.svg");

fn asset(content_type: &'static str, body: &'static str) -> HttpResponse {
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((header::CACHE_CONTROL, "public, max-age=3600"))
        .body(body)
}

async fn asset_explorer_css() -> HttpResponse {
    asset("text/css; charset=utf-8", EXPLORER_CSS)
}

async fn asset_explorer_js() -> HttpResponse {
    asset("application/javascript; charset=utf-8", EXPLORER_JS)
}

async fn asset_query_api_js() -> HttpResponse {
    asset("application/javascript; charset=utf-8", QUERY_API_JS)
}

async fn asset_terminal_css() -> HttpResponse {
    asset("text/css; charset=utf-8", TERMINAL_CSS)
}

async fn asset_terminal_js() -> HttpResponse {
    asset("application/javascript; charset=utf-8", TERMINAL_JS)
}

async fn asset_pypi_icon() -> HttpResponse {
    asset("image/svg+xml; charset=utf-8", PYPI_ICON_SVG)
}

async fn asset_book_icon() -> HttpResponse {
    asset("image/svg+xml; charset=utf-8", BOOK_ICON_SVG)
}

async fn asset_swagger_icon() -> HttpResponse {
    asset("image/svg+xml; charset=utf-8", SWAGGER_ICON_SVG)
}

/// Serve a vendored DuckDB-WASM asset (wasm binary, worker script, bundled
/// ESM, or xterm CSS) from the shared binary-embedded store. The large wasm
/// binaries are stored pre-gzipped and served with `Content-Encoding: gzip`;
/// see [`crate::duckdb_vendor`].
async fn asset_duckdb_vendor(req: HttpRequest) -> HttpResponse {
    let path: String = req.match_info().query("path").into();
    crate::duckdb_vendor::serve(&path).unwrap_or_else(|| {
        HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("Not Found")
    })
}

/// Serve the vendored Apache Arrow UMD bundle from the binary-embedded
/// directory. Immutable per release, so it carries a long-lived cache header.
async fn asset_arrow_vendor(req: HttpRequest) -> HttpResponse {
    let path: String = req.match_info().query("path").into();
    match ARROW_VENDOR.get_file(&path) {
        Some(f) => HttpResponse::Ok()
            .content_type(
                mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .as_ref(),
            )
            .insert_header((header::CACHE_CONTROL, "public, max-age=86400"))
            .body(f.contents()),
        None => HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("Not Found"),
    }
}

async fn dataset_detail(state: web::Data<ExplorerState>, path: web::Path<String>) -> HttpResponse {
    let name = path.into_inner();
    // Clone the config out of the lock so we don't hold the guard across the
    // `await` below (the guard isn't `Send`).
    let Some(ds) = state
        .datasets
        .read()
        .unwrap()
        .iter()
        .find(|d| d.name == name)
        .cloned()
    else {
        // Dataset names are validated to `[A-Za-z0-9_.-]` at config load,
        // so the echoed name is safe to inline without HTML escaping.
        return HttpResponse::NotFound()
            .content_type("text/html; charset=utf-8")
            .body(format!(
                "<div class=\"alert alert-warning\">Unknown dataset: {name}</div>"
            ));
    };
    let ds = &ds;

    let summary = state.backend.summary(&name).ok();
    let rows = summary.as_ref().map(|s| s.rows).unwrap_or(0);

    let schema = state.backend.schema(&name).ok();
    let indexed = state
        .backend
        .indexed_columns(&name)
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.to_lowercase())
        .collect::<std::collections::HashSet<_>>();

    let mut columns = Vec::new();
    let mut nullable_count = 0usize;
    if let Some(sc) = schema.as_ref() {
        for c in &sc.columns {
            if c.nullable {
                nullable_count += 1;
            }
            columns.push(ColumnView {
                name: c.name.clone(),
                logical: logical_str(c.logical),
                sql_type: c.sql_type.clone(),
                nullable: c.nullable,
                indexed: indexed.contains(&c.name.to_lowercase()),
            });
        }
    }
    let column_count = summary
        .as_ref()
        .map(|s| s.columns)
        .unwrap_or(columns.len());

    let sample_pretty = match state.backend.sample(&name).await {
        Ok(s) if s.trim() == "null" => "—".to_string(),
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or(s),
        Err(_) => "—".to_string(),
    };

    let projection = if ds.columns.is_empty() {
        "all columns".to_string()
    } else {
        ds.columns.join(", ")
    };

    let (has_s3, s3_region, s3_endpoint, s3_addressing, s3_partitioning, s3_creds) =
        match ds.s3.as_ref() {
            Some(s3) => (
                true,
                s3.region.clone().unwrap_or_else(|| "—".into()),
                s3.endpoint.clone().unwrap_or_else(|| "(AWS default)".into()),
                s3.addressing_style.as_str().to_string(),
                s3.partitioning.as_str().to_string(),
                if s3.access_key_id.is_some() && s3.secret_access_key.is_some() {
                    "inline keys".to_string()
                } else {
                    "env / provider chain".to_string()
                },
            ),
            None => (
                false,
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };

    let tpl = DatasetTemplate {
        name: ds.name.clone(),
        rows,
        column_count,
        indexed_count: indexed.len(),
        nullable_count,
        source_kind: ds.source.kind.as_str().to_string(),
        source_location: ds.source.location.clone(),
        index_mode: format!("{:?}", ds.index.mode).to_lowercase(),
        index_columns: ds.index.columns.join(", "),
        projection,
        dict_encode: ds.dict_encode,
        lazy: ds.lazy,
        parquet_url: format!("{}/datasets/{}/all.parquet", state.api_base, ds.name),
        schema_url: format!("{}/datasets/{}/schema", state.api_base, ds.name),
        datasets_url: format!("{}/datasets", state.api_base),
        columns,
        sample_pretty,
        has_s3,
        s3_region,
        s3_endpoint,
        s3_addressing,
        s3_partitioning,
        s3_creds,
    };
    render(&tpl)
}

// ---------------------------------------------------------------------------
// Live dataset registration
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "explorer/register_result.html")]
struct RegisterResultTemplate {
    ok: bool,
    message: String,
    name: String,
    rows: usize,
    columns: usize,
    toml_block: String,
    explorer_base: String,
    can_persist: bool,
    config_path: String,
}

/// Form payload from the explorer "Register dataset" tab. Checkbox fields are
/// only present in the body when ticked, so every field carries a serde
/// default and unchecked boxes deserialize to `None`.
#[derive(Debug, serde::Deserialize)]
struct RegisterForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    columns: String,
    #[serde(default)]
    dict_encode: Option<String>,
    #[serde(default)]
    lazy: Option<String>,
    #[serde(default)]
    index_mode: String,
    #[serde(default)]
    index_columns: String,
    #[serde(default)]
    index_max_cardinality: String,
    #[serde(default)]
    s3_enabled: Option<String>,
    #[serde(default)]
    s3_region: String,
    #[serde(default)]
    s3_endpoint: String,
    #[serde(default)]
    s3_addressing: String,
    #[serde(default)]
    s3_allow_http: Option<String>,
    #[serde(default)]
    s3_access_key_id: String,
    #[serde(default)]
    s3_secret_access_key: String,
    #[serde(default)]
    s3_session_token: String,
    #[serde(default)]
    s3_partitioning: String,
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Translate a submitted [`RegisterForm`] into a [`DatasetConfig`]. Rejects
/// malformed enum selections; deeper validation (name charset, source
/// reachability) happens in the backend's `register`.
fn build_register_config(f: &RegisterForm) -> Result<DatasetConfig, crate::errors::AppError> {
    use crate::errors::AppError;

    let kind = match f.kind.trim() {
        "delta" => SourceKind::Delta,
        "parquet" | "" => SourceKind::Parquet,
        other => return Err(AppError::InvalidValue(format!("unknown source kind '{other}'"))),
    };
    let location = f.location.trim();
    if location.is_empty() {
        return Err(AppError::InvalidValue(
            "source location must not be empty".into(),
        ));
    }

    let index_mode = match f.index_mode.trim() {
        "none" => IndexMode::None,
        "list" => IndexMode::List,
        "auto" | "" => IndexMode::Auto,
        other => return Err(AppError::InvalidValue(format!("unknown index mode '{other}'"))),
    };
    let mut index = IndexConfig {
        mode: index_mode,
        columns: split_csv(&f.index_columns),
        ..IndexConfig::default()
    };
    if let Some(mc) = non_empty(&f.index_max_cardinality) {
        index.max_cardinality = mc.parse().map_err(|_| {
            AppError::InvalidValue("index max_cardinality must be a whole number".into())
        })?;
    }

    let s3 = if f.s3_enabled.is_some() {
        let addressing_style = match f.s3_addressing.trim() {
            "path" => AddressingStyle::Path,
            "virtual" | "" => AddressingStyle::Virtual,
            other => {
                return Err(AppError::InvalidValue(format!(
                    "unknown S3 addressing style '{other}'"
                )));
            }
        };
        let partitioning = match f.s3_partitioning.trim() {
            "hive" => Partitioning::Hive,
            "none" => Partitioning::None,
            "auto" | "" => Partitioning::Auto,
            other => {
                return Err(AppError::InvalidValue(format!(
                    "unknown S3 partitioning '{other}'"
                )));
            }
        };
        Some(S3Config {
            region: non_empty(&f.s3_region),
            endpoint: non_empty(&f.s3_endpoint),
            addressing_style,
            allow_http: f.s3_allow_http.is_some(),
            access_key_id: non_empty(&f.s3_access_key_id),
            secret_access_key: non_empty(&f.s3_secret_access_key),
            session_token: non_empty(&f.s3_session_token),
            partitioning,
            ..S3Config::default()
        })
    } else {
        None
    };

    Ok(DatasetConfig {
        name: f.name.trim().to_string(),
        source: SourceConfig {
            kind,
            location: location.to_string(),
        },
        s3,
        index,
        columns: split_csv(&f.columns),
        dict_encode: f.dict_encode.is_some(),
        lazy: f.lazy.is_some(),
        predicate_filter: crate::config::ColumnFilter::default(),
        projection_filter: crate::config::ColumnFilter::default(),
    })
}

fn register_result_error(msg: &str) -> HttpResponse {
    let tpl = RegisterResultTemplate {
        ok: false,
        message: msg.to_string(),
        name: String::new(),
        rows: 0,
        columns: 0,
        toml_block: String::new(),
        explorer_base: String::new(),
        can_persist: false,
        config_path: String::new(),
    };
    render(&tpl)
}

/// `POST {explorer_base}/datasets` — build a dataset config from the form,
/// register it live in the backend, and add it to the explorer's dataset
/// list. Returns an HTML fragment (success card or error alert) for htmx.
async fn register_dataset(
    state: web::Data<ExplorerState>,
    form: web::Form<RegisterForm>,
) -> HttpResponse {
    let cfg = match build_register_config(&form) {
        Ok(c) => c,
        Err(e) => return register_result_error(&e.to_string()),
    };

    match state.backend.register(cfg.clone()).await {
        Ok(summary) => {
            let toml_block = cfg.to_toml_block().unwrap_or_default();
            state.datasets.write().unwrap().push(cfg);
            let config_path = crate::config::source_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let tpl = RegisterResultTemplate {
                ok: true,
                message: String::new(),
                name: summary.name,
                rows: summary.rows,
                columns: summary.columns,
                toml_block,
                explorer_base: state.explorer_base.clone(),
                can_persist: !config_path.is_empty(),
                config_path,
            };
            render(&tpl)
        }
        Err(e) => register_result_error(&e.to_string()),
    }
}

fn html_alert(kind: &str, inner_html: &str) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(format!(
            "<div class=\"alert alert-{kind} mb-0\" role=\"alert\">{inner_html}</div>"
        ))
}

/// `POST {explorer_base}/datasets/{name}/persist` — append the named
/// dataset's `[[dataset]]` block to the server's on-disk config file so it
/// survives a restart. Only works when the server was loaded from a file.
async fn persist_dataset(
    state: web::Data<ExplorerState>,
    path: web::Path<String>,
) -> HttpResponse {
    let name = path.into_inner();
    if crate::config::source_config_path().is_none() {
        return html_alert(
            "warning",
            "This server has no on-disk config file to append to.",
        );
    }
    // Only persist datasets we actually registered (guards against echoing
    // an arbitrary path segment and against writing unknown configs).
    let Some(cfg) = state
        .datasets
        .read()
        .unwrap()
        .iter()
        .find(|d| d.name == name)
        .cloned()
    else {
        return html_alert("warning", "Unknown or not-yet-registered dataset.");
    };

    // Delegate to the same helper the API's `POST /datasets/persist` uses.
    match cfg.persist_to_source_config() {
        Ok(config_path) => html_alert(
            "success",
            &format!(
                "Appended <code>[[dataset]]</code> for <strong>{}</strong> to \
                 <code>{}</code>.",
                cfg.name,
                config_path.display()
            ),
        ),
        Err(e) => html_alert("danger", &format!("Failed to write config: {e}")),
    }
}

