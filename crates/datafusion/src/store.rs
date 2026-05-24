use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use arrow::array::{
    Array, ArrayRef, BooleanArray, Decimal128Array, Decimal256Array,
    Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array,
    LargeStringArray, RecordBatch, Scalar, StringArray, StringViewArray,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::compute;
use arrow::compute::kernels::cmp::{eq, gt, gt_eq, lt, lt_eq, neq};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
use parquet::arrow::ProjectionMask;
use serde_json::Value as JsonValue;

use datafusion::datasource::{MemTable, TableProvider};
use datafusion::datasource::file_format::parquet::ParquetFormat;
use datafusion::datasource::listing::{
    ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
};
use datafusion::prelude::{ParquetReadOptions, SessionContext};

use object_store::aws::AmazonS3Builder;
use url::Url;

use datapress_core::config::{
    AddressingStyle, AppConfig, DatasetConfig, IndexConfig, IndexMode,
    ResolvedCreds, S3Config, SourceKind,
};
use datapress_core::errors::AppError;
use datapress_core::models::{CountRequest, Predicate, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Pre-built equality index: lowercase col name → string-encoded value → sorted row ids.
type EqIndex = HashMap<String, HashMap<String, Vec<u32>>>;

/// Per-dataset state: schema metadata, the resident chunks, and the
/// equality index built per the dataset's `[dataset.index]` policy.
///
/// `data` is the dataset as a `Vec<RecordBatch>` — exactly the chunks
/// produced by the underlying reader, after temporal columns are cast to
/// `Utf8`. We deliberately do **not** call `concat_batches` to fuse them
/// into one batch: on wide schemas (hundreds of columns) that transiently
/// allocates a second full copy of the decoded Arrow data, pushing peak
/// RSS to ~2× the resident size and OOM-killing the process at startup.
///
/// When `lazy` is true the dataset is *not* materialised: `data` is empty,
/// `index` is empty, and every query is dispatched to DataFusion SQL
/// against a registered `ListingTable`. `arrow_schema` still carries the
/// inferred schema so discovery endpoints work.
pub struct DatasetState {
    pub schema:       DatasetSchema,
    pub data:         Vec<RecordBatch>,
    pub arrow_schema: Arc<Schema>,
    pub index:        EqIndex,
    pub lazy:         bool,
}

impl DatasetState {
    /// Sum of `num_rows()` across all resident chunks. `0` for lazy datasets.
    pub fn num_rows(&self) -> usize {
        self.data.iter().map(|b| b.num_rows()).sum()
    }
}

/// Multi-dataset registry. Each dataset is registered in the shared
/// `SessionContext` under its configured name. The per-dataset state is
/// held behind `ArcSwap` so a reload can atomically replace it without
/// blocking concurrent queries.
pub struct Store {
    ctx:      SessionContext,
    /// Original dataset configs, indexed by name. Reload reads the source
    /// path from here — clients can't redirect a reload at an arbitrary file.
    configs:  HashMap<String, DatasetConfig>,
    /// Hot-swappable snapshot of all currently loaded datasets.
    datasets: ArcSwap<HashMap<String, Arc<DatasetState>>>,
    /// Per-name reload mutex. Serialises concurrent reloads of the same
    /// dataset; reloads of different datasets proceed in parallel.
    reload_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Outcome of a successful `reload`.
pub struct ReloadStats {
    pub rows:       usize,
    pub elapsed_ms: u128,
}

impl Store {
    /// Load every dataset declared in `cfg`.
    pub async fn load(cfg: &AppConfig) -> Result<Self, AppError> {
        // One-shot init for the deltalake S3 backend. Safe to call more
        // than once — the handlers are idempotent.
        if cfg.datasets.iter().any(|d| d.source.kind == SourceKind::Delta && d.source.is_s3()) {
            deltalake::aws::register_handlers(None);
        }

        let ctx = SessionContext::new();
        let mut datasets = HashMap::with_capacity(cfg.datasets.len());
        let mut configs  = HashMap::with_capacity(cfg.datasets.len());

        for d in &cfg.datasets {
            let (state, provider) = build_dataset(d, &ctx).await?;
            ctx.register_table(d.name.as_str(), provider)?;
            datasets.insert(d.name.clone(), Arc::new(state));
            configs.insert(d.name.clone(), d.clone());
        }
        Ok(Self {
            ctx,
            configs,
            datasets: ArcSwap::from_pointee(datasets),
            reload_locks: Mutex::new(HashMap::new()),
        })
    }

    /// Sorted list of dataset names.
    pub fn names(&self) -> Vec<String> {
        let snap = self.datasets.load();
        let mut v: Vec<String> = snap.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn dataset(&self, name: &str) -> Result<Arc<DatasetState>, AppError> {
        self.datasets
            .load()
            .get(name)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("dataset: {name}")))
    }

    /// JSON for the first row of the dataset, or `null` if empty. Used by
    /// `GET /api/datasets/{name}/schema` for discoverability.
    pub async fn sample(&self, name: &str) -> Result<String, AppError> {
        let st = self.dataset(name)?;

        // Lazy datasets have no resident batch — pull one row via SQL.
        if st.lazy {
            let table = DatasetSchema::quote_ident(&st.schema.name);
            let sql = format!("SELECT * FROM {table} LIMIT 1");
            let df = self.ctx.sql(&sql).await?;
            let batches = df.collect().await?;
            if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
                return Ok("null".into());
            }
            let arr = serialize(&batches[0].slice(0, 1))?;
            let trimmed = arr.trim();
            let inner = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                .unwrap_or(trimmed);
            return Ok(inner.to_string());
        }

        let first = match st.data.iter().find(|b| b.num_rows() > 0) {
            Some(b) => b,
            None    => return Ok("null".into()),
        };
        let arr = serialize(&first.slice(0, 1))?;
        // strip the outer [] to return a single object
        let trimmed = arr.trim();
        let inner = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            .unwrap_or(trimmed);
        Ok(inner.to_string())
    }

    /// Rebuild `name` from disk and atomically swap it in. Concurrent queries
    /// against the same name continue to see the *old* `Arc<DatasetState>`
    /// until they finish; the old data is dropped once the last reference
    /// goes away.
    pub async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        // 1. Look up the dataset config. Not finding it = 404.
        let cfg = self
            .configs
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("dataset: {name}")))?
            .clone();

        // 2. Per-name lock: only one reload of this dataset at a time.
        let lock = {
            let mut locks = self.reload_locks.lock().unwrap();
            locks
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        let started = std::time::Instant::now();

        // 3. Heavy lifting (source read + index build). Parquet/delta
        // readers are themselves async, so we don't wrap in `web::block`.
        let (state, provider) = build_dataset(&cfg, &self.ctx).await?;
        let rows = state.num_rows();

        // 4. Atomic swap.
        //   a) Replace the MemTable inside the SessionContext.
        //   b) ArcSwap a new snapshot map with the updated Arc<DatasetState>.
        // In-flight queries already hold the old provider + old Arc; they
        // run to completion. New queries see the new data.
        let _ = self.ctx.deregister_table(name)?;
        self.ctx.register_table(name, provider)?;

        let mut new_map = (**self.datasets.load()).clone();
        new_map.insert(name.to_string(), Arc::new(state));
        self.datasets.store(Arc::new(new_map));

        let elapsed_ms = started.elapsed().as_millis();
        log::info!("reloaded dataset '{name}': {rows} rows in {elapsed_ms} ms");
        Ok(ReloadStats { rows, elapsed_ms })
    }

    /// Run a `QueryRequest` against `name`. Empty predicates → O(1) Arrow
    /// slice. Otherwise → DataFusion SQL on the single registered table.
    /// Lazy datasets skip the in-memory hot paths and always dispatch to SQL.
    pub async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        let st = self.dataset(name)?;

        let page      = req.page.max(1);
        let page_size = req.page_size.clamp(1, 1000);
        let offset    = ((page - 1) * page_size) as usize;
        let limit     = page_size as usize;

        // In-memory hot paths only fire when the dataset is materialised.
        if !st.lazy {
            let total = st.num_rows();

            // No predicates → O(1) raw Arrow slice across chunks, no engine overhead.
            if req.predicates.is_empty() {
                let start = offset.min(total);
                let len   = limit.min(total - start);
                let page  = slice_global(&st.data, &st.arrow_schema, start, len)?;
                return serialize(&project(&st.schema, page, &req.columns)?);
            }

            // Index fast path: if every predicate is eq/in on an indexed column,
            // resolve via the pre-built equality index — far cheaper than a full
            // SIMD scan over the entire dataset.
            if let Some(rows) = try_index(&st.index, &req.predicates) {
                let batch = take_page(&st.data, &st.arrow_schema, &rows, offset, limit)?;
                return serialize(&project(&st.schema, batch, &req.columns)?);
            }
        }

        // Fallback (and only path for lazy datasets): DataFusion SQL —
        // multi-threaded vectorised execution covers all operators
        // (LIKE, ILIKE, ranges, NOT NULL, …).
        let sql     = build_query_sql(&st.schema, req)?;
        let df      = self.ctx.sql(&sql).await?;
        let batches = df.collect().await?;
        if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
            return Ok("[]".to_string());
        }
        let batch = compute::concat_batches(&batches[0].schema(), batches.iter())?;
        serialize(&batch)
    }

    /// Return the number of rows matching `req.predicates`. With no
    /// predicates this is a cheap metadata lookup on materialised datasets
    /// and a `SELECT COUNT(*)` on lazy ones.
    pub async fn count(&self, name: &str, req: &CountRequest) -> Result<i64, AppError> {
        let st = self.dataset(name)?;

        if !st.lazy {
            // No predicates → resident row count, no scan.
            if req.predicates.is_empty() {
                return Ok(st.num_rows() as i64);
            }
            // Index fast path: same eligibility rules as `query`.
            if let Some(rows) = try_index(&st.index, &req.predicates) {
                return Ok(rows.len() as i64);
            }
        }

        // Fallback: DataFusion SQL — same predicate translation as `query`.
        let sql     = build_count_sql(&st.schema, &req.predicates)?;
        let df      = self.ctx.sql(&sql).await?;
        let batches = df.collect().await?;
        let n = batches.first()
            .and_then(|b| b.column(0).as_any().downcast_ref::<arrow::array::Int64Array>())
            .filter(|a| a.len() > 0)
            .map(|a| a.value(0))
            .unwrap_or(0);
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Dataset loading
// ---------------------------------------------------------------------------

async fn build_dataset(
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    // Lazy datasets: register a ListingTable straight against the source
    // files and skip the materialise / index / partition pipeline below.
    // Only local parquet is supported for now — lazy on remote or delta
    // sources is rejected so the user gets a clear config-time error rather
    // than silently falling through to the eager path.
    if d.lazy {
        match (d.source.kind, d.source.is_s3()) {
            (SourceKind::Parquet, false) => return build_lazy_local_parquet(d, ctx).await,
            _ => return Err(AppError::Internal(format!(
                "dataset '{}': lazy mode is only supported for local parquet (kind=parquet, no s3)",
                d.name
            ))),
        }
    }

    // Fetch raw RecordBatches from whichever backing store the dataset
    // is configured to use. All four (parquet, delta) x (local, s3)
    // combinations converge into one Vec<RecordBatch>; the materialisation
    // / indexing / partitioning logic below is shared.
    let raw_batches: Vec<RecordBatch> = match (d.source.kind, d.source.is_s3()) {
        (SourceKind::Parquet, false) => read_local_parquet(d)?,
        (SourceKind::Parquet, true)  => read_s3_parquet(d, ctx).await?,
        (SourceKind::Delta,   false) => read_delta(d, HashMap::new()).await?,
        (SourceKind::Delta,   true)  => read_delta(d, delta_s3_options(d)?).await?,
    };
    if raw_batches.is_empty() {
        return Err(AppError::Internal(format!(
            "dataset '{}': source produced no batches", d.name
        )));
    }

    let chunks    = raw_batches;
    let arrow_sch = chunks[0].schema();

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

    // Build the equality index per the per-dataset policy. Operates on the
    // chunked representation directly so we never have to materialise a
    // single concatenated batch (which would double peak RSS on wide
    // schemas — see `DatasetState` docs).
    let index = build_eq_index_with_policy(&chunks, &d.index);

    // Partition for parallel scans by the SQL fallback path. We distribute
    // the existing batches round-robin across `n_parts` partitions instead
    // of re-slicing a concatenated batch — `clone()` on a RecordBatch is
    // an Arc-clone of the column buffers, not a copy.
    let n_parts = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut parts: Vec<Vec<RecordBatch>> = (0..n_parts).map(|_| Vec::new()).collect();
    for (i, b) in chunks.iter().enumerate() {
        if b.num_rows() == 0 { continue; }
        parts[i % n_parts].push(b.clone());
    }
    parts.retain(|p| !p.is_empty());
    let provider: Arc<dyn TableProvider> = Arc::new(MemTable::try_new(arrow_sch.clone(), parts)?);

    let total_rows: usize = chunks.iter().map(|b| b.num_rows()).sum();
    let mem_mb: usize = chunks.iter()
        .flat_map(|b| b.columns().iter())
        .map(|c| c.get_buffer_memory_size())
        .sum::<usize>() / 1_048_576;
    log::info!(
        "dataset '{}' [{}]: {} rows, {} cols, {} MB, {} chunks, {} indexed cols",
        d.name, d.source.kind.as_str(),
        total_rows, schema.columns.len(), mem_mb, chunks.len(), index.len()
    );

    Ok((
        DatasetState {
            schema,
            data: chunks,
            arrow_schema: arrow_sch,
            index,
            lazy: false,
        },
        provider,
    ))
}

/// Build a lazy state + `ListingTable` provider for a local parquet dataset.
/// The dataset is never read into RAM; DataFusion streams row groups on
/// each query. The returned `DatasetState.data` is an empty `Vec` —
/// `arrow_schema` still carries the inferred Arrow schema for discovery.
async fn build_lazy_local_parquet(
    d:   &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    let files = d.resolve_local_parquet_files()?;

    let urls: Vec<ListingTableUrl> = files.iter()
        .map(|p| {
            let s = p.to_str().ok_or_else(|| AppError::Internal(format!(
                "dataset '{}': non-utf8 path {}", d.name, p.display()
            )))?;
            ListingTableUrl::parse(s).map_err(|e| AppError::Internal(format!(
                "dataset '{}': bad url '{s}': {e}", d.name
            )))
        })
        .collect::<Result<_, _>>()?;

    let opts = ListingOptions::new(Arc::new(ParquetFormat::default()))
        .with_file_extension(".parquet");

    let session_state = ctx.state();
    let resolved_schema = opts
        .infer_schema(&session_state, &urls[0])
        .await
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': infer parquet schema: {e}", d.name
        )))?;

    let cfg = ListingTableConfig::new_with_multi_paths(urls)
        .with_listing_options(opts)
        .with_schema(resolved_schema.clone());
    let table = ListingTable::try_new(cfg)
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': ListingTable::try_new: {e}", d.name
        )))?;
    let provider: Arc<dyn TableProvider> = Arc::new(table);

    let arrow_sch = resolved_schema;
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

    log::info!(
        "dataset '{}' [{}, lazy]: {} files, {} cols (no materialise, no index)",
        d.name, d.source.kind.as_str(), files.len(), schema.columns.len()
    );

    Ok((
        DatasetState {
            schema,
            data: Vec::new(),
            arrow_schema: arrow_sch,
            index: EqIndex::new(),
            lazy: true,
        },
        provider,
    ))
}

