use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array,
    LargeStringArray, RecordBatch, Scalar, StringArray, UInt32Array,
};
use arrow::compute;
use arrow::compute::kernels::cmp::{eq, gt, gt_eq, lt, lt_eq, neq};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value as JsonValue;

use datafusion::datasource::MemTable;
use datafusion::prelude::SessionContext;

use crate::config::{AppConfig, DatasetConfig, IndexConfig, IndexMode};
use crate::errors::AppError;
use crate::models::{Predicate, QueryRequest};
use crate::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Pre-built equality index: lowercase col name → string-encoded value → sorted row ids.
type EqIndex = HashMap<String, HashMap<String, Vec<u32>>>;

/// Per-dataset state: schema metadata, the resident RecordBatch, and the
/// equality index built per the dataset's `[dataset.index]` policy.
pub struct DatasetState {
    pub schema: DatasetSchema,
    pub data:   RecordBatch,
    pub index:  EqIndex,
}

/// Multi-dataset registry. Each dataset is registered in the shared
/// `SessionContext` under its configured name.
pub struct Store {
    ctx:      SessionContext,
    datasets: HashMap<String, DatasetState>,
}

impl Store {
    /// Load every dataset declared in `cfg`.
    pub fn load(cfg: &AppConfig) -> Result<Self, AppError> {
        let ctx = SessionContext::new();
        let mut datasets = HashMap::with_capacity(cfg.datasets.len());

        for d in &cfg.datasets {
            let state = load_dataset(&ctx, d)?;
            datasets.insert(d.name.clone(), state);
        }
        Ok(Self { ctx, datasets })
    }

    /// Sorted list of dataset names.
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.datasets.keys().map(String::as_str).collect();
        v.sort();
        v
    }

    pub fn dataset(&self, name: &str) -> Result<&DatasetState, AppError> {
        self.datasets
            .get(name)
            .ok_or_else(|| AppError::UnknownColumn(format!("unknown dataset: {name}")))
    }

    /// JSON for the first row of the dataset, or `null` if empty. Used by
    /// `GET /api/datasets/{name}/schema` for discoverability.
    pub fn sample(&self, name: &str) -> Result<String, AppError> {
        let st = self.dataset(name)?;
        if st.data.num_rows() == 0 {
            return Ok("null".into());
        }
        let arr = serialize(&st.data.slice(0, 1))?;
        // strip the outer [] to return a single object
        let trimmed = arr.trim();
        let inner = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            .unwrap_or(trimmed);
        Ok(inner.to_string())
    }

    /// Run a `QueryRequest` against `name`. Empty predicates → O(1) Arrow
    /// slice. Otherwise → DataFusion SQL on the single registered table.
    pub async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        let st = self.dataset(name)?;

        let page      = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset    = ((page - 1) * page_size) as usize;
        let limit     = page_size as usize;

        // No predicates → O(1) raw Arrow slice, no engine overhead.
        if req.predicates.is_empty() {
            let start = offset.min(st.data.num_rows());
            let len   = limit.min(st.data.num_rows() - start);
            return serialize(&project(&st.schema, st.data.slice(start, len), &req.columns)?);
        }

        // Index fast path: if every predicate is eq/in on an indexed column,
        // resolve via the pre-built equality index — far cheaper than a full
        // SIMD scan over the entire RecordBatch.
        if let Some(rows) = try_index(&st.index, &req.predicates) {
            let batch = take_page(&st.data, &rows, offset, limit)?;
            return serialize(&project(&st.schema, batch, &req.columns)?);
        }

        // Fallback: DataFusion SQL — multi-threaded vectorised execution
        // covers all operators (LIKE, ILIKE, ranges, NOT NULL, …).
        let sql     = build_query_sql(&st.schema, req)?;
        let df      = self.ctx.sql(&sql).await?;
        let batches = df.collect().await?;
        if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
            return Ok("[]".to_string());
        }
        let batch = compute::concat_batches(&batches[0].schema(), batches.iter())?;
        serialize(&batch)
    }
}

// ---------------------------------------------------------------------------
// Dataset loading
// ---------------------------------------------------------------------------

