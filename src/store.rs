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

use crate::errors::AppError;
use crate::models::{Predicate, QueryRequest};

// ---------------------------------------------------------------------------
// Store – all rows in one RecordBatch; filters via Arrow SIMD compute kernels
// ---------------------------------------------------------------------------

/// Pre-built equality index: lowercase col name → string-encoded value → sorted row ids.
type EqIndex = HashMap<String, HashMap<String, Vec<u32>>>;

pub struct Store {
    data:    RecordBatch,
    /// Lowercase column name → column index.
    col_idx: HashMap<String, usize>,
    /// Equality index for O(1) eq/in lookup in get_page().
    index:   EqIndex,
    /// DataFusion context with the data registered as table "accidents".
    ctx:     SessionContext,
}

impl Store {
    /// Load the Parquet file fully into memory.
    /// Temporal columns are cast to Utf8 once so serialisation stays uniform.
    pub fn load(path: &str) -> Result<Self, AppError> {
        let file = std::fs::File::open(path)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        let batches: Vec<RecordBatch> = reader
            .collect::<Result<_, _>>()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        if batches.is_empty() {
            return Err(AppError::Internal("parquet file is empty".into()));
        }

        let batches = cast_temporal(batches)?;
        let schema  = batches[0].schema();
        // Single contiguous RecordBatch: O(1) slice, one-pass SIMD filter.
        let data = compute::concat_batches(&schema, batches.iter())?;

        let col_idx = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name().to_lowercase(), i))
            .collect();

        let mem_mb: usize = data.columns().iter()
            .map(|c| c.get_buffer_memory_size())
            .sum::<usize>()
            / 1_048_576;
        let index = build_eq_index(&data);

        // Register partitioned in-memory table for DataFusion predicate queries.
        let n_parts   = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let part_size = (data.num_rows() + n_parts - 1) / n_parts;
        let parts: Vec<Vec<RecordBatch>> = (0..n_parts)
            .map(|i| {
                let start = i * part_size;
                let len   = part_size.min(data.num_rows() - start);
                vec![data.slice(start, len)]
            })
            .filter(|v| v[0].num_rows() > 0)
            .collect();
        let ctx = SessionContext::new();
        let provider = MemTable::try_new(data.schema(), parts)?;
        ctx.register_table("accidents", Arc::new(provider))?;

        log::info!(
            "loaded {} rows, {} cols, {}MB resident, {} indexed cols, {} df partitions",
            data.num_rows(), schema.fields().len(), mem_mb, index.len(), n_parts
        );
        Ok(Self { data, col_idx, index, ctx })
    }

    // -----------------------------------------------------------------------
    // GET /api/accidents  – equality filters + pagination
    // -----------------------------------------------------------------------

    pub fn get_page(
        &self,
        page:      u64,
        page_size: u64,
        state:     Option<&str>,
        severity:  Option<i64>,
        city:      Option<&str>,
    ) -> Result<String, AppError> {
        let offset = ((page - 1) * page_size) as usize;
        let limit  = page_size as usize;

        // Fast path: no predicates → O(1) slice, zero compute.
        if state.is_none() && severity.is_none() && city.is_none() {
            return self.slice_json(offset, limit);
        }

        // Index fast path: O(1) lookup + O(page_size) take.
        let mut preds: Vec<Predicate> = Vec::new();
        if let Some(v) = state    { preds.push(Predicate { col: "state".into(),    op: "eq".into(), val: Some(JsonValue::String(v.into())) }); }
        if let Some(v) = severity { preds.push(Predicate { col: "severity".into(), op: "eq".into(), val: Some(JsonValue::Number(serde_json::Number::from(v))) }); }
        if let Some(v) = city     { preds.push(Predicate { col: "city".into(),     op: "eq".into(), val: Some(JsonValue::String(v.into())) }); }

        if let Some(rows) = self.try_index(&preds) {
            return serialize(&self.take_page(&rows, offset, limit)?);
        }

        // Fallback: full SIMD scan.
        let mut mask: Option<BooleanArray> = None;
        for pred in &preds {
            mask = Some(and_opt(mask, self.eval_pred(pred)?)?);
        }
        let filtered = compute::filter_record_batch(&self.data, &mask.unwrap())?;
        let start = offset.min(filtered.num_rows());
        let len   = limit.min(filtered.num_rows() - start);
        serialize(&filtered.slice(start, len))
    }

    // -----------------------------------------------------------------------
    // POST /api/accidents/query – arbitrary predicates + column projection
    // -----------------------------------------------------------------------

    pub async fn query(&self, req: &QueryRequest) -> Result<String, AppError> {
        let page      = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset    = ((page - 1) * page_size) as usize;
        let limit     = page_size as usize;

        // No predicates → O(1) raw Arrow slice, no engine overhead.
        if req.predicates.is_empty() {
            let start = offset.min(self.data.num_rows());
            let len   = limit.min(self.data.num_rows() - start);
            return serialize(&self.project(self.data.slice(start, len), &req.columns)?);
        }

        // Predicates → DataFusion SQL: multi-threaded vectorised execution
        // handles all operators (eq, gte, like, ilike, in, …) correctly.
        let sql     = build_query_sql(req)?;
        let df      = self.ctx.sql(&sql).await?;
        let batches = df.collect().await?;
        if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
            return Ok("[]".to_string());
        }
        let batch = compute::concat_batches(&batches[0].schema(), batches.iter())?;
        serialize(&batch)
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn col_req(&self, lower: &str) -> Result<usize, AppError> {
        self.col_idx.get(lower).copied()
            .ok_or_else(|| AppError::UnknownColumn(lower.into()))
    }

    fn slice_json(&self, offset: usize, limit: usize) -> Result<String, AppError> {
        let start = offset.min(self.data.num_rows());
        let len   = limit.min(self.data.num_rows() - start);
        serialize(&self.data.slice(start, len))
    }

    fn project(&self, batch: RecordBatch, columns: &[String]) -> Result<RecordBatch, AppError> {
        if columns.is_empty() {
            return Ok(batch);
        }
        let indices: Vec<usize> = columns
            .iter()
            .map(|c| {
                self.col_idx.get(c.to_lowercase().as_str()).copied()
                    .ok_or_else(|| AppError::UnknownColumn(c.clone()))
            })
            .collect::<Result<_, _>>()?;
        let fields: Vec<Field>    = indices.iter().map(|&i| batch.schema().field(i).clone()).collect();
        let cols:   Vec<ArrayRef> = indices.iter().map(|&i| batch.column(i).clone()).collect();
        Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
    }

    fn eval_pred(&self, pred: &Predicate) -> Result<BooleanArray, AppError> {
        let col_i = self.col_idx.get(pred.col.to_lowercase().as_str()).copied()
            .ok_or_else(|| AppError::UnknownColumn(pred.col.clone()))?;
        let col = self.data.column(col_i);

        match pred.op.as_str() {
            "is_null"     => return Ok(compute::is_null(col.as_ref())?),
            "is_not_null" => return Ok(compute::is_not_null(col.as_ref())?),
            _ => {}
        }

        let val = pred.val.as_ref()
            .ok_or_else(|| AppError::InvalidValue(format!("'{}' requires a value", pred.op)))?;

        if pred.op == "in" {
            let items = val.as_array()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| AppError::InvalidValue("'in' needs a non-empty array".into()))?;
            return in_list(col, items);
        }

        let op = match pred.op.as_str() {
            "eq"    => CmpOp::Eq,
            "neq"   => CmpOp::Neq,
            "gt"    => CmpOp::Gt,
            "gte"   => CmpOp::Gte,
            "lt"    => CmpOp::Lt,
            "lte"   => CmpOp::Lte,
            "like"  => CmpOp::Like,
            "ilike" => CmpOp::ILike,
            other   => return Err(AppError::UnknownOperator(other.into())),
        };
        cmp_scalar(col, op, val)
    }

    // -----------------------------------------------------------------------
    // Index helpers
    // -----------------------------------------------------------------------

    /// Gather `limit` rows starting at `offset` from a pre-computed index row list.
    /// O(page_size) — far cheaper than filter_record_batch on the full dataset.
    fn take_page(&self, rows: &[u32], offset: usize, limit: usize) -> Result<RecordBatch, AppError> {
        let start = offset.min(rows.len());
        let len   = limit.min(rows.len() - start);
        let idx   = UInt32Array::from(rows[start..start + len].to_vec());
        let cols: Vec<ArrayRef> = self.data.columns()
            .iter()
            .map(|c| arrow::compute::take(c.as_ref(), &idx, None::<arrow::compute::TakeOptions>)
                     .map_err(AppError::from))
            .collect::<Result<_, _>>()?;
        RecordBatch::try_new(self.data.schema(), cols).map_err(AppError::from)
    }

    /// Try to resolve `predicates` entirely from the pre-built equality index.
    /// Returns `None` if any predicate references a non-indexed column or uses
    /// an operator other than `eq` / `in` — caller falls back to the SIMD scan.
    fn try_index(&self, predicates: &[Predicate]) -> Option<Vec<u32>> {
        if predicates.is_empty() { return None; }

        let mut result: Option<Vec<u32>> = None;
        for pred in predicates {
            let col_lower = pred.col.to_lowercase();
            let col_map   = self.index.get(&col_lower)?; // not indexed → None

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
                _ => return None, // unsupported op → use SIMD scan
            };

            result = Some(match result {
                None    => rows,
                Some(r) => intersect_sorted(&r, &rows),
            });
        }

        result
    }
}