/// Original local-parquet code path — sync file I/O. We set a large reader
/// batch size so wide schemas (hundreds of columns) don't pay per-array
/// metadata overhead on thousands of small (default 1024-row) batches.
///
/// Two memory-saving knobs are applied here:
///
/// * **Column projection** — if `d.columns` is non-empty, only those
///   columns are decoded; everything else is skipped at the parquet reader
///   level (no Arrow array is ever allocated for the dropped columns).
/// * **Dictionary preservation** — Utf8 columns whose parquet column chunks
///   carry a dictionary page are materialised as Arrow
///   `Dictionary(Int32, Utf8)` instead of plain `Utf8`. Low-cardinality
///   string columns (state, country, severity, …) stay represented as
///   `n_unique` string slots plus an Int32 index per row instead of
///   `n_rows` independent strings — typically 10×–50× smaller for
///   real-world data.
fn read_local_parquet(d: &DatasetConfig) -> Result<Vec<RecordBatch>, AppError> {
    let files = d.resolve_local_parquet_files()?;
    let mut all = Vec::new();
    let wanted: Option<std::collections::HashSet<String>> = if d.columns.is_empty() {
        None
    } else {
        Some(d.columns.iter().map(|c| c.to_lowercase()).collect())
    };

    for f in &files {
        let file = std::fs::File::open(f)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", f.display())))?;

        // First pass: peek the parquet metadata + default Arrow schema so we
        // can (a) decide a column projection and (b) override Utf8 columns
        // that are dictionary-encoded in the file so the reader materialises
        // them as Arrow Dictionary arrays instead of expanding to plain Utf8.
        let probe = ParquetRecordBatchReaderBuilder::try_new(file.try_clone()
            .map_err(|e| AppError::Internal(format!("dup fd {}: {e}", f.display())))?)?;
        let parquet_schema = probe.parquet_schema().clone();
        let arrow_schema   = probe.schema().clone();
        let metadata       = probe.metadata().clone();
        drop(probe);

        // Column projection (top-level / leaf indices for flat schemas).
        let projection = if let Some(w) = &wanted {
            let indices: Vec<usize> = arrow_schema.fields().iter().enumerate()
                .filter(|(_, fld)| w.contains(&fld.name().to_lowercase()))
                .map(|(i, _)| i)
                .collect();
            if indices.is_empty() {
                return Err(AppError::Internal(format!(
                    "dataset '{}': no columns from `columns = {:?}` match parquet schema for {}",
                    d.name, d.columns, f.display()
                )));
            }
            ProjectionMask::roots(&parquet_schema, indices)
        } else {
            ProjectionMask::all()
        };

        // Dictionary override: any Utf8 column whose first row group carries
        // a dictionary page is re-typed to Dictionary(Int32, Utf8). The
        // override schema must still describe every column in the parquet
        // file (projection is applied separately). Skipped entirely when
        // the dataset has `dict_encode = false` — escape hatch for cases
        // where the override interacts badly with null propagation in the
        // downstream engine.
        let mut new_fields: Vec<Field> = arrow_schema.fields().iter()
            .map(|f| f.as_ref().clone()).collect();
        if d.dict_encode {
            if let Some(rg0) = metadata.row_groups().first() {
                for (i, fld) in arrow_schema.fields().iter().enumerate() {
                    if !matches!(fld.data_type(),
                        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View) {
                        continue;
                    }
                    if let Some(col) = rg0.columns().get(i) {
                        if col.dictionary_page_offset().is_some() {
                            new_fields[i] = Field::new(
                                fld.name(),
                                DataType::Dictionary(
                                    Box::new(DataType::Int32),
                                    Box::new(DataType::Utf8),
                                ),
                                fld.is_nullable(),
                            );
                        }
                    }
                }
            }
        }
        let forced_schema = Arc::new(Schema::new(new_fields));

        let opts = ArrowReaderOptions::new().with_schema(forced_schema);
        let reader = ParquetRecordBatchReaderBuilder::try_new_with_options(file, opts)?
            .with_batch_size(65_536)
            .with_projection(projection)
            .build()?;
        for batch in reader {
            all.push(batch.map_err(|e| AppError::Internal(e.to_string()))?);
        }
    }
    if all.is_empty() {
        return Err(AppError::Internal(format!(
            "dataset '{}': parquet source is empty", d.name
        )));
    }
    Ok(all)
}