fn load_dataset(ctx: &SessionContext, d: &DatasetConfig) -> Result<DatasetState, AppError> {
    let files = d.resolve_files()?;
    let mut all = Vec::new();
    for f in &files {
        let file = std::fs::File::open(f)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", f.display())))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        for batch in reader {
            all.push(batch.map_err(|e| AppError::Internal(e.to_string()))?);
        }
    }
    if all.is_empty() {
        return Err(AppError::Internal(format!(
            "dataset '{}': parquet source is empty", d.name
        )));
    }

    let all       = cast_temporal(all)?;
    let arrow_sch = all[0].schema();
    let data      = compute::concat_batches(&arrow_sch, all.iter())?;

    // Build DatasetSchema from the Arrow schema.
    let columns: Vec<ColumnInfo> = arrow_sch.fields().iter().map(|f| {
        let dt = f.data_type();
        ColumnInfo {
            name:     f.name().clone(),
            logical:  arrow_to_logical(dt),
            sql_type: format!("{dt:?}"),
            nullable: f.is_nullable(),
        }
    }).collect();
    let schema = DatasetSchema::new(&d.name, columns);

    // Build the equality index per the per-dataset policy.
    let index = build_eq_index_with_policy(&data, &d.index);

    // Register as a partitioned MemTable under the dataset's name.
    let n_parts   = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let part_size = (data.num_rows() + n_parts - 1) / n_parts;
    let parts: Vec<Vec<RecordBatch>> = (0..n_parts)
        .map(|i| {
            let start = i * part_size;
            let len   = part_size.min(data.num_rows().saturating_sub(start));
            vec![data.slice(start, len)]
        })
        .filter(|v| v[0].num_rows() > 0)
        .collect();
    let provider = MemTable::try_new(data.schema(), parts)?;
    ctx.register_table(d.name.as_str(), Arc::new(provider))?;

    let mem_mb: usize = data.columns().iter()
        .map(|c| c.get_buffer_memory_size())
        .sum::<usize>() / 1_048_576;
    log::info!(
        "dataset '{}': {} rows, {} cols, {} MB, {} indexed cols",
        d.name, data.num_rows(), schema.columns.len(), mem_mb, index.len()
    );

    Ok(DatasetState { schema, data, index })
}

fn arrow_to_logical(dt: &DataType) -> LogicalType {
    match dt {
        DataType::Boolean => LogicalType::Bool,
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64
            | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
                => LogicalType::Int,
        DataType::Float16 | DataType::Float32 | DataType::Float64
                => LogicalType::Float,
        DataType::Utf8 | DataType::LargeUtf8
                => LogicalType::Utf8,
        DataType::Date32 | DataType::Date64
            | DataType::Time32(_) | DataType::Time64(_)
            | DataType::Timestamp(_, _)
            | DataType::Duration(_)
            | DataType::Interval(_)
                => LogicalType::Temporal,
        _ => LogicalType::Other,
    }
}

// ---------------------------------------------------------------------------
// Per-batch projection
// ---------------------------------------------------------------------------

fn project(schema: &DatasetSchema, batch: RecordBatch, columns: &[String])
    -> Result<RecordBatch, AppError>
{
    if columns.is_empty() {
        return Ok(batch);
    }
    let indices: Vec<usize> = columns.iter()
        .map(|c| schema.find(c).map(|info| schema.by_name[&info.name.to_lowercase()]))
        .collect::<Result<_, _>>()?;
    let fields: Vec<Field>    = indices.iter().map(|&i| batch.schema().field(i).clone()).collect();
    let cols:   Vec<ArrayRef> = indices.iter().map(|&i| batch.column(i).clone()).collect();
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
}

// ---------------------------------------------------------------------------
// SQL builder
// ---------------------------------------------------------------------------

fn build_query_sql(schema: &DatasetSchema, req: &QueryRequest) -> Result<String, AppError> {
    let cols = if req.columns.is_empty() {
        "*".to_string()
    } else {
        req.columns.iter()
            .map(|c| schema.find(c).map(|info| DatasetSchema::quote_ident(&info.name)))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ")
    };

    let page_size = req.page_size.clamp(1, 1000);
    let offset    = (req.page.max(1) - 1) * page_size;

    let clauses: Vec<String> = req.predicates.iter()
        .map(|p| pred_to_sql(schema, p))
        .collect::<Result<_, _>>()?;

    let table = DatasetSchema::quote_ident(&schema.name);
    Ok(format!(
        "SELECT {cols} FROM {table} WHERE {} LIMIT {page_size} OFFSET {offset}",
        clauses.join(" AND ")
    ))
}

