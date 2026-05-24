use duckdb::{Connection, params_from_iter};
use serde_json::Value as JsonValue;

use datapress_core::errors::AppError;
use datapress_core::models::{Predicate, QueryRequest};
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
            if let Some(i) = n.as_i64()      { Ok(ParamVal::Int(i)) }
            else if let Some(f) = n.as_f64() { Ok(ParamVal::Float(f)) }
            else                              { Err(AppError::InvalidValue(n.to_string())) }
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
        if !first { buf.push(','); }
        buf.push_str(&obj);
        first = false;
    }
    buf.push(']');
    Ok(buf)
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

// ---------------------------------------------------------------------------
// Repository — scoped to a single dataset's schema
// ---------------------------------------------------------------------------

pub struct DatasetRepository<'a> {
    conn:   &'a Connection,
    schema: &'a DatasetSchema,
}

impl<'a> DatasetRepository<'a> {
    pub fn new(conn: &'a Connection, schema: &'a DatasetSchema) -> Self {
        Self { conn, schema }
    }

    pub fn query(&self, req: &QueryRequest) -> Result<String, AppError> {
        // Resolve projection through the dataset schema.
        let cols: Vec<&datapress_core::schema::ColumnInfo> = if req.columns.is_empty() {
            self.schema.columns.iter().collect()
        } else {
            req.columns
                .iter()
                .map(|n| self.schema.find(n))
                .collect::<Result<_, _>>()?
        };

        let page      = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset    = (page - 1) * page_size;

        let mut conditions: Vec<String>   = Vec::new();
        let mut bind_vals:  Vec<ParamVal> = Vec::new();

        for pred in &req.predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }

        let where_clause = build_where(&conditions);
        let pairs        = json_obj_pairs(cols);
        let table        = DatasetSchema::quote_ident(&self.schema.name);
        let sql = format!(
            "SELECT json_object({pairs}) FROM {table}{where_clause} \
             LIMIT {page_size} OFFSET {offset}"
        );

        stream_as_json_array(self.conn, &sql, &bind_vals)
    }

    /// Return the number of rows matching `predicates` (empty = all rows).
    pub fn count(&self, predicates: &[Predicate]) -> Result<i64, AppError> {
        let mut conditions: Vec<String>   = Vec::new();
        let mut bind_vals:  Vec<ParamVal> = Vec::new();
        for pred in predicates {
            self.apply_predicate(pred, &mut conditions, &mut bind_vals)?;
        }
        let where_clause = build_where(&conditions);
        let table = DatasetSchema::quote_ident(&self.schema.name);
        let sql   = format!("SELECT COUNT(*) FROM {table}{where_clause}");

        let mut stmt = self.conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(params_from_iter(bind_vals.iter()), |r| r.get(0))?;
        Ok(n)
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
        pred:       &Predicate,
        conditions: &mut Vec<String>,
        bind_vals:  &mut Vec<ParamVal>,
    ) -> Result<(), AppError> {
        let col = self.schema.find(&pred.col)?;
        let cref = DatasetSchema::quote_ident(&col.name);

        match pred.op.as_str() {
            "is_null"     => { conditions.push(format!("{cref} IS NULL")); }
            "is_not_null" => { conditions.push(format!("{cref} IS NOT NULL")); }
            "in" => {
                let arr = pred.val.as_ref()
                    .and_then(|v| v.as_array())
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| AppError::InvalidValue(
                        format!("'in' requires a non-empty array for column {}", col.name),
                    ))?;
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
                    other   => return Err(AppError::UnknownOperator(other.into())),
                };
                let val = pred.val.as_ref().ok_or_else(|| AppError::InvalidValue(
                    format!("operator '{op}' requires a value for column {}", col.name),
                ))?;
                conditions.push(format!("{cref} {sql_op} ?"));
                bind_vals.push(json_to_param(val)?);
            }
        }
        Ok(())
    }
}