/// Register an `AmazonS3` object store on the SessionContext (so DataFusion's
/// `read_parquet("s3://…")` can resolve the URL) and stream the whole
/// dataset back through `DataFrame::collect`.
async fn read_s3_parquet(
    d:   &DatasetConfig,
    ctx: &SessionContext,
) -> Result<Vec<RecordBatch>, AppError> {
    register_s3_object_store(d, ctx)?;
    let df = ctx
        .read_parquet(d.source.location.clone(), ParquetReadOptions::default())
        .await?;
    Ok(df.collect().await?)
}

/// Open a Delta table (local or S3) and stream every row back as a Vec of
/// `RecordBatch`. We materialise eagerly so the rest of the backend can
/// treat all datasets uniformly (single in-memory batch + eq-index).
async fn read_delta(
    d:    &DatasetConfig,
    opts: HashMap<String, String>,
) -> Result<Vec<RecordBatch>, AppError> {
    let url = deltalake::ensure_table_uri(&d.source.location)
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': bad delta location '{}': {e}",
            d.name, d.source.location
        )))?;
    let table = deltalake::open_table_with_storage_options(url, opts)
        .await
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': delta open '{}': {e}",
            d.name, d.source.location
        )))?;
    let provider = table.table_provider().await
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': delta table_provider: {e}", d.name
        )))?;
    // Drive a full scan via a throwaway SessionContext so we end up with
    // an in-memory Vec<RecordBatch> the shared materialise path can use.
    let scan_ctx = SessionContext::new();
    let df = scan_ctx.read_table(provider)
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': delta read_table: {e}", d.name
        )))?;
    Ok(df.collect().await?)
}