fn pred_to_sql(schema: &DatasetSchema, pred: &Predicate) -> Result<String, AppError> {
    let info = schema.find(&pred.col)?;
    let col  = DatasetSchema::quote_ident(&info.name);

    match pred.op.as_str() {
        "is_null"     => return Ok(format!("{col} IS NULL")),
        "is_not_null" => return Ok(format!("{col} IS NOT NULL")),
        _ => {}
    }

    let val = pred.val.as_ref()
        .ok_or_else(|| AppError::InvalidValue(format!("'{}' requires a value", pred.op)))?;

    if pred.op == "in" {
        let items = val.as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| AppError::InvalidValue("'in' needs a non-empty array".into()))?;
        let lits: Vec<String> = items.iter().map(json_to_sql_lit).collect::<Result<_, _>>()?;
        return Ok(format!("{col} IN ({})", lits.join(", ")));
    }

    let sql_op = match pred.op.as_str() {
        "eq"    => "=",
        "neq"   => "!=",
        "gt"    => ">",
        "gte"   => ">=",
        "lt"    => "<",
        "lte"   => "<=",
        "like"  => "LIKE",
        "ilike" => "ILIKE",
        other   => return Err(AppError::UnknownOperator(other.into())),
    };
    Ok(format!("{col} {sql_op} {}", json_to_sql_lit(val)?))
}

fn json_to_sql_lit(val: &JsonValue) -> Result<String, AppError> {
    match val {
        JsonValue::String(s) => Ok(format!("'{}'", s.replace('\'', "''"))),
        JsonValue::Number(n) => Ok(n.to_string()),
        JsonValue::Bool(b)   => Ok(b.to_string()),
        JsonValue::Null      => Ok("NULL".to_string()),
        _ => Err(AppError::InvalidValue("unsupported literal type in predicate".into())),
    }
}

// ---------------------------------------------------------------------------
// Equality index — built once at startup, queried on every predicate request
// ---------------------------------------------------------------------------

fn json_index_key(val: &JsonValue) -> Option<String> {
    match val {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b)   => Some(b.to_string()),
        _ => None,
    }
}

fn intersect_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Equal   => { out.push(a[i]); i += 1; j += 1; }
            Ordering::Less    => i += 1,
            Ordering::Greater => j += 1,
        }
    }
    out
}

fn union_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less    => { out.push(a[i]); i += 1; }
            Ordering::Greater => { out.push(b[j]); j += 1; }
            Ordering::Equal   => { out.push(a[i]); i += 1; j += 1; }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn try_index(index: &EqIndex, predicates: &[Predicate]) -> Option<Vec<u32>> {
    if predicates.is_empty() || index.is_empty() { return None; }

    let mut result: Option<Vec<u32>> = None;
    for pred in predicates {
        let col_lower = pred.col.to_lowercase();
        let col_map   = index.get(&col_lower)?;

        let rows: Vec<u32> = match pred.op.as_str() {
            "eq" => {
                let key = json_index_key(pred.val.as_ref()?)?;
                col_map.get(&key).cloned().unwrap_or_default()
            }
            "in" => {
                let items = pred.val.as_ref()?.as_array()?;
                let mut merged: Vec<u32> = Vec::new();
                for item in items {
                    if let Some(r) = col_map.get(&json_index_key(item)?) {
                        merged = union_sorted(&merged, r);
                    }
                }
                merged
            }
            _ => return None,
        };

        result = Some(match result {
            None    => rows,
            Some(r) => intersect_sorted(&r, &rows),
        });
    }
    result
}

fn take_page(data: &RecordBatch, rows: &[u32], offset: usize, limit: usize)
    -> Result<RecordBatch, AppError>
{
    let start = offset.min(rows.len());
    let len   = limit.min(rows.len() - start);
    let idx   = UInt32Array::from(rows[start..start + len].to_vec());
    let cols: Vec<ArrayRef> = data.columns().iter()
        .map(|c| arrow::compute::take(c.as_ref(), &idx, None::<arrow::compute::TakeOptions>)
                 .map_err(AppError::from))
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(data.schema(), cols).map_err(AppError::from)
}

