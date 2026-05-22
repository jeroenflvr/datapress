use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array,
    LargeStringArray, RecordBatch, Scalar, StringArray,
};
use arrow::compute;
use arrow::compute::kernels::cmp::{eq, gt, gt_eq, lt, lt_eq, neq};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value as JsonValue;

use crate::errors::AppError;
use crate::models::{Predicate, QueryRequest};

// ---------------------------------------------------------------------------
// Store – all rows in one RecordBatch; filters via Arrow SIMD compute kernels
// ---------------------------------------------------------------------------

pub struct Store {
    data:    RecordBatch,
    /// Lowercase column name → column index.
    col_idx: HashMap<String, usize>,
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

        log::info!("loaded {} rows, {} cols", data.num_rows(), schema.fields().len());
        Ok(Self { data, col_idx })
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

        let mut mask: Option<BooleanArray> = None;
        if let Some(v) = state {
            let m = eq_str(self.data.column(self.col_req("state")?), v)?;
            mask = Some(and_opt(mask, m)?);
        }
        if let Some(v) = severity {
            let m = eq_int(self.data.column(self.col_req("severity")?), v)?;
            mask = Some(and_opt(mask, m)?);
        }
        if let Some(v) = city {
            let m = eq_str(self.data.column(self.col_req("city")?), v)?;
            mask = Some(and_opt(mask, m)?);
        }

        let filtered = compute::filter_record_batch(&self.data, &mask.unwrap())?;
        let start = offset.min(filtered.num_rows());
        let len   = limit.min(filtered.num_rows() - start);
        serialize(&filtered.slice(start, len))
    }

    // -----------------------------------------------------------------------
    // POST /api/accidents/query – arbitrary predicates + column projection
    // -----------------------------------------------------------------------

    pub fn query(&self, req: &QueryRequest) -> Result<String, AppError> {
        let page      = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset    = ((page - 1) * page_size) as usize;
        let limit     = page_size as usize;

        let batch = if req.predicates.is_empty() {
            let start = offset.min(self.data.num_rows());
            let len   = limit.min(self.data.num_rows() - start);
            self.data.slice(start, len)
        } else {
            let mut mask: Option<BooleanArray> = None;
            for pred in &req.predicates {
                mask = Some(and_opt(mask, self.eval_pred(pred)?)?);
            }
            let filtered = compute::filter_record_batch(&self.data, &mask.unwrap())?;
            let start = offset.min(filtered.num_rows());
            let len   = limit.min(filtered.num_rows() - start);
            filtered.slice(start, len)
        };

        serialize(&self.project(batch, &req.columns)?)
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