/// Build the storage-options HashMap that `deltalake::open_table_with_storage_options`
/// expects for S3 access. Keys mirror the AWS env-var names; deltalake
/// passes them through to object_store internally.
fn delta_s3_options(d: &DatasetConfig) -> Result<HashMap<String, String>, AppError> {
    let creds = d.resolved_creds();
    let region = d.resolved_region();
    let s3 = d.s3.clone().unwrap_or_default();

    let mut opts = HashMap::new();
    opts.insert("AWS_REGION".into(), region);
    if let Some(ep) = s3.endpoint.as_deref().filter(|s| !s.is_empty()) {
        opts.insert("AWS_ENDPOINT_URL".into(), ep.to_string());
    }
    if s3.allow_http {
        opts.insert("AWS_ALLOW_HTTP".into(), "true".into());
    }
    opts.insert(
        "AWS_VIRTUAL_HOSTED_STYLE_REQUEST".into(),
        (s3.addressing_style == AddressingStyle::Virtual).to_string(),
    );
    if let Some(k) = creds.access_key_id { opts.insert("AWS_ACCESS_KEY_ID".into(), k); }
    if let Some(s) = creds.secret_access_key { opts.insert("AWS_SECRET_ACCESS_KEY".into(), s); }
    if let Some(t) = creds.session_token { opts.insert("AWS_SESSION_TOKEN".into(), t); }
    // Read-only paths don't need the S3 lock-provider plumbing.
    opts.insert("AWS_S3_ALLOW_UNSAFE_RENAME".into(), "true".into());
    Ok(opts)
}

