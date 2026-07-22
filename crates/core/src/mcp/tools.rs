//! MCP tool registry and dispatch.
//!
//! Every tool is a thin wrapper over the existing [`Backend`] trait and
//! `crate::sql::validate` / `crate::sql::enforce_column_access` — no new
//! query logic lives here.
//!
//! # Tool set
//! - `list_datasets` — list all datasets
//! - `describe_dataset` — schema + sample for one dataset
//! - `describe_all_datasets` — column schemas for all datasets (no samples)
//! - `query_dataset` — structured query (mirrors `/api/v1/{name}/query`)
//! - `count_rows` — cheap row count with optional predicates
//! - `sql` — raw SELECT (only when `[mcp].expose_sql && [sql].enabled`)

use std::sync::Arc;

use serde_json::Value;

use crate::backend::Backend;
use crate::config::{McpConfig, SqlConfig};
use crate::models::{CountRequest, QueryRequest};
use crate::schema::DatasetSchema;

use super::protocol::{ToolInfo, tool_err, tool_ok};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub static TOOL_LIST_DATASETS: ToolInfo = ToolInfo {
    name: "list_datasets",
    description: "List every dataset available on this server with name and column count. \
                  Call this first to discover what data exists.",
    input_schema_json: r#"{"type":"object","properties":{},"required":[]}"#,
};

pub static TOOL_DESCRIBE_DATASET: ToolInfo = ToolInfo {
    name: "describe_dataset",
    description: "Get the schema of one dataset: column names, types, nullability, and one \
                  sample row showing real values. Call before writing predicates or SQL. \
                  Column matching in queries is case-insensitive.",
    input_schema_json: r#"{"type":"object","properties":{"name":{"type":"string","description":"Dataset name (case-insensitive)."}},"required":["name"]}"#,
};

pub static TOOL_DESCRIBE_ALL_DATASETS: ToolInfo = ToolInfo {
    name: "describe_all_datasets",
    description: "Get the column names and types of ALL datasets in one call. Use when \
                  planning a query across multiple datasets (joins via the sql tool). \
                  For sample values, call describe_dataset on a specific dataset.",
    input_schema_json: r#"{"type":"object","properties":{},"required":[]}"#,
};

pub static TOOL_QUERY_DATASET: ToolInfo = ToolInfo {
    name: "query_dataset",
    description: "Query one dataset with structured filters. Supports column projection, \
                  predicates (eq, neq, gt, gte, lt, lte, like, ilike, in, is_null, \
                  is_not_null — ANDed), sorting, group_by + aggregations \
                  (count/sum/avg/min/max), distinct, and pagination. Prefer this over \
                  the sql tool for anything touching a single dataset. Results are \
                  paged; default page_size is small — use count_rows first if unsure \
                  of result size, and always project only the columns you need. \
                  NEVER guess column names: if you have not seen this dataset's schema in \
                  this conversation, call describe_dataset first. Date/time columns must be \
                  filtered with the exact column name and a string value in the column's \
                  native format (see the sample row).",
    input_schema_json: r#"{"type":"object","properties":{"name":{"type":"string"},"columns":{"type":"array","items":{"type":"string"}},"predicates":{"type":"array","items":{"type":"object","properties":{"col":{"type":"string"},"op":{"type":"string","enum":["eq","neq","gt","gte","lt","lte","like","ilike","in","is_null","is_not_null"]},"val":{}},"required":["col","op"]}},"group_by":{"type":"array","items":{"type":"string"}},"aggregations":{"type":"array","items":{"type":"object","properties":{"op":{"type":"string","enum":["count","sum","avg","min","max"]},"col":{"type":"string"},"alias":{"type":"string"}},"required":["op"]}},"having":{"type":"array","items":{"type":"object","properties":{"col":{"type":"string"},"op":{"type":"string"},"val":{}},"required":["col","op"]}},"distinct":{"type":"boolean"},"order_by":{"type":"array","items":{"type":"object","properties":{"col":{"type":"string"},"dir":{"type":"string","enum":["asc","desc"]}},"required":["col"]}},"limit":{"type":"integer"},"page":{"type":"integer"},"page_size":{"type":"integer"}},"required":["name"]}"#,
};