// ---------------------------------------------------------------------------
// SQL builder – converts QueryRequest predicates to a DataFusion SQL string
// ---------------------------------------------------------------------------

fn build_query_sql(req: &QueryRequest) -> Result<String, AppError> {
    let cols = if req.columns.is_empty() {
        "*".to_string()
    } else {
        req.columns.iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let page_size = req.page_size.clamp(1, 1000);
    let offset    = (req.page.max(1) - 1) * page_size;

    let clauses: Vec<String> = req.predicates.iter()
        .map(pred_to_sql)
        .collect::<Result<_, _>>()?;

    Ok(format!(
        "SELECT {cols} FROM accidents WHERE {} LIMIT {page_size} OFFSET {offset}",
        clauses.join(" AND ")
    ))
}

fn pred_to_sql(pred: &Predicate) -> Result<String, AppError> {
    // Quote the column identifier to allow special characters (parens, spaces …).
    let col = format!("\"{}\"", pred.col.replace('"', "\"\""));

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
        let lits: Vec<String> = items.iter()
            .map(json_to_sql_lit)
            .collect::<Result<_, _>>()?;
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
// Equality index – built once at startup, queried on every predicate request
// ---------------------------------------------------------------------------

/// Encode a JSON scalar as the string key used in the equality index.
fn json_index_key(val: &JsonValue) -> Option<String> {
    match val {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b)   => Some(b.to_string()),
        _ => None,
    }
}

/// Merge-intersect two sorted `u32` slices.
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

/// Merge-union two sorted `u32` slices (deduplicating equal values).
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

/// Build an equality index in parallel (one rayon task per column).
/// Columns with cardinality > MAX_CARD are skipped (floats, free-text, IDs).
fn build_eq_index(data: &RecordBatch) -> EqIndex {
    use rayon::prelude::*;
    const MAX_CARD: usize = 100_000;
    let n = data.num_rows();

    data.schema().fields().par_iter().enumerate()
        .filter_map(|(ci, field)| {
            let col       = data.column(ci);
            let col_lower = field.name().to_lowercase();
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
                            if map.len() >= MAX_CARD { return None; }
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
    let ts: Vec<usize> = schema
        .fields()
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match f.data_type() {
            DataType::Timestamp(_, _) | DataType::Date32 | DataType::Date64 => Some(i),
            _ => None,
        })
        .collect();
    if ts.is_empty() {
        return Ok(batches);
    }
    let new_fields: Vec<Field> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| if ts.contains(&i) { Field::new(f.name(), DataType::Utf8, f.is_nullable()) }
                      else               { f.as_ref().clone() })
        .collect();
    let new_schema = Arc::new(Schema::new(new_fields));
    batches
        .into_iter()
        .map(|b| {
            let cols: Vec<ArrayRef> = b
                .columns()
                .iter()
                .enumerate()
                .map(|(i, c)| if ts.contains(&i) {
                    compute::cast(c.as_ref(), &DataType::Utf8).map_err(AppError::from)
                } else {
                    Ok(c.clone())
                })
                .collect::<Result<_, _>>()?;
            RecordBatch::try_new(new_schema.clone(), cols).map_err(AppError::from)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Arrow compute helpers (all SIMD-accelerated)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum CmpOp { Eq, Neq, Gt, Gte, Lt, Lte, Like, ILike }

fn eq_str(col: &ArrayRef, val: &str) -> Result<BooleanArray, AppError> {
    let arr = col.as_any().downcast_ref::<StringArray>()
        .ok_or_else(|| AppError::InvalidValue("equality: column is not a string".into()))?;
    let s = Scalar::new(StringArray::from(vec![val]));
    Ok(eq(arr, &s)?)
}

fn eq_int(col: &ArrayRef, val: i64) -> Result<BooleanArray, AppError> {
    macro_rules! do_eq {
        ($arr_type:ty, $cast:ty) => {{
            let arr = col.as_any().downcast_ref::<$arr_type>().unwrap();
            let s   = Scalar::new(<$arr_type>::from(vec![val as $cast]));
            eq(arr, &s).map_err(AppError::from)
        }};
    }
    match col.data_type() {
        DataType::Int8  => do_eq!(Int8Array,  i8),
        DataType::Int16 => do_eq!(Int16Array, i16),
        DataType::Int32 => do_eq!(Int32Array, i32),
        DataType::Int64 => do_eq!(Int64Array, i64),
        dt => Err(AppError::InvalidValue(format!("expected integer column, got {dt:?}"))),
    }
}

fn and_opt(prev: Option<BooleanArray>, next: BooleanArray) -> Result<BooleanArray, AppError> {
    Ok(match prev { None => next, Some(p) => compute::and(&p, &next)? })
}

/// OR-of-equalities for `in` predicate. Simple and always correct.
fn in_list(col: &ArrayRef, items: &[JsonValue]) -> Result<BooleanArray, AppError> {
    let mut it = items.iter();
    let first_val = it.next().unwrap(); // guarded by caller
    let mut mask = eval_eq(col, first_val)?;
    for item in it {
        mask = compute::or(&mask, &eval_eq(col, item)?)?;
    }
    Ok(mask)
}

fn eval_eq(col: &ArrayRef, val: &JsonValue) -> Result<BooleanArray, AppError> {
    match col.data_type() {
        DataType::Utf8 => {
            let s = val.as_str().ok_or_else(|| AppError::InvalidValue("'in': expected string".into()))?;
            eq_str(col, s)
        }
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            let n = val.as_i64().ok_or_else(|| AppError::InvalidValue("'in': expected integer".into()))?;
            eq_int(col, n)
        }
        dt => Err(AppError::InvalidValue(format!("'in' not supported for {dt:?}"))),
    }
}

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
// Serialisation – direct byte writing, no serde, no per-row allocations
// ---------------------------------------------------------------------------

pub fn serialize(batch: &RecordBatch) -> Result<String, AppError> {
    let schema = batch.schema();
    let n_cols = schema.fields().len();
    let n_rows = batch.num_rows();

    // Pre-compute `"FieldName":` key bytes once per response.
    let keys: Vec<Vec<u8>> = schema
        .fields()
        .iter()
        .map(|f| {
            let mut k = Vec::with_capacity(f.name().len() + 3);
            k.push(b'"');
            k.extend_from_slice(f.name().as_bytes());
            k.extend_from_slice(b"\":");
            k
        })
        .collect();

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
    // Safety: StringArrays are always valid UTF-8; all other types emit ASCII only.
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