/// Construct an `AmazonS3` object_store from the dataset's `[dataset.s3]`
/// block + resolved credentials and register it on `ctx` under
/// `s3://bucket/`.
fn register_s3_object_store(d: &DatasetConfig, ctx: &SessionContext) -> Result<(), AppError> {
    let (bucket, _key) = d.source.s3_bucket()?;
    let creds  = d.resolved_creds();
    let region = d.resolved_region();
    let s3     = d.s3.clone().unwrap_or_default();

    let store = build_s3(bucket, &region, &s3, &creds)
        .map_err(|e| AppError::Internal(format!(
            "dataset '{}': build S3 store for '{bucket}': {e}", d.name
        )))?;

    let url = Url::parse(&format!("s3://{bucket}"))
        .map_err(|e| AppError::Internal(format!("invalid s3 URL for bucket {bucket}: {e}")))?;
    ctx.register_object_store(&url, Arc::new(store));
    Ok(())
}

fn build_s3(
    bucket: &str,
    region: &str,
    s3:     &S3Config,
    creds:  &ResolvedCreds,
) -> Result<object_store::aws::AmazonS3, object_store::Error> {
    let mut b = AmazonS3Builder::new()
        .with_bucket_name(bucket)
        .with_region(region)
        .with_allow_http(s3.allow_http)
        .with_virtual_hosted_style_request(s3.addressing_style == AddressingStyle::Virtual);
    if let Some(ep) = s3.endpoint.as_deref().filter(|s| !s.is_empty()) {
        b = b.with_endpoint(ep);
    }
    if let Some(k) = creds.access_key_id.as_deref() {
        b = b.with_access_key_id(k);
    }
    if let Some(s) = creds.secret_access_key.as_deref() {
        b = b.with_secret_access_key(s);
    }
    if let Some(t) = creds.session_token.as_deref() {
        b = b.with_token(t);
    }
    b.build()
}

