use duckdb::{Connection, params_from_iter};
use serde_json::Value as JsonValue;

use std::io::Write;

use arrow::datatypes::Schema;
use arrow::ipc::writer::StreamWriter;

use datapress_core::errors::AppError;
use datapress_core::models::{AggPlan, Predicate, QueryRequest};
use datapress_core::schema::DatasetSchema;

// ---------------------------------------------------------------------------
// Parameter binding
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ParamVal {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl duckdb::ToSql for ParamVal {
    fn to_sql(&self) -> duckdb::Result<duckdb::types::ToSqlOutput<'_>> {
        match self {
            ParamVal::Text(s) => s.to_sql(),
            ParamVal::Int(i) => i.to_sql(),
            ParamVal::Float(f) => f.to_sql(),
            ParamVal::Bool(b) => b.to_sql(),
        }
    }
}

fn json_to_param(v: &JsonValue) -> Result<ParamVal, AppError> {
    match v {
        JsonValue::String(s) => Ok(ParamVal::Text(s.clone())),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ParamVal::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(ParamVal::Float(f))
            } else {
                Err(AppError::InvalidValue(n.to_string()))
            }
        }
        JsonValue::Bool(b) => Ok(ParamVal::Bool(*b)),
        other => Err(AppError::InvalidValue(format!("unsupported type: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// SQL helpers
// ---------------------------------------------------------------------------

/// Build the `'key', expr` pairs for a `json_object(…)` call from a list of
/// schema columns. Temporal columns are CAST to VARCHAR.
fn json_obj_pairs<'a, I>(cols: I) -> String
where
    I: IntoIterator<Item = &'a datapress_core::schema::ColumnInfo>,
{
    cols.into_iter()
        .map(|c| {
            let q = DatasetSchema::quote_ident(&c.name);
            if c.logical.needs_cast() {
                format!("'{}', CAST({} AS VARCHAR)", c.name.replace('\'', "''"), q)
            } else {
                format!("'{}', {}", c.name.replace('\'', "''"), q)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the `'key', expr` pairs for the outer `json_object(…)` of a
/// grouped query. Group columns and aggregation outputs are referenced by
/// the aliases produced in the inner aggregation subquery, so each is a
/// real output column (visible to `ORDER BY`) rather than an inline
/// expression buried inside `json_object` — DuckDB cannot order by a name
/// that only exists as a `json_object` key.
fn group_json_obj_pairs(schema: &DatasetSchema, plan: &AggPlan) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(plan.group_cols.len() + plan.aggs.len());
    for name in &plan.group_cols {
        let q = DatasetSchema::quote_ident(name);
        // Group columns inherit the dataset's logical type; temporal cols
        // need a string cast to land cleanly in JSON.
        let needs_cast = schema
            .find(name)
            .map(|c| c.logical.needs_cast())
            .unwrap_or(false);
        if needs_cast {
            parts.push(format!(
                "'{}', CAST({q} AS VARCHAR)",
                name.replace('\'', "''")
            ));
        } else {
            parts.push(format!("'{}', {q}", name.replace('\'', "''")));
        }
    }
    for a in &plan.aggs {
        // Reference the inner subquery's aggregation alias by name.
        let q = DatasetSchema::quote_ident(&a.alias);
        parts.push(format!("'{}', {q}", a.alias.replace('\'', "''")));
    }
    parts.join(", ")
}

/// Build a raw SELECT list (no `json_object`) for an aggregation plan:
/// `group_col1, group_col2, …, <agg_expr> AS <alias>, …`. Used by the
/// Arrow IPC path so the client gets typed columns.
fn agg_select_list(plan: &AggPlan) -> Result<String, AppError> {
    let mut parts: Vec<String> = Vec::with_capacity(plan.group_cols.len() + plan.aggs.len());
    for name in &plan.group_cols {
        parts.push(DatasetSchema::quote_ident(name));
    }
    for a in &plan.aggs {
        let expr = a.sql_expr()?;
        parts.push(format!(
            "{expr} AS {}",
            DatasetSchema::quote_ident(&a.alias)
        ));
    }
    Ok(parts.join(", "))
}

fn stream_as_json_array(
    conn: &Connection,
    sql: &str,
    bind_vals: &[ParamVal],
) -> Result<String, AppError> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params_from_iter(bind_vals.iter()))?;

    let mut buf = String::with_capacity(256 * 1024);
    buf.push('[');
    let mut first = true;
    while let Some(row) = rows.next()? {
        let obj: String = row.get(0)?;
        if !first {
            buf.push(',');
        }
        buf.push_str(&obj);
        first = false;
    }
    buf.push(']');
    Ok(buf)
}

/// Execute a pre-validated raw `SELECT` and return the JSON `data` array.
///
/// The statement has already passed [`datapress_core::sql::validate`]
/// (single read-only query, registered datasets only). Here it is wrapped
/// so each result row is emitted as a JSON object via DuckDB's `to_json`,
/// and an outer `LIMIT` caps the total at `max_rows` regardless of the
/// user's own clauses.
pub fn query_sql(conn: &Connection, sql: &str, max_rows: u64) -> Result<String, AppError> {
    let cap = max_rows.max(1);
    let wrapped = format!("SELECT to_json(_dp) FROM ({sql}) AS _dp LIMIT {cap}");
    stream_as_json_array(conn, &wrapped, &[])
}

/// Execute a pre-validated raw `SELECT` and write the result as an Arrow
/// IPC stream (schema message + batches + EOS) to `writer`.
///
/// Unlike [`query_sql`], the projection is emitted as **raw typed
/// columns** (no `to_json`), so the client receives proper Arrow arrays.
/// An outer `LIMIT` caps the total at `max_rows`. Backs the Arrow
/// content-negotiated branch of `POST /api/v1/sql`.
pub fn query_sql_arrow_write<W: Write>(
    conn: &Connection,
    sql: &str,
    max_rows: u64,
    writer: &mut W,
) -> Result<(), AppError> {
    let cap = max_rows.max(1);
    let wrapped = format!("SELECT * FROM ({sql}) AS _dp LIMIT {cap}");
    let mut stmt = conn.prepare(&wrapped)?;
    let arrow_iter = stmt.query_arrow([])?;
    let schema: Schema = (*arrow_iter.get_schema()).clone();
    let mut w = StreamWriter::try_new(writer, &schema)
        .map_err(|e| AppError::Internal(format!("arrow ipc init: {e}")))?;
    for b in arrow_iter {
        w.write(&b)
            .map_err(|e| AppError::Internal(format!("arrow ipc write: {e}")))?;
    }
    w.finish()
        .map_err(|e| AppError::Internal(format!("arrow ipc finish: {e}")))?;
    Ok(())
}

fn build_where<S: AsRef<str>>(conditions: &[S]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!(
            " WHERE {}",
            conditions
                .iter()
                .map(|c| c.as_ref())
                .collect::<Vec<_>>()
                .join(" AND ")
        )
    }
}

// ---------------------------------------------------------------------------
// Repository — scoped to a single dataset's schema
// ---------------------------------------------------------------------------

pub struct DatasetRepository<'a> {
    conn: &'a Connection,
    schema: &'a DatasetSchema,
    max_page_size: u64,
}

impl<'a> DatasetRepository<'a> {
    pub fn new(conn: &'a Connection, schema: &'a DatasetSchema, max_page_size: u64) -> Self {
        Self {
            conn,
            schema,
            max_page_size: max_page_size.max(1),
        }
    }

    pub fn query(&self, req: &QueryRequest) -> Result<String, AppError> {
        let agg_plan = req.agg_plan(self.schema)?;

        let (limit, offset) = req.effective_limit_offset(self.max_page_size);

        let mut conditions: Vec<String> = Vec::new();
        let mut bind_vals: Vec<ParamVal> = Vec::new();

        for pred in &req.predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }

        let where_clause = build_where(&conditions);
        let having_clause = self.having_clause(req, agg_plan.as_ref(), &mut bind_vals)?;
        let order_clause = match req.order_by_sql(self.schema, agg_plan.as_ref())? {
            Some(s) => format!(" ORDER BY {s}"),
            None => String::new(),
        };
        let table = DatasetSchema::quote_ident(&self.schema.name);

        let sql = if let Some(plan) = &agg_plan {
            // Grouped / aggregated path. Run the aggregation in an inner
            // query so each aggregation alias is a real output column —
            // visible to ORDER BY — then wrap the surviving rows in
            // json_object. Emitting the aliases only inside json_object
            // would hide them from the outer scope and DuckDB would reject
            // `ORDER BY <alias>`.
            let inner_select = agg_select_list(plan)?;
            let group_by = plan
                .group_cols
                .iter()
                .map(|c| DatasetSchema::quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            let pairs = group_json_obj_pairs(self.schema, plan);
            format!(
                "SELECT json_object({pairs}) FROM (\
                    SELECT {inner_select} FROM {table}{where_clause} \
                    GROUP BY {group_by}{having_clause}{order_clause} \
                    LIMIT {limit} OFFSET {offset}\
                 ) sub"
            )
        } else if req.distinct {
            // DISTINCT path: dedup on the raw projected columns inside a
            // subquery, then format each surviving row as a JSON object.
            // This avoids running DISTINCT over the (expensive) json_object
            // string and keeps ORDER BY / LIMIT / OFFSET applied to the
            // deduped set.
            let pairs = self.row_json_obj_pairs(req)?;
            let projection: String = if req.columns.is_empty() {
                "*".into()
            } else {
                req.columns
                    .iter()
                    .map(|n| {
                        self.schema
                            .find(n)
                            .map(|c| DatasetSchema::quote_ident(&c.name))
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            };
            format!(
                "SELECT json_object({pairs}) FROM (\
                    SELECT DISTINCT {projection} FROM {table}{where_clause}{order_clause} \
                    LIMIT {limit} OFFSET {offset}\
                 ) sub"
            )
        } else {
            // Plain row path: one json_object per projected row.
            let pairs = self.row_json_obj_pairs(req)?;
            format!(
                "SELECT json_object({pairs}) FROM {table}{where_clause}{order_clause} \
                 LIMIT {limit} OFFSET {offset}"
            )
        };

        stream_as_json_array(self.conn, &sql, &bind_vals)
    }

    /// Resolve the request projection through the dataset schema and build
    /// the `'key', expr` pairs for a row-shaped `json_object(…)`.
    fn row_json_obj_pairs(&self, req: &QueryRequest) -> Result<String, AppError> {
        let cols: Vec<&datapress_core::schema::ColumnInfo> = if req.columns.is_empty() {
            self.schema.columns.iter().collect()
        } else {
            req.columns
                .iter()
                .map(|n| self.schema.find(n))
                .collect::<Result<_, _>>()?
        };
        Ok(json_obj_pairs(cols))
    }

    /// Same shape as [`query`], but returns the result as an Arrow IPC
    /// stream byte buffer. The projection emits **raw typed columns**
    /// instead of `json_object(...)` rows, so the client receives proper
    /// Arrow arrays rather than a string column of JSON.
    pub fn query_arrow_bytes(&self, req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        let mut buf = Vec::with_capacity(64 * 1024);
        self.query_arrow_write(req, &mut buf)?;
        Ok(buf)
    }

    /// Same query shape as [`Self::query_arrow_bytes`], but writes Arrow IPC
    /// directly to `writer` so HTTP handlers can stream chunks downstream.
    pub fn query_arrow_write<W: Write>(
        &self,
        req: &QueryRequest,
        writer: &mut W,
    ) -> Result<(), AppError> {
        self.query_arrow_write_inner(req, writer, true)
    }

    pub fn query_arrow_write_all<W: Write>(
        &self,
        req: &QueryRequest,
        writer: &mut W,
    ) -> Result<(), AppError> {
        self.query_arrow_write_inner(req, writer, false)
    }

    fn query_arrow_write_inner<W: Write>(
        &self,
        req: &QueryRequest,
        writer: &mut W,
        paged: bool,
    ) -> Result<(), AppError> {
        let agg_plan = req.agg_plan(self.schema)?;

        // Build the SELECT list — column refs for the row path, or
        // `<expr> AS <alias>` items for the aggregation path.
        let projection: String = if let Some(plan) = &agg_plan {
            agg_select_list(plan)?
        } else if req.columns.is_empty() {
            "*".into()
        } else {
            req.columns
                .iter()
                .map(|n| {
                    self.schema
                        .find(n)
                        .map(|c| DatasetSchema::quote_ident(&c.name))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        };

        let limit_clause = if paged {
            let (limit, offset) = req.effective_limit_offset(self.max_page_size);
            format!(" LIMIT {limit} OFFSET {offset}")
        } else {
            req.limit
                .map(|limit| format!(" LIMIT {limit}"))
                .unwrap_or_default()
        };

        let mut conditions: Vec<String> = Vec::new();
        let mut bind_vals: Vec<ParamVal> = Vec::new();
        for pred in &req.predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }

        let where_clause = build_where(&conditions);
        let group_clause = match &agg_plan {
            Some(p) => format!(
                " GROUP BY {}",
                p.group_cols
                    .iter()
                    .map(|c| DatasetSchema::quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            None => String::new(),
        };
        let having_clause = self.having_clause(req, agg_plan.as_ref(), &mut bind_vals)?;
        let order_clause = match req.order_by_sql(self.schema, agg_plan.as_ref())? {
            Some(s) => format!(" ORDER BY {s}"),
            None => String::new(),
        };
        let table = DatasetSchema::quote_ident(&self.schema.name);

        let sql = if req.distinct && agg_plan.is_none() {
            format!(
                "SELECT DISTINCT {projection} FROM {table}{where_clause}{order_clause}{limit_clause}"
            )
        } else {
            format!(
                "SELECT {projection} FROM {table}{where_clause}{group_clause}{having_clause}{order_clause}{limit_clause}"
            )
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let arrow_iter = stmt.query_arrow(params_from_iter(bind_vals.iter()))?;
        let schema: Schema = (*arrow_iter.get_schema()).clone();

        // Encode: one schema message + N batches + EOS.
        let mut w = StreamWriter::try_new(writer, &schema)
            .map_err(|e| AppError::Internal(format!("arrow ipc init: {e}")))?;
        for b in arrow_iter {
            w.write(&b)
                .map_err(|e| AppError::Internal(format!("arrow ipc write: {e}")))?;
        }
        w.finish()
            .map_err(|e| AppError::Internal(format!("arrow ipc finish: {e}")))?;
        Ok(())
    }

    /// Return the number of rows matching `predicates` (empty = all rows).
    pub fn count(&self, predicates: &[Predicate]) -> Result<i64, AppError> {
        let mut conditions: Vec<String> = Vec::new();
        let mut bind_vals: Vec<ParamVal> = Vec::new();
        for pred in predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }
        let where_clause = build_where(&conditions);
        let table = DatasetSchema::quote_ident(&self.schema.name);
        let sql = format!("SELECT COUNT(*) FROM {table}{where_clause}");

        let mut stmt = self.conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(params_from_iter(bind_vals.iter()), |r| r.get(0))?;
        Ok(n)
    }

    /// Encode the entire dataset as a single self-contained Parquet file and
    /// return its bytes.
    ///
    /// Uses DuckDB's native `COPY … TO … (FORMAT parquet)` writer (so the
    /// output carries proper row-group + footer metadata) into a temp file,
    /// then reads it back. Powers the cached `GET /datasets/{name}/parquet`
    /// HTTP endpoint, which a DuckDB `httpfs` client can read over HTTP.
    pub fn parquet_bytes(&self) -> Result<Vec<u8>, AppError> {
        let table = DatasetSchema::quote_ident(&self.schema.name);
        let tmp = tempfile::Builder::new()
            .prefix("datapress-parquet-")
            .suffix(".parquet")
            .tempfile()
            .map_err(|e| AppError::Internal(format!("parquet tempfile: {e}")))?;
        // Single-quote the path for the SQL string literal.
        let path_lit = tmp.path().to_string_lossy().replace('\'', "''");
        let sql = format!(
            "COPY (SELECT * FROM {table}) TO '{path_lit}' (FORMAT parquet, COMPRESSION snappy)"
        );
        self.conn.execute_batch(&sql)?;
        let bytes = std::fs::read(tmp.path())
            .map_err(|e| AppError::Internal(format!("read parquet temp: {e}")))?;
        // `tmp` drops here, removing the temp file.
        Ok(bytes)
    }

    /// Return a single row at offset 0 (used by `/schema` for a discoverable
    /// sample). Returns `null` when the dataset is empty.
    pub fn sample(&self) -> Result<String, AppError> {
        let pairs = json_obj_pairs(self.schema.columns.iter());
        let table = DatasetSchema::quote_ident(&self.schema.name);
        let sql = format!("SELECT json_object({pairs}) FROM {table} LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(row.get::<_, String>(0)?)
        } else {
            Ok("null".into())
        }
    }

    fn apply_predicate(
        &self,
        pred: &Predicate,
        conditions: &mut Vec<String>,
        bind_vals: &mut Vec<ParamVal>,
    ) -> Result<(), AppError> {
        let col = self.schema.find(&pred.col)?;
        let cref = DatasetSchema::quote_ident(&col.name);
        predicate_to_condition(&cref, &col.name, pred, conditions, bind_vals)
    }

    /// Build the ` HAVING …` clause for a grouped query, binding any
    /// literal values onto `bind_vals` (which must already hold the
    /// `WHERE` bindings, since `HAVING` placeholders follow them in the
    /// statement). Returns an empty string when no `HAVING` was requested;
    /// errors if `having` is set without grouping.
    fn having_clause(
        &self,
        req: &QueryRequest,
        plan: Option<&AggPlan>,
        bind_vals: &mut Vec<ParamVal>,
    ) -> Result<String, AppError> {
        let resolved = req.having_plan(plan)?;
        if resolved.is_empty() {
            return Ok(String::new());
        }
        let mut conditions: Vec<String> = Vec::new();
        for (lhs, p) in &resolved {
            predicate_to_condition(lhs, &p.col, p, &mut conditions, bind_vals)?;
        }
        Ok(format!(" HAVING {}", conditions.join(" AND ")))
    }
}

/// Render one predicate as a SQL condition (pushed onto `conditions`) plus
/// its bound values (pushed onto `bind_vals`), against a pre-resolved
/// left-hand-side expression `lhs`. The dataset-`WHERE` path passes a
/// quoted column name; the `HAVING` path passes an aggregate expression
/// such as `COUNT(*)`. `label` is the human-readable name used in error
/// messages.
fn predicate_to_condition(
    lhs: &str,
    label: &str,
    pred: &Predicate,
    conditions: &mut Vec<String>,
    bind_vals: &mut Vec<ParamVal>,
) -> Result<(), AppError> {
    match pred.op.as_str() {
        "is_null" => {
            conditions.push(format!("{lhs} IS NULL"));
        }
        "is_not_null" => {
            conditions.push(format!("{lhs} IS NOT NULL"));
        }
        "in" => {
            let arr = pred
                .val
                .as_ref()
                .and_then(|v| v.as_array())
                .filter(|a| !a.is_empty())
                .ok_or_else(|| {
                    AppError::InvalidValue(format!(
                        "'in' requires a non-empty array for column {label}"
                    ))
                })?;
            let placeholders = arr.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            conditions.push(format!("{lhs} IN ({placeholders})"));
            for v in arr {
                bind_vals.push(json_to_param(v)?);
            }
        }
        op => {
            let sql_op = match op {
                "eq" => "=",
                "neq" => "<>",
                "gt" => ">",
                "gte" => ">=",
                "lt" => "<",
                "lte" => "<=",
                "like" => "LIKE",
                "ilike" => "ILIKE",
                other => return Err(AppError::UnknownOperator(other.into())),
            };
            let val = pred.val.as_ref().ok_or_else(|| {
                AppError::InvalidValue(format!(
                    "operator '{op}' requires a value for column {label}"
                ))
            })?;
            conditions.push(format!("{lhs} {sql_op} ?"));
            bind_vals.push(json_to_param(val)?);
        }
    }
    Ok(())
}