pub static TOOL_COUNT_ROWS: ToolInfo = ToolInfo {
    name: "count_rows",
    description: "Count rows matching predicates without returning data. Cheap. \
                  Call before query_dataset when the result size is unknown.",
    input_schema_json: r#"{"type":"object","properties":{"name":{"type":"string"},"predicates":{"type":"array","items":{"type":"object","properties":{"col":{"type":"string"},"op":{"type":"string"},"val":{}},"required":["col","op"]}}},"required":["name"]}"#,
};

pub static TOOL_SQL: ToolInfo = ToolInfo {
    name: "sql",
    description: "Run a single read-only SELECT (or DESCRIBE) referencing only registered \
                  datasets by name. Joins across datasets, CTEs, subqueries, and expressions \
                  are supported. Not allowed: writes, DDL, file functions, multiple statements. \
                  ALWAYS include a LIMIT. Prefer query_dataset for single-dataset filters; \
                  use this tool for joins and expressions it cannot express. \
                  Call describe_all_datasets first to get the schemas. \
                  NEVER guess column or dataset names; get them from describe_all_datasets.",
    input_schema_json: r#"{"type":"object","properties":{"sql":{"type":"string","description":"Read-only SQL statement."},"max_rows":{"type":"integer","description":"Optional row cap (clamped to server limit)."}},"required":["sql"]}"#,
};

// ---------------------------------------------------------------------------
// Tool list — built at call time so `expose_sql` is honoured at runtime
// ---------------------------------------------------------------------------

/// Return the list of tools to advertise in `tools/list`. The `sql` tool is
/// included only when `mcp.expose_sql && sql.enabled`.
pub fn tool_list(mcp_cfg: &McpConfig, sql_cfg: &SqlConfig) -> Vec<&'static ToolInfo> {
    let mut tools: Vec<&'static ToolInfo> = vec![
        &TOOL_LIST_DATASETS,
        &TOOL_DESCRIBE_DATASET,
        &TOOL_DESCRIBE_ALL_DATASETS,
        &TOOL_QUERY_DATASET,
        &TOOL_COUNT_ROWS,
    ];
    if mcp_cfg.expose_sql && sql_cfg.enabled {
        tools.push(&TOOL_SQL);
    }
    tools
}

// ---------------------------------------------------------------------------
// Dispatch — route a `tools/call` to the right tool impl
// ---------------------------------------------------------------------------

/// Settings cloned from `AppConfig` that the tool dispatcher needs. Kept
/// separate from `McpConfig` so the dispatcher doesn't need the whole config.
#[derive(Debug, Clone)]
pub struct ToolSettings {
    pub default_page_size: u64,
    pub max_page_size: u64,
    pub sql_enabled: bool,
    pub expose_sql: bool,
    pub sql_max_rows: u64,
    /// Per-tool-call timeout in ms (from `server.request_timeout_ms`). `0` = disabled.
    pub request_timeout_ms: u64,
}