fn arrow_to_logical(dt: &DataType) -> LogicalType {
    match dt {
        DataType::Boolean => LogicalType::Bool,
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64
            | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
                => LogicalType::Int,
        DataType::Float16 | DataType::Float32 | DataType::Float64
                => LogicalType::Float,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
                => LogicalType::Utf8,
        // Dictionary-encoded strings are reported as plain strings — clients
        // (and the rest of the backend) shouldn't have to care that we keep
        // a compressed representation in memory.
        DataType::Dictionary(_, v) if matches!(v.as_ref(),
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View) => LogicalType::Utf8,
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
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok(format!(
        "SELECT {cols} FROM {table}{where_clause} LIMIT {page_size} OFFSET {offset}"
    ))
}

fn build_count_sql(schema: &DatasetSchema, predicates: &[Predicate]) -> Result<String, AppError> {
    let clauses: Vec<String> = predicates.iter()
        .map(|p| pred_to_sql(schema, p))
        .collect::<Result<_, _>>()?;
    let table = DatasetSchema::quote_ident(&schema.name);
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok(format!("SELECT COUNT(*) FROM {table}{where_clause}"))
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

/// Return rows `[offset, offset+limit)` from a chunked dataset by slicing
/// the underlying batches (zero-copy) and concatenating the (small) page.
fn slice_global(
    chunks: &[RecordBatch],
    schema: &Arc<Schema>,
    offset: usize,
    limit:  usize,
) -> Result<RecordBatch, AppError> {
    if limit == 0 || chunks.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }
    let mut out = Vec::new();
    let mut to_skip   = offset;
    let mut remaining = limit;
    for b in chunks {
        if remaining == 0 { break; }
        let n = b.num_rows();
        if to_skip >= n { to_skip -= n; continue; }
        let take = remaining.min(n - to_skip);
        out.push(b.slice(to_skip, take));
        to_skip = 0;
        remaining -= take;
    }
    if out.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }
    compute::concat_batches(schema, out.iter()).map_err(AppError::from)
}

/// Materialise the page `rows[offset..offset+limit]` from a chunked dataset.
/// Row ids are global (across the concatenation of all chunks). We map each
/// requested row to its (chunk, local-index), `take` per chunk, then stitch
/// the per-chunk results back together preserving the original row order.
fn take_page(
    chunks: &[RecordBatch],
    schema: &Arc<Schema>,
    rows:   &[u32],
    offset: usize,
    limit:  usize,
) -> Result<RecordBatch, AppError> {
    let start = offset.min(rows.len());
    let len   = limit.min(rows.len() - start);
    if len == 0 || chunks.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }

    // Prefix-sum table: `offsets[i]` is the first global row id of chunk `i`,
    // and `offsets.last()` is the total row count.
    let mut offsets: Vec<u32> = Vec::with_capacity(chunks.len() + 1);
    let mut acc: u32 = 0;
    offsets.push(0);
    for b in chunks {
        acc = acc.checked_add(b.num_rows() as u32).expect("row count exceeds u32::MAX");
        offsets.push(acc);
    }

    // Bucket each global row id into the chunk that contains it, remembering
    // the original output position so we can restore page order at the end.
    let mut buckets: Vec<Vec<(u32, u32)>> = (0..chunks.len()).map(|_| Vec::new()).collect();
    for (out_pos, &gid) in rows[start..start + len].iter().enumerate() {
        let bi    = offsets.partition_point(|&x| x <= gid).saturating_sub(1);
        let local = gid - offsets[bi];
        buckets[bi].push((out_pos as u32, local));
    }

    // Per-chunk take, recording the destination index for each emitted row.
    let mut takens: Vec<RecordBatch> = Vec::new();
    let mut dest:   Vec<u32>         = Vec::with_capacity(len);
    for (bi, bucket) in buckets.iter().enumerate() {
        if bucket.is_empty() { continue; }
        let idx = UInt32Array::from(bucket.iter().map(|(_, l)| *l).collect::<Vec<u32>>());
        let cols: Vec<ArrayRef> = chunks[bi].columns().iter()
            .map(|c| arrow::compute::take(c.as_ref(), &idx, None::<arrow::compute::TakeOptions>)
                     .map_err(AppError::from))
            .collect::<Result<_, _>>()?;
        takens.push(RecordBatch::try_new(chunks[bi].schema(), cols)?);
        dest.extend(bucket.iter().map(|(out_pos, _)| *out_pos));
    }

    // Stitch per-chunk results then permute to restore the requested order.
    let stitched = compute::concat_batches(schema, takens.iter())?;
    let mut inv = vec![0u32; len];
    for (i, &d) in dest.iter().enumerate() {
        inv[d as usize] = i as u32;
    }
    let perm = UInt32Array::from(inv);
    let cols: Vec<ArrayRef> = stitched.columns().iter()
        .map(|c| arrow::compute::take(c.as_ref(), &perm, None::<arrow::compute::TakeOptions>)
                 .map_err(AppError::from))
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(stitched.schema(), cols).map_err(AppError::from)
}

