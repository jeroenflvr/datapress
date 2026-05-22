use duckdb::{params_from_iter, Connection};
use serde_json::Value as JsonValue;

use crate::errors::AppError;
use crate::models::{Predicate, QueryRequest};
use crate::schema::{find_column, col_ref, ALL_COLUMNS};

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
            ParamVal::Text(s)  => s.to_sql(),
            ParamVal::Int(i)   => i.to_sql(),
            ParamVal::Float(f) => f.to_sql(),
            ParamVal::Bool(b)  => b.to_sql(),
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

/// Build the `'key', expr` pairs for a `json_object(…)` call.
fn json_obj_pairs(col_specs: &[(&str, bool)]) -> String {
    col_specs
        .iter()
        .map(|(name, is_ts)| {
            if *is_ts {
                format!("'{}', CAST(\"{}\" AS VARCHAR)", name, name)
            } else {
                format!("'{}', \"{}\"", name, name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Execute `sql` and stream each row's `json_object` string into a JSON array.
/// Rows are streamed one at a time — no aggregate allocation in DuckDB memory.
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

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

pub struct AccidentsRepository<'a> {
    conn: &'a Connection,
}

impl<'a> AccidentsRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Simple paginated scan with optional equality filters.
    pub fn get_page(
        &self,
        page: u64,
        page_size: u64,
        state: Option<&str>,
        severity: Option<i64>,
        city: Option<&str>,
    ) -> Result<String, AppError> {
        let offset = (page - 1) * page_size;

        let mut conditions: Vec<&'static str> = Vec::new();
        let mut bind_vals: Vec<ParamVal> = Vec::new();

        if let Some(s) = state {
            conditions.push("State = ?");
            bind_vals.push(ParamVal::Text(s.to_owned()));
        }
        if let Some(sev) = severity {
            conditions.push("Severity = ?");
            bind_vals.push(ParamVal::Int(sev));
        }
        if let Some(c) = city {
            conditions.push("City = ?");
            bind_vals.push(ParamVal::Text(c.to_owned()));
        }

        let where_clause = build_where(&conditions);
        let pairs = json_obj_pairs(ALL_COLUMNS);
        let sql = format!(
            "SELECT json_object({pairs}) FROM accidents{where_clause} \
             LIMIT {page_size} OFFSET {offset}"
        );

        stream_as_json_array(self.conn, &sql, &bind_vals)
    }

    /// Flexible query: caller picks columns and supplies arbitrary predicates.
    pub fn query(&self, req: &QueryRequest) -> Result<String, AppError> {
        let col_specs: Vec<(&'static str, bool)> = if req.columns.is_empty() {
            ALL_COLUMNS.to_vec()
        } else {
            req.columns
                .iter()
                .map(|name| {
                    find_column(name)
                        .ok_or_else(|| AppError::UnknownColumn(name.clone()))
                })
                .collect::<Result<_, _>>()?
        };

        let page = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset = (page - 1) * page_size;

        let mut conditions: Vec<String> = Vec::new();
        let mut bind_vals: Vec<ParamVal> = Vec::new();

        for pred in &req.predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }

        let where_clause = build_where(&conditions);
        let pairs = json_obj_pairs(&col_specs);
        let sql = format!(
            "SELECT json_object({pairs}) FROM accidents{where_clause} \
             LIMIT {page_size} OFFSET {offset}"
        );

        stream_as_json_array(self.conn, &sql, &bind_vals)
    }

    fn apply_predicate(
        &self,
        pred: &Predicate,
        conditions: &mut Vec<String>,
        bind_vals: &mut Vec<ParamVal>,
    ) -> Result<(), AppError> {
        let (col_name, _) = find_column(&pred.col)
            .ok_or_else(|| AppError::UnknownColumn(pred.col.clone()))?;
        let cref = col_ref(col_name);

        match pred.op.as_str() {
            "is_null" => {
                conditions.push(format!("{cref} IS NULL"));
            }
            "is_not_null" => {
                conditions.push(format!("{cref} IS NOT NULL"));
            }
            "in" => {
                let arr = pred
                    .val
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| {
                        AppError::InvalidValue(format!(
                            "'in' requires a non-empty array for column {col_name}"
                        ))
                    })?;
                let placeholders = arr.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                conditions.push(format!("{cref} IN ({placeholders})"));
                for v in arr {
                    bind_vals.push(json_to_param(v)?);
                }
            }
            op => {
                let sql_op = match op {
                    "eq"    => "=",
                    "neq"   => "<>",
                    "gt"    => ">",
                    "gte"   => ">=",
                    "lt"    => "<",
                    "lte"   => "<=",
                    "like"  => "LIKE",
                    "ilike" => "ILIKE",
                    other   => return Err(AppError::UnknownOperator(other.to_string())),
                };
                let val = pred.val.as_ref().ok_or_else(|| {
                    AppError::InvalidValue(format!(
                        "operator '{op}' requires a value for column {col_name}"
                    ))
                })?;
                conditions.push(format!("{cref} {sql_op} ?"));
                bind_vals.push(json_to_param(val)?);
            }
        }
        Ok(())
    }
}

fn build_where<S: AsRef<str>>(conditions: &[S]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!(
            " WHERE {}",
            conditions.iter().map(|c| c.as_ref()).collect::<Vec<_>>().join(" AND ")
        )
    }
}
