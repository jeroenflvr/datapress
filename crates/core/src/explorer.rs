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

use std::sync::Arc;

use actix_web::{HttpResponse, http::header, web};
use askama::Template;

use crate::backend::Backend;
use crate::config::DatasetConfig;
use crate::schema::LogicalType;

/// Shared state handed to the explorer handlers.
pub struct ExplorerState {
    pub backend: Arc<dyn Backend>,
    pub datasets: Vec<DatasetConfig>,
    /// Absolute mount path of the explorer UI, e.g. `/explore`.
    pub explorer_base: String,
    /// Absolute base path of the versioned API, e.g. `/api/v1` (or
    /// `{prefix}/api/v1` behind a reverse proxy).
    pub api_base: String,
    /// Human-readable backend name shown in the navbar (e.g. `DuckDB`).
    pub backend_label: String,
}

#[derive(Template)]
#[template(path = "explorer/index.html")]
struct IndexTemplate {
    backend_label: String,
    explorer_base: String,
    api_base: String,
    datasets: Vec<DatasetListItem>,
    datasets_json: String,
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
    datasets_json: String,
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
                .route("/datasets/{name}", web::get().to(dataset_detail)),
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
    let mut items = Vec::with_capacity(state.datasets.len());
    let mut json_items = Vec::with_capacity(state.datasets.len());
    for ds in &state.datasets {
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
    let tpl = IndexTemplate {
        backend_label: state.backend_label.clone(),
        explorer_base: state.explorer_base.clone(),
        api_base: state.api_base.clone(),
        datasets: items,
        datasets_json,
    };
    render(&tpl)
}

async fn terminal(state: web::Data<ExplorerState>) -> HttpResponse {
    let (_, datasets_json) = collect_datasets(&state);
    let tpl = TerminalTemplate {
        backend_label: state.backend_label.clone(),
        datasets_json,
    };
    render(&tpl)
}

async fn dataset_detail(state: web::Data<ExplorerState>, path: web::Path<String>) -> HttpResponse {
    let name = path.into_inner();
    let Some(ds) = state.datasets.iter().find(|d| d.name == name) else {
        // Dataset names are validated to `[A-Za-z0-9_.-]` at config load,
        // so the echoed name is safe to inline without HTML escaping.
        return HttpResponse::NotFound()
            .content_type("text/html; charset=utf-8")
            .body(format!(
                "<div class=\"alert alert-warning\">Unknown dataset: {name}</div>"
            ));
    };

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