/// Dispatch a `tools/call` request. Returns the JSON value for the
/// `tools/call` result field (a `{"content":[...],"isError":bool}` object).
pub async fn dispatch(
    name: &str,
    args: &Value,
    backend: &Arc<dyn Backend>,
    settings: &ToolSettings,
) -> Result<Value, super::protocol::RpcError> {
    match name {
        "list_datasets" => Ok(call_list_datasets(backend)),
        "describe_dataset" => Ok(call_describe_dataset(args, backend).await),
        "describe_all_datasets" => Ok(call_describe_all_datasets(backend).await),
        "query_dataset" => Ok(call_query_dataset(args, backend, settings).await),
        "count_rows" => Ok(call_count_rows(args, backend).await),
        "sql" if settings.expose_sql && settings.sql_enabled => {
            Ok(call_sql(args, backend, settings).await)
        }
        "sql" => Err(super::protocol::RpcError::invalid_params(
            "unknown tool: sql (not enabled on this server)",
        )),
        _ => Err(super::protocol::RpcError::invalid_params(format!(
            "unknown tool: {name}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

fn call_list_datasets(backend: &Arc<dyn Backend>) -> Value {
    let entries = backend.dataset_statuses();
    let json = serde_json::to_string(&serde_json::json!({ "datasets": entries }))
        .unwrap_or_else(|e| format!(r#"{{"error":"{e}"}}"#));
    tool_ok(json)
}

async fn call_describe_dataset(args: &Value, backend: &Arc<dyn Backend>) -> Value {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return tool_err("missing required parameter: name"),
    };

    let schema = match backend.schema(&name) {
        Ok(s) => s,
        Err(e) => return tool_err(format!("dataset '{name}': {e}")),
    };

    let summary = match backend.summary(&name) {
        Ok(s) => s,
        Err(e) => return tool_err(format!("dataset '{name}' summary: {e}")),
    };

    let sample = match backend.sample(&name).await {
        Ok(s) => s,
        Err(e) => return tool_err(format!("dataset '{name}' sample: {e}")),
    };

    let visible_cols = schema_columns_json(&schema);

    let result = serde_json::json!({
        "name": schema.name,
        "rows": summary.rows,
        "columns": visible_cols,
        "sample": serde_json::from_str::<Value>(&sample).unwrap_or(Value::Null),
    });
    tool_ok(result.to_string())
}

async fn call_describe_all_datasets(backend: &Arc<dyn Backend>) -> Value {
    let names = backend.names();
    let mut map = serde_json::Map::new();
    for name in &names {
        match backend.schema(name) {
            Ok(schema) => {
                let cols = schema_columns_json(&schema);
                map.insert(name.clone(), serde_json::json!({ "columns": cols }));
            }
            Err(e) => {
                map.insert(name.clone(), serde_json::json!({ "error": format!("{e}") }));
            }
        }
    }
    tool_ok(Value::Object(map).to_string())
}

async fn call_query_dataset(
    args: &Value,
    backend: &Arc<dyn Backend>,
    settings: &ToolSettings,
) -> Value {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return tool_err("missing required parameter: name"),
    };

    // Deserialise the QueryRequest from `args` (reusing the existing model).
    // We inject the default page_size when it's absent.
    let mut args_obj = match args.as_object().cloned() {
        Some(o) => o,
        None => return tool_err("arguments must be an object"),
    };
    // Remove `name` — QueryRequest doesn't have it.
    args_obj.remove("name");
    // Inject default page_size if absent.
    if !args_obj.contains_key("page_size") {
        args_obj.insert(
            "page_size".into(),
            Value::Number(settings.default_page_size.into()),
        );
    }

    let mut req: QueryRequest = match serde_json::from_value(Value::Object(args_obj)) {
        Ok(r) => r,
        Err(e) => return tool_err(format!("invalid query parameters: {e}")),
    };

    // Clamp page_size.
    let page_size = req.page_size.clamp(1, settings.max_page_size);
    req.page_size = page_size;
    req.page = req.page.max(1);

    // Apply column-access filters.
    if let Ok(schema) = backend.schema(&name)
        && let Err(e) = req.enforce_column_filters(&schema)
    {
        return tool_err(format!("{e}"));
    }

    let page = req.page;
    let data: Value = match backend.query(&name, &req).await {
        Ok(arr) => serde_json::from_str(&arr).unwrap_or(Value::Array(vec![])),
        Err(e) => return tool_err(format!("{e}")),
    };
    let returned = data.as_array().map(|a| a.len()).unwrap_or(0);
    let mut envelope = serde_json::json!({
        "data": data,
        "page": page,
        "page_size": page_size,
        "returned": returned,
    });
    if returned == page_size as usize {
        envelope["note"] = Value::String(
            "more rows may exist; request the next page or add predicates".into(),
        );
    }
    tool_ok(envelope.to_string())
}

async fn call_count_rows(args: &Value, backend: &Arc<dyn Backend>) -> Value {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return tool_err("missing required parameter: name"),
    };

    let predicates_val = args.get("predicates").cloned().unwrap_or(Value::Array(vec![]));
    let count_req: CountRequest = match serde_json::from_value(serde_json::json!({
        "predicates": predicates_val,
    })) {
        Ok(r) => r,
        Err(e) => return tool_err(format!("invalid predicates: {e}")),
    };

    match backend.count(&name, &count_req).await {
        Ok(n) => tool_ok(format!(r#"{{"count":{n}}}"#)),
        Err(e) => tool_err(format!("{e}")),
    }
}

async fn call_sql(
    args: &Value,
    backend: &Arc<dyn Backend>,
    settings: &ToolSettings,
) -> Value {
    let sql = match args.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return tool_err("missing required parameter: sql"),
    };

    let max_rows = match args.get("max_rows").and_then(|v| v.as_u64()) {
        Some(r) => r.clamp(1, settings.sql_max_rows),
        None => settings.sql_max_rows,
    };

    let validated = match crate::sql::validate_and_authorize(&sql, backend) {
        Ok(v) => v,
        Err(e) => return tool_err(format!("{e}")),
    };
    match backend
        .query_sql(&validated.sql, &validated.datasets, max_rows)
        .await
    {
        Ok(arr) => tool_ok(format!(r#"{{"data":{arr},"max_rows":{max_rows}}}"#)),
        Err(e) => tool_err(format!("{e}")),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialise the visible columns of `schema` as a JSON array of objects with
/// `name`, `logical`, `sql_type`, `nullable` fields.
fn schema_columns_json(schema: &DatasetSchema) -> Value {
    let cols: Vec<Value> = schema
        .visible_columns()
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "logical": format!("{:?}", c.logical),
                "sql_type": c.sql_type,
                "nullable": c.nullable,
            })
        })
        .collect();
    Value::Array(cols)
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpConfig, SqlConfig};

    #[test]
    fn tool_list_without_sql() {
        let mcp = McpConfig { expose_sql: false, ..Default::default() };
        let sql = SqlConfig { enabled: false, ..Default::default() };
        let tools = tool_list(&mcp, &sql);
        assert!(!tools.iter().any(|t| t.name == "sql"));
        assert!(tools.iter().any(|t| t.name == "list_datasets"));
        assert!(tools.iter().any(|t| t.name == "count_rows"));
    }

    #[test]
    fn tool_list_with_sql_when_both_enabled() {
        let mcp = McpConfig { expose_sql: true, ..Default::default() };
        let sql = SqlConfig { enabled: true, ..Default::default() };
        let tools = tool_list(&mcp, &sql);
        assert!(tools.iter().any(|t| t.name == "sql"));
    }

    #[test]
    fn tool_list_sql_absent_when_expose_false() {
        let mcp = McpConfig { expose_sql: false, ..Default::default() };
        let sql = SqlConfig { enabled: true, ..Default::default() };
        let tools = tool_list(&mcp, &sql);
        assert!(!tools.iter().any(|t| t.name == "sql"));
    }

    #[test]
    fn tool_list_sql_absent_when_sql_disabled() {
        let mcp = McpConfig { expose_sql: true, ..Default::default() };
        let sql = SqlConfig { enabled: false, ..Default::default() };
        let tools = tool_list(&mcp, &sql);
        assert!(!tools.iter().any(|t| t.name == "sql"));
    }
}