/// Build the equality index per the dataset's policy.
fn build_eq_index_with_policy(data: &RecordBatch, cfg: &IndexConfig) -> EqIndex {
    use rayon::prelude::*;

    if cfg.mode == IndexMode::None {
        return EqIndex::new();
    }

    let allow: Option<HashMap<String, ()>> = if cfg.mode == IndexMode::List {
        Some(cfg.columns.iter().map(|c| (c.to_lowercase(), ())).collect())
    } else {
        None
    };

    let max_card = if cfg.mode == IndexMode::Auto { Some(cfg.max_cardinality) } else { None };
    let n = data.num_rows();

    data.schema().fields().par_iter().enumerate()
        .filter_map(|(ci, field)| {
            let col_lower = field.name().to_lowercase();
            if let Some(a) = &allow {
                if !a.contains_key(&col_lower) { return None; }
            }
            let col = data.column(ci);
            let mut map: HashMap<String, Vec<u32>> = HashMap::new();

            macro_rules! index_col {
                ($arr_ty:ty) => {{
                    let arr = col.as_any().downcast_ref::<$arr_ty>()?;
                    for row in 0..n {
                        if arr.is_null(row) { continue; }
                        let key = arr.value(row).to_string();
                        if let Some(v) = map.get_mut(&key) {
                            v.push(row as u32);
                        } else {
                            if let Some(mc) = max_card {
                                if map.len() >= mc { return None; }
                            }
                            map.insert(key, vec![row as u32]);
                        }
                    }
                }};
            }

            match field.data_type() {
                DataType::Utf8    => index_col!(StringArray),
                DataType::Boolean => index_col!(BooleanArray),
                DataType::Int8    => index_col!(Int8Array),
                DataType::Int16   => index_col!(Int16Array),
                DataType::Int32   => index_col!(Int32Array),
                DataType::Int64   => index_col!(Int64Array),
                _ => return None,
            }

            Some((col_lower, map))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Startup: cast temporal columns to Utf8 (one-time cost)
// ---------------------------------------------------------------------------

fn cast_temporal(batches: Vec<RecordBatch>) -> Result<Vec<RecordBatch>, AppError> {
    let schema = batches[0].schema();
    let ts: Vec<usize> = schema.fields().iter().enumerate()
        .filter_map(|(i, f)| match f.data_type() {
            DataType::Timestamp(_, _) | DataType::Date32 | DataType::Date64 => Some(i),
            _ => None,
        })
        .collect();
    if ts.is_empty() {
        return Ok(batches);
    }
    let new_fields: Vec<Field> = schema.fields().iter().enumerate()
        .map(|(i, f)| if ts.contains(&i) { Field::new(f.name(), DataType::Utf8, f.is_nullable()) }
                      else               { f.as_ref().clone() })
        .collect();
    let new_schema = Arc::new(Schema::new(new_fields));
    batches.into_iter()
        .map(|b| {
            let cols: Vec<ArrayRef> = b.columns().iter().enumerate()
                .map(|(i, c)| if ts.contains(&i) {
                    compute::cast(c.as_ref(), &DataType::Utf8).map_err(AppError::from)
                } else { Ok(c.clone()) })
                .collect::<Result<_, _>>()?;
            RecordBatch::try_new(new_schema.clone(), cols).map_err(AppError::from)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Compute helpers — retained for symmetry; reserved for future inline scan
// path. Currently the engine fallback handles all non-index queries.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum CmpOp { Eq, Neq, Gt, Gte, Lt, Lte, Like, ILike }

#[allow(dead_code)]
fn eq_str(col: &ArrayRef, val: &str) -> Result<BooleanArray, AppError> {
    let arr = col.as_any().downcast_ref::<StringArray>()
        .ok_or_else(|| AppError::InvalidValue("equality: column is not a string".into()))?;
    let s = Scalar::new(StringArray::from(vec![val]));
    Ok(eq(arr, &s)?)
}

#[allow(dead_code)]
fn cmp_scalar(col: &ArrayRef, op: CmpOp, val: &JsonValue) -> Result<BooleanArray, AppError> {
    macro_rules! num_cmp {
        ($arr_type:ty, $cast:ty) => {{
            let n   = val.as_f64().ok_or_else(|| AppError::InvalidValue("expected number".into()))? as $cast;
            let arr = col.as_any().downcast_ref::<$arr_type>().unwrap();
            let s   = Scalar::new(<$arr_type>::from(vec![n]));
            Ok(match op {
                CmpOp::Eq    => eq(arr, &s)?,
                CmpOp::Neq   => neq(arr, &s)?,
                CmpOp::Gt    => gt(arr, &s)?,
                CmpOp::Gte   => gt_eq(arr, &s)?,
                CmpOp::Lt    => lt(arr, &s)?,
                CmpOp::Lte   => lt_eq(arr, &s)?,
                CmpOp::Like | CmpOp::ILike =>
                    return Err(AppError::InvalidValue("LIKE requires a string column".into())),
            })
        }};
    }
    match col.data_type() {
        DataType::Utf8 => {
            let s   = val.as_str().ok_or_else(|| AppError::InvalidValue("expected string".into()))?;
            let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
            let sc  = Scalar::new(StringArray::from(vec![s]));
            Ok(match op {
                CmpOp::Eq    => eq(arr, &sc)?,
                CmpOp::Neq   => neq(arr, &sc)?,
                CmpOp::Gt    => gt(arr, &sc)?,
                CmpOp::Gte   => gt_eq(arr, &sc)?,
                CmpOp::Lt    => lt(arr, &sc)?,
                CmpOp::Lte   => lt_eq(arr, &sc)?,
                CmpOp::Like  => compute::like(arr, &sc)?,
                CmpOp::ILike => compute::ilike(arr, &sc)?,
            })
        }
        DataType::Int8    => num_cmp!(Int8Array,   i8),
        DataType::Int16   => num_cmp!(Int16Array,  i16),
        DataType::Int32   => num_cmp!(Int32Array,  i32),
        DataType::Int64   => num_cmp!(Int64Array,  i64),
        DataType::Float32 => num_cmp!(Float32Array, f32),
        DataType::Float64 => num_cmp!(Float64Array, f64),
        dt => Err(AppError::InvalidValue(format!("unsupported type for comparison: {dt:?}"))),
    }
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

pub fn serialize(batch: &RecordBatch) -> Result<String, AppError> {
    let schema = batch.schema();
    let n_cols = schema.fields().len();
    let n_rows = batch.num_rows();

    let keys: Vec<Vec<u8>> = schema.fields().iter().map(|f| {
        let mut k = Vec::with_capacity(f.name().len() + 3);
        k.push(b'"');
        k.extend_from_slice(f.name().as_bytes());
        k.extend_from_slice(b"\":");
        k
    }).collect();

    let mut buf: Vec<u8> = Vec::with_capacity(n_rows.max(1) * 300);
    buf.push(b'[');

    for row in 0..n_rows {
        if row > 0 { buf.push(b','); }
        buf.push(b'{');
        for i in 0..n_cols {
            if i > 0 { buf.push(b','); }
            buf.extend_from_slice(&keys[i]);
            let col = batch.column(i);
            if col.is_null(row) {
                buf.extend_from_slice(b"null");
            } else {
                write_value(&mut buf, col.as_ref(), row);
            }
        }
        buf.push(b'}');
    }

    buf.push(b']');
    Ok(unsafe { String::from_utf8_unchecked(buf) })
}

#[inline]
fn write_value(buf: &mut Vec<u8>, col: &dyn Array, row: usize) {
    match col.data_type() {
        DataType::Utf8 =>
            write_str(buf, col.as_any().downcast_ref::<StringArray>().unwrap().value(row)),
        DataType::LargeUtf8 =>
            write_str(buf, col.as_any().downcast_ref::<LargeStringArray>().unwrap().value(row)),
        DataType::Boolean => {
            let v = col.as_any().downcast_ref::<BooleanArray>().unwrap().value(row);
            buf.extend_from_slice(if v { b"true" } else { b"false" });
        }
        DataType::Int8   => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int8Array>() .unwrap().value(row)).as_bytes()); }
        DataType::Int16  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int16Array>().unwrap().value(row)).as_bytes()); }
        DataType::Int32  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int32Array>().unwrap().value(row)).as_bytes()); }
        DataType::Int64  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int64Array>().unwrap().value(row)).as_bytes()); }
        DataType::Float32 => {
            let v = col.as_any().downcast_ref::<Float32Array>().unwrap().value(row);
            if v.is_finite() { let mut b = ryu::Buffer::new(); buf.extend_from_slice(b.format_finite(v).as_bytes()); }
            else { buf.extend_from_slice(b"null"); }
        }
        DataType::Float64 => {
            let v = col.as_any().downcast_ref::<Float64Array>().unwrap().value(row);
            if v.is_finite() { let mut b = ryu::Buffer::new(); buf.extend_from_slice(b.format_finite(v).as_bytes()); }
            else { buf.extend_from_slice(b"null"); }
        }
        _ => buf.extend_from_slice(b"null"),
    }
}

#[inline]
fn write_str(buf: &mut Vec<u8>, s: &str) {
    buf.push(b'"');
    for &byte in s.as_bytes() {
        match byte {
            b'"'        => buf.extend_from_slice(b"\\\""),
            b'\\'       => buf.extend_from_slice(b"\\\\"),
            b'\n'       => buf.extend_from_slice(b"\\n"),
            b'\r'       => buf.extend_from_slice(b"\\r"),
            b'\t'       => buf.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                buf.extend_from_slice(b"\\u00");
                const HEX: &[u8] = b"0123456789abcdef";
                buf.push(HEX[(byte >> 4) as usize]);
                buf.push(HEX[(byte & 0xf) as usize]);
            }
            b => buf.push(b),
        }
    }
    buf.push(b'"');
}