/// Build the equality index per the dataset's policy, against the chunked
/// representation. Row ids are global across the concatenation of all
/// chunks (so they remain compatible with `take_page` / `slice_global`).
fn build_eq_index_with_policy(chunks: &[RecordBatch], cfg: &IndexConfig) -> EqIndex {
    use rayon::prelude::*;

    if cfg.mode == IndexMode::None || chunks.is_empty() {
        return EqIndex::new();
    }

    let allow: Option<HashMap<String, ()>> = if cfg.mode == IndexMode::List {
        Some(cfg.columns.iter().map(|c| (c.to_lowercase(), ())).collect())
    } else {
        None
    };

    let max_card = if cfg.mode == IndexMode::Auto { Some(cfg.max_cardinality) } else { None };

    // Per-chunk starting global row id.
    let mut batch_offsets: Vec<u32> = Vec::with_capacity(chunks.len());
    let mut acc: u32 = 0;
    for b in chunks {
        batch_offsets.push(acc);
        acc = acc.checked_add(b.num_rows() as u32).expect("row count exceeds u32::MAX");
    }

    let schema = chunks[0].schema();

    schema.fields().par_iter().enumerate()
        .filter_map(|(ci, field)| {
            let col_lower = field.name().to_lowercase();
            if let Some(a) = &allow {
                if !a.contains_key(&col_lower) { return None; }
            }

            // Only build for index-friendly types; skip everything else
            // up-front so we don't pay the per-chunk dispatch cost.
            let dtype = field.data_type();
            let dict_utf8 = matches!(dtype,
                DataType::Dictionary(k, v)
                    if matches!(k.as_ref(), DataType::Int32)
                    && matches!(v.as_ref(), DataType::Utf8));
            match dtype {
                DataType::Utf8 | DataType::Utf8View
                | DataType::Boolean
                | DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {}
                _ if dict_utf8 => {}
                _ => return None,
            }

            let mut map: HashMap<String, Vec<u32>> = HashMap::new();

            for (bi, batch) in chunks.iter().enumerate() {
                let base = batch_offsets[bi];
                let col  = batch.column(ci);

                macro_rules! index_col {
                    ($arr_ty:ty) => {{
                        let arr = col.as_any().downcast_ref::<$arr_ty>()?;
                        for row in 0..arr.len() {
                            if arr.is_null(row) { continue; }
                            let key = arr.value(row).to_string();
                            let gid = base + row as u32;
                            if let Some(v) = map.get_mut(&key) {
                                v.push(gid);
                            } else {
                                if let Some(mc) = max_card {
                                    if map.len() >= mc { return None; }
                                }
                                map.insert(key, vec![gid]);
                            }
                        }
                    }};
                }

                if dict_utf8 {
                    // Dictionary(Int32, Utf8): iterate keys + look up the
                    // string value from the (small) dictionary. We allocate
                    // the key string only when the value is new — repeated
                    // values reuse the existing HashMap entry by hash, but
                    // `HashMap::get_mut` still needs the key, so we use a
                    // borrowed lookup via `get` first to avoid the alloc.
                    let arr = col.as_any()
                        .downcast_ref::<arrow::array::DictionaryArray<arrow::datatypes::Int32Type>>()?;
                    let keys   = arr.keys();
                    let values = arr.values().as_any().downcast_ref::<StringArray>()?;
                    for row in 0..arr.len() {
                        if arr.is_null(row) { continue; }
                        let k = keys.value(row) as usize;
                        let s = values.value(k);
                        let gid = base + row as u32;
                        if let Some(v) = map.get_mut(s) {
                            v.push(gid);
                        } else {
                            if let Some(mc) = max_card {
                                if map.len() >= mc { return None; }
                            }
                            map.insert(s.to_string(), vec![gid]);
                        }
                    }
                } else {
                    match dtype {
                        DataType::Utf8     => index_col!(StringArray),
                        DataType::Utf8View => index_col!(StringViewArray),
                        DataType::Boolean  => index_col!(BooleanArray),
                        DataType::Int8     => index_col!(Int8Array),
                        DataType::Int16    => index_col!(Int16Array),
                        DataType::Int32    => index_col!(Int32Array),
                        DataType::Int64    => index_col!(Int64Array),
                        _ => unreachable!(),
                    }
                }
            }

            Some((col_lower, map))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Serialise-time temporal cast: convert Timestamp/Date/Time columns to Utf8
// on the page batch right before JSON encoding. We deliberately do **not**
// pay this cost at load time — a `Date32` is 4 bytes per row, its ISO-8601
// rendering is ~10–24 bytes per row, and a wide dataset full of temporal
// columns would balloon resident RAM. The cast is applied per page (≤ 1000
// rows after pagination), so the per-query overhead is negligible.
// ---------------------------------------------------------------------------

/// Returns true for Arrow types that `write_value` can render directly. Any
/// type returning false is pre-cast to Utf8 in [`cast_for_serialize`] so the
/// JSON output is faithful rather than silently `null`.
fn writable_inline(dt: &DataType) -> bool {
    match dt {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
        | DataType::Boolean
        | DataType::Int8  | DataType::Int16  | DataType::Int32  | DataType::Int64
        | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
        | DataType::Float32 | DataType::Float64
        | DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => true,
        DataType::Dictionary(k, v)
            if matches!(k.as_ref(), DataType::Int32)
                && matches!(v.as_ref(), DataType::Utf8) => true,
        _ => false,
    }
}

/// Cast any column whose dtype isn't directly writable by `write_value` to
/// `Utf8`, on the bounded page batch. Covers temporals (Timestamp/Date/Time)
/// — kept native in resident memory to save RAM — and also any exotic dtype
/// (Float16, Binary, List, Struct, Decimal-with-unsupported-precision, …)
/// so the JSON serializer never falls back to writing literal `null`.
fn cast_for_serialize(batch: &RecordBatch) -> Result<RecordBatch, AppError> {
    let schema = batch.schema();
    let to_cast: Vec<usize> = schema.fields().iter().enumerate()
        .filter_map(|(i, f)| if writable_inline(f.data_type()) { None } else { Some(i) })
        .collect();
    if to_cast.is_empty() {
        return Ok(batch.clone());
    }
    let new_fields: Vec<Field> = schema.fields().iter().enumerate()
        .map(|(i, f)| if to_cast.contains(&i) {
            Field::new(f.name(), DataType::Utf8, f.is_nullable())
        } else { f.as_ref().clone() })
        .collect();
    let new_schema = Arc::new(Schema::new(new_fields));
    let cols: Vec<ArrayRef> = batch.columns().iter().enumerate()
        .map(|(i, c)| if to_cast.contains(&i) {
            compute::cast(c.as_ref(), &DataType::Utf8).map_err(AppError::from)
        } else { Ok(c.clone()) })
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(new_schema, cols).map_err(AppError::from)
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
    // Temporal columns are kept native in resident memory (compact). Cast
    // them — plus any other dtype `write_value` can't render directly — to
    // Utf8 here, on the bounded page batch, so the JSON output is faithful
    // without paying the load-time RAM cost.
    let batch  = cast_for_serialize(batch)?;
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
        DataType::Utf8View =>
            write_str(buf, col.as_any().downcast_ref::<StringViewArray>().unwrap().value(row)),
        DataType::Dictionary(key, value)
            if matches!(key.as_ref(), DataType::Int32)
                && matches!(value.as_ref(), DataType::Utf8) =>
        {
            let dict = col.as_any()
                .downcast_ref::<arrow::array::DictionaryArray<arrow::datatypes::Int32Type>>()
                .unwrap();
            let keys   = dict.keys();
            let values = dict.values().as_any().downcast_ref::<StringArray>().unwrap();
            let k = keys.value(row) as usize;
            write_str(buf, values.value(k));
        }
        DataType::Boolean => {
            let v = col.as_any().downcast_ref::<BooleanArray>().unwrap().value(row);
            buf.extend_from_slice(if v { b"true" } else { b"false" });
        }
        DataType::Int8   => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int8Array>()  .unwrap().value(row)).as_bytes()); }
        DataType::Int16  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int16Array>() .unwrap().value(row)).as_bytes()); }
        DataType::Int32  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int32Array>() .unwrap().value(row)).as_bytes()); }
        DataType::Int64  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<Int64Array>() .unwrap().value(row)).as_bytes()); }
        DataType::UInt8  => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<UInt8Array>() .unwrap().value(row)).as_bytes()); }
        DataType::UInt16 => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<UInt16Array>().unwrap().value(row)).as_bytes()); }
        DataType::UInt32 => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<UInt32Array>().unwrap().value(row)).as_bytes()); }
        DataType::UInt64 => { let mut b = itoa::Buffer::new(); buf.extend_from_slice(b.format(col.as_any().downcast_ref::<UInt64Array>().unwrap().value(row)).as_bytes()); }
        DataType::Decimal128(_, _) => {
            let arr = col.as_any().downcast_ref::<Decimal128Array>().unwrap();
            write_str(buf, &arr.value_as_string(row));
        }
        DataType::Decimal256(_, _) => {
            let arr = col.as_any().downcast_ref::<Decimal256Array>().unwrap();
            write_str(buf, &arr.value_as_string(row));
        }
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
        // Any dtype not handled above must have been pre-cast to Utf8 by
        // `cast_for_serialize`. Hitting this arm is a bug — surface it as a
        // visible JSON string rather than a silent null so it can't be
        // mistaken for a real NULL value.
        other => write_str(buf, &format!("<unsupported dtype: {other:?}>")),
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
