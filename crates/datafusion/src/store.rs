use std::any::Any;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use arrow::array::{
    Array, ArrayRef, BooleanArray, Decimal128Array, Decimal256Array, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, LargeStringArray, RecordBatch, Scalar,
    StringArray, StringViewArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::compute;
use arrow::compute::kernels::cmp::{eq, gt, gt_eq, lt, lt_eq, neq};
use arrow::datatypes::{DataType, Field, Schema};
use async_trait::async_trait;
use parquet::arrow::AsyncArrowWriter;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
use parquet::arrow::async_writer::ParquetObjectWriter;
use parquet::file::properties::WriterProperties;
use serde_json::Value as JsonValue;

use datafusion::catalog::information_schema::InformationSchemaProvider;
use datafusion::catalog::{CatalogProviderList, SchemaProvider};
use datafusion::datasource::file_format::parquet::ParquetFormat;
use datafusion::datasource::listing::{
    ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
};
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::error::Result as DfResult;
use datafusion::execution::cache::DefaultListFilesCache;
use datafusion::execution::cache::cache_manager::CacheManagerConfig;
use datafusion::execution::disk_manager::DiskManagerBuilder;
use datafusion::execution::memory_pool::FairSpillPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};
use datafusion::prelude::{SessionConfig, SessionContext};
use datafusion::scalar::ScalarValue;

use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt};
use url::Url;

use datapress_core::backend::{
    ArrowIpcStream, Backend, CascadeHandle, DatasetStatus, DatasetSummary, ReloadStats,
    arrow_ipc_stream_channel,
};
use datapress_core::config::{
    AddressingStyle, AppConfig, DataFusionConfig, DatasetConfig, IndexConfig, IndexMode,
    MaterializeResidency, OnStart, Partitioning, ResolvedCreds, S3Config, ServerConfig, SourceKind,
};
use datapress_core::errors::AppError;
use datapress_core::models::{CountRequest, Predicate, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};
use datapress_core::storage::{
    GenerationManifest, MaterializationStorage, build_materialization_storage, fnv1a_hash,
    gc_generations, list_complete_generations, new_ulid, now_rfc3339,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Row-group size used when writing sorted (`sort_by` set) materialized parquet
/// files. Smaller groups enable row-group min/max pruning at query time — the
/// 1 MiB default (1M rows) spans the entire dataset for typical materializations
/// and makes pruning impossible (R2B.5 MUST: "prune effectively").
/// 128 K rows gives 2+ groups for datasets > 128 K rows.
const MAT_SORTED_ROW_GROUP_SIZE: usize = 131_072; // 128 KiB rows

/// Hasher-swapped map alias. The default `std::collections::HashMap` uses
/// SipHash (DoS-resistant but slow). The equality index keys are our own
/// column values, not untrusted hash-flood input, so we use `ahash` — a much
/// faster non-cryptographic hasher — on this per-request hot path.
type FastMap<K, V> = HashMap<K, V, ahash::RandomState>;

/// Pre-built equality index: lowercase col name → string-encoded value → sorted row ids.
type EqIndex = FastMap<String, FastMap<String, Vec<u32>>>;

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
    pub schema: DatasetSchema,
    pub data: Vec<RecordBatch>,
    pub arrow_schema: Arc<Schema>,
    pub index: EqIndex,
    pub lazy: bool,
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
    ctx: SessionContext,
    max_page_size: u64,
    /// Original dataset configs, indexed by name. Reload reads the source
    /// path from here — clients can't redirect a reload at an arbitrary file.
    /// Behind an `RwLock` so datasets registered at runtime can be added
    /// without a restart.
    configs: RwLock<HashMap<String, DatasetConfig>>,
    /// Hot-swappable snapshot of all currently loaded datasets.
    datasets: ArcSwap<HashMap<String, Arc<DatasetState>>>,
    /// Per-name reload mutex. Serialises concurrent reloads of the same
    /// dataset; reloads of different datasets proceed in parallel.
    reload_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-dataset lifecycle state (Pending / Building / Published / Failed)
    /// and startup policy. All configured datasets are present, including
    /// those not yet built. ArcSwap so status updates are lock-free reads.
    statuses: ArcSwap<HashMap<String, (DatasetStatus, OnStart)>>,
    /// Cascade notification handle (R4.3). Set once by the server after
    /// building the cascade engine; `None` when no cascade is configured.
    cascade_handle: std::sync::Mutex<Option<CascadeHandle>>,
    /// Phase 2B: server-level materialization storage backend. `None` when
    /// no `[server.storage]` block is configured → all query datasets live
    /// in memory.
    storage: Option<Arc<MaterializationStorage>>,
}

// ---------------------------------------------------------------------------
// Phase 2B — Materialization storage state
// ---------------------------------------------------------------------------

impl Store {
    /// Shared `SessionContext` all datasets are registered in. Handed to the
    /// optional pgwire server so it queries the exact same tables as the HTTP
    /// API (never a fresh context). Clone it before moving it into a task.
    pub fn session_context(&self) -> &SessionContext {
        &self.ctx
    }

    /// Load every dataset declared in `cfg` synchronously (blocking startup).
    ///
    /// Used by tests and by callers that want the old behaviour where the
    /// server only starts after all datasets are ready. For production use
    /// prefer [`Store::new_nonblocking`] + [`Store::spawn_startup_builds`].
    pub async fn load(cfg: &AppConfig) -> Result<Self, AppError> {
        // One-shot init for the deltalake S3 backend. Safe to call more
        // than once — the handlers are idempotent.
        if cfg
            .datasets
            .iter()
            .any(|d| d.source.kind == SourceKind::Delta && d.source.is_s3())
        {
            deltalake::aws::register_handlers(None);
        }

        // NOTE: identifier handling for the raw-SQL endpoint is done by
        // rewriting schema column/table references to quoted canonical
        // names before execution (see `query_sql`). DataFusion's default
        // identifier normalization is left ON so unquoted *aliases* and
        // CTE names stay case-insensitive, matching DuckDB.
        let ctx = build_tuned_context(&cfg.datafusion);
        let mut datasets = HashMap::with_capacity(cfg.datasets.len());
        let mut configs = HashMap::with_capacity(cfg.datasets.len());
        let mut statuses: HashMap<String, (DatasetStatus, OnStart)> =
            HashMap::with_capacity(cfg.datasets.len());

        // Build storage backend once, before any dataset builds.
        let storage: Option<Arc<MaterializationStorage>> = cfg
            .server
            .storage
            .as_ref()
            .map(build_materialization_storage)
            .transpose()
            .map_err(|e| AppError::Internal(format!("server.storage init: {e}")))?
            .map(Arc::new);
        let storage_ref = storage.as_ref();
        // Boot GC: clean up incomplete/orphaned generation directories.
        if let Some(ref stor) = storage {
            boot_gc_storage(stor, &cfg.datasets);
        }

        // Build in topological dependency order so query datasets that
        // depend on others find their dependencies already registered.
        let build_order = cfg
            .topological_dataset_order()
            .map_err(|e| AppError::Internal(format!("startup order error: {e}")))?;

        for idx in build_order {
            let d = &cfg.datasets[idx];
            if d.source.kind == SourceKind::Query {
                log::info!("Loading dataset '{}' (query)", d.name);
            } else {
                log::info!(
                    "Loading dataset '{}' ({} @ {})",
                    d.name,
                    d.source.kind.as_str(),
                    d.source.location
                );
            }
            // Force lazy when the source exceeds the server size threshold.
            // S3-aware: local sources are stat'd, S3 sources are sized by
            // listing the object store under their prefix.
            let d: std::borrow::Cow<'_, DatasetConfig> = match should_force_lazy(d, &cfg.server)
                .await
            {
                Some(bytes) => {
                    log::info!(
                        "dataset '{}': {:.1} MiB exceeds force_lazy_above_mb = {} → forcing lazy",
                        d.name,
                        bytes as f64 / (1024.0 * 1024.0),
                        cfg.server.force_lazy_above_mb
                    );
                    let mut forced = d.clone();
                    forced.lazy = true;
                    std::borrow::Cow::Owned(forced)
                }
                None => std::borrow::Cow::Borrowed(d),
            };
            let d = d.as_ref();
            let (state, provider) = match build_dataset(d, &ctx, storage_ref).await {
                Ok(built) => built,
                Err(AppError::EmptyDataset(msg)) => {
                    log::warn!("skipping empty dataset '{}': {msg}", d.name);
                    continue;
                }
                // An S3 source we're not authorized to read (bad creds, no
                // bucket/prefix policy, expired token) returns a 403. Don't
                // abort the whole registry for one inaccessible dataset —
                // log and skip it, exactly like an empty source. The rest of
                // the datasets still load and serve traffic.
                Err(e) if d.source.is_s3() && is_s3_access_denied(&e.to_string()) => {
                    log::warn!(
                        "skipping dataset '{}': S3 access denied — check credentials \
                         and bucket policy ({e})",
                        d.name
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            ctx.register_table(d.name.as_str(), provider)?;
            datasets.insert(d.name.clone(), Arc::new(state));
            configs.insert(d.name.clone(), d.clone());
            statuses.insert(
                d.name.clone(),
                (DatasetStatus::Published, d.on_start.clone()),
            );
        }
        Ok(Self {
            ctx,
            max_page_size: cfg.server.max_page_size.max(1),
            configs: RwLock::new(configs),
            datasets: ArcSwap::from_pointee(datasets),
            reload_locks: Mutex::new(HashMap::new()),
            statuses: ArcSwap::from_pointee(statuses),
            cascade_handle: std::sync::Mutex::new(None),
            storage,
        })
    }

    /// Create the store with all configured datasets in `Pending` state.
    /// No datasets are built yet. Call [`Store::spawn_startup_builds`] after
    /// binding the HTTP listener to kick off background builds.
    pub async fn new_nonblocking(cfg: &AppConfig) -> Result<Self, AppError> {
        if cfg
            .datasets
            .iter()
            .any(|d| d.source.kind == SourceKind::Delta && d.source.is_s3())
        {
            deltalake::aws::register_handlers(None);
        }
        let ctx = build_tuned_context(&cfg.datafusion);
        let statuses: HashMap<String, (DatasetStatus, OnStart)> = cfg
            .datasets
            .iter()
            .map(|d| (d.name.clone(), (DatasetStatus::Pending, d.on_start.clone())))
            .collect();
        let configs: HashMap<String, DatasetConfig> = cfg
            .datasets
            .iter()
            .map(|d| (d.name.clone(), d.clone()))
            .collect();
        Ok(Self {
            ctx,
            max_page_size: cfg.server.max_page_size.max(1),
            configs: RwLock::new(configs),
            datasets: ArcSwap::from_pointee(HashMap::new()),
            reload_locks: Mutex::new(HashMap::new()),
            statuses: ArcSwap::from_pointee(statuses),
            cascade_handle: std::sync::Mutex::new(None),
            storage: cfg
                .server
                .storage
                .as_ref()
                .map(build_materialization_storage)
                .transpose()
                .map_err(|e| AppError::Internal(format!("server.storage init: {e}")))?
                .map(Arc::new),
        })
    }

    /// Spawn background tasks to build all `eager` datasets. `lazy` datasets
    /// are left `Pending` until their first query; `skip` datasets are left
    /// `Pending` until an explicit reload. At most `max_concurrent` builds
    /// run simultaneously; the spawned tasks are fire-and-forget (they update
    /// the store's status as they progress).
    ///
    /// Must be called on an `Arc<Self>` so the spawned tasks can hold a
    /// reference to the store beyond this call.
    pub fn spawn_startup_builds(
        self: Arc<Self>,
        max_concurrent: usize,
        server_cfg: &datapress_core::config::ServerConfig,
    ) {
        use std::collections::HashSet;
        use tokio::sync::Semaphore;
        let sem = Arc::new(Semaphore::new(max_concurrent.max(1)));
        // Compute topological levels so dependencies build before dependents.
        let levels = self.startup_levels();
        let server_cfg = server_cfg.clone();
        let store = self;
        tokio::spawn(async move {
            // R4.2: track failed datasets so transitive eager dependents are
            // marked Failed without attempting a build.
            let mut globally_failed: HashSet<String> = HashSet::new();
            for level in levels {
                // Pre-mark any dataset in this level whose upstream failed.
                let mut to_build = Vec::new();
                for (name, dcfg) in level {
                    let failed_dep = dcfg
                        .source
                        .depends_on
                        .iter()
                        .find(|dep| globally_failed.contains(*dep))
                        .cloned();
                    if let Some(upstream) = failed_dep {
                        log::warn!(
                            "startup: skipping '{}' — upstream '{}' failed \
                             (upstream_unavailable)",
                            name,
                            upstream
                        );
                        store.set_status(&name, DatasetStatus::Failed);
                        globally_failed.insert(name);
                    } else {
                        to_build.push((name, dcfg));
                    }
                }
                let level_names: Vec<String> = to_build.iter().map(|(n, _)| n.clone()).collect();
                let mut handles = Vec::with_capacity(to_build.len());
                for (name, dcfg) in to_build {
                    let sem = Arc::clone(&sem);
                    let store = Arc::clone(&store);
                    let server_cfg = server_cfg.clone();
                    let h = tokio::spawn(async move {
                        let _permit = sem.acquire().await;
                        store.build_one_startup(name, dcfg, server_cfg).await;
                    });
                    handles.push(h);
                }
                for h in handles {
                    let _ = h.await;
                }
                // Collect failures from this level to propagate downward.
                for name in &level_names {
                    if let Some((DatasetStatus::Failed, _)) = store.get_status_entry(name) {
                        globally_failed.insert(name.clone());
                    }
                }
            }
        });
    }

    /// Compute topological build levels for startup. Each level is a batch of
    /// `(name, config)` pairs that can build concurrently; a dataset in level
    /// `k` depends only on datasets in levels `0..k`. Datasets with
    /// `on_start != Eager` are excluded.
    ///
    /// Uses Kahn's algorithm: nodes with in-degree 0 form the first level;
    /// removing them exposes the next level, and so on.
    fn startup_levels(&self) -> Vec<Vec<(String, DatasetConfig)>> {
        use datapress_core::config::{OnStart, SourceKind};
        use std::collections::HashMap;

        let snap = self.statuses.load();
        let configs = self.configs.read().unwrap();

        // Only eager datasets participate in startup builds.
        let eager: Vec<(&str, &DatasetConfig)> = configs
            .iter()
            .filter(|(name, _)| {
                snap.get(*name)
                    .map(|(_, on_start)| *on_start == OnStart::Eager)
                    .unwrap_or(false)
            })
            .map(|(name, cfg)| (name.as_str(), cfg))
            .collect();

        if eager.is_empty() {
            return vec![];
        }

        // Build index: name → position in `eager`.
        let idx_map: HashMap<&str, usize> = eager
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (*name, i))
            .collect();

        let n = eager.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n]; // adj[dep] = [dependents]

        for (i, (_, cfg)) in eager.iter().enumerate() {
            if cfg.source.kind == SourceKind::Query {
                for dep_name in &cfg.source.depends_on {
                    // dep_name may be a non-eager dataset (already built); skip.
                    if let Some(&dep_idx) = idx_map.get(dep_name.as_str()) {
                        adj[dep_idx].push(i);
                        in_degree[i] += 1;
                    }
                }
            }
        }

        // Kahn: group by generation level.
        let mut current: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut levels = Vec::new();
        while !current.is_empty() {
            let batch: Vec<(String, DatasetConfig)> = current
                .iter()
                .map(|&i| (eager[i].0.to_string(), eager[i].1.clone()))
                .collect();
            levels.push(batch);
            let mut next = Vec::new();
            for node in &current {
                for &dep in &adj[*node] {
                    in_degree[dep] -= 1;
                    if in_degree[dep] == 0 {
                        next.push(dep);
                    }
                }
            }
            current = next;
        }
        levels
    }

    /// Build a single dataset during non-blocking startup (called from
    /// `spawn_startup_builds` tasks). Updates the status ArcSwap.
    async fn build_one_startup(
        self: &Arc<Self>,
        name: String,
        dcfg: DatasetConfig,
        server_cfg: datapress_core::config::ServerConfig,
    ) {
        self.set_status(&name, DatasetStatus::Building);
        // Apply force_lazy check before building.
        let effective_cfg: std::borrow::Cow<'_, DatasetConfig> =
            match should_force_lazy(&dcfg, &server_cfg).await {
                Some(bytes) => {
                    log::info!(
                        "dataset '{}': {:.1} MiB exceeds force_lazy_above_mb = {} → forcing lazy",
                        dcfg.name,
                        bytes as f64 / (1024.0 * 1024.0),
                        server_cfg.force_lazy_above_mb
                    );
                    let mut forced = dcfg.clone();
                    forced.lazy = true;
                    std::borrow::Cow::Owned(forced)
                }
                None => std::borrow::Cow::Borrowed(&dcfg),
            };
        if effective_cfg.source.kind == datapress_core::config::SourceKind::Query {
            log::info!("Startup: building dataset '{}' (query)", effective_cfg.name);
        } else {
            log::info!(
                "Startup: building dataset '{}' ({} @ {})",
                effective_cfg.name,
                effective_cfg.source.kind.as_str(),
                effective_cfg.source.location
            );
        }
        // R4.5: capture dependency snapshots before planning so a concurrent
        // reload cannot change the data under this build.
        let _dep_snaps: Vec<_> = effective_cfg
            .source
            .depends_on
            .iter()
            .filter_map(|dep| self.dataset(dep).ok())
            .collect();
        let result = build_dataset(effective_cfg.as_ref(), &self.ctx, self.storage.as_ref()).await;
        match result {
            Ok((state, provider)) => {
                let rows = state.num_rows();
                if let Err(e) = self.ctx.register_table(name.as_str(), provider) {
                    log::error!("startup: failed to register dataset '{name}': {e}");
                    self.set_status(&name, DatasetStatus::Failed);
                    return;
                }
                // Update the effective config (lazy may have been forced).
                self.configs
                    .write()
                    .unwrap()
                    .insert(name.clone(), effective_cfg.into_owned());
                let mut new_map = (**self.datasets.load()).clone();
                new_map.insert(name.clone(), Arc::new(state));
                self.datasets.store(Arc::new(new_map));
                self.set_status(&name, DatasetStatus::Published);
                log::info!("Startup: dataset '{name}' published ({rows} rows)");
            }
            Err(AppError::EmptyDataset(msg)) => {
                log::warn!("startup: skipping empty dataset '{name}': {msg}");
                self.set_status(&name, DatasetStatus::Failed);
            }
            Err(e) if dcfg.source.is_s3() && is_s3_access_denied(&e.to_string()) => {
                log::warn!("startup: S3 access denied for '{name}': {e}");
                self.set_status(&name, DatasetStatus::Failed);
            }
            Err(e) => {
                log::error!("startup: failed to build dataset '{name}': {e}");
                self.set_status(&name, DatasetStatus::Failed);
            }
        }
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

    /// Snapshot rule (R4.5 / T1.2): capture one strong `Arc` clone of each
    /// named dataset's currently-published state, *before* any planning
    /// begins. Holding these clones through the life of the query ensures
    /// a concurrent reload (which atomically swaps the ArcSwap entry)
    /// cannot change the data that this query reads mid-execution.
    fn capture_snapshots(&self, datasets: &[String]) -> Result<Vec<Arc<DatasetState>>, AppError> {
        datasets.iter().map(|n| self.dataset(n)).collect()
    }

    /// Read the current `(DatasetStatus, OnStart)` for `name`.
    fn get_status_entry(&self, name: &str) -> Option<(DatasetStatus, OnStart)> {
        self.statuses.load().get(name).cloned()
    }

    /// Atomically update the status for `name` to `new_status`, preserving
    /// the existing `OnStart` policy.
    fn set_status(&self, name: &str, new_status: DatasetStatus) {
        let mut new_map = (**self.statuses.load()).clone();
        if let Some(entry) = new_map.get_mut(name) {
            entry.0 = new_status;
        }
        self.statuses.store(Arc::new(new_map));
    }

    /// Ensure `name` is ready to serve queries. If the dataset is:
    /// - `Published`: no-op.
    /// - `Pending` + `Lazy`: triggers a first-touch build (coalesced via the
    ///   reload mutex) and waits for it.
    /// - Any other non-published state: returns `AppError::NotReady`.
    pub async fn ensure_ready(&self, name: &str) -> Result<(), AppError> {
        match self.get_status_entry(name) {
            Some((DatasetStatus::Published, _)) => Ok(()),
            Some((DatasetStatus::Pending, OnStart::Lazy)) => self.first_touch_build(name).await,
            Some((status, _)) => Err(AppError::NotReady {
                dataset: name.to_string(),
                state: format!("{status:?}").to_lowercase(),
            }),
            None => Err(AppError::NotFound(format!("dataset: {name}"))),
        }
    }

    /// Build a lazy dataset on its first query. Uses the per-dataset reload
    /// mutex to coalesce concurrent first-touches into a single build.
    async fn first_touch_build(&self, name: &str) -> Result<(), AppError> {
        let lock = {
            let mut locks = self.reload_locks.lock().unwrap();
            locks
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        // Re-check after acquiring the lock: another task may have built it.
        if let Some((DatasetStatus::Published, _)) = self.get_status_entry(name) {
            return Ok(());
        }
        let cfg = self
            .configs
            .read()
            .unwrap()
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("dataset: {name}")))?
            .clone();
        self.set_status(name, DatasetStatus::Building);
        log::info!("Lazy first-touch: building dataset '{name}'");
        match build_dataset(&cfg, &self.ctx, self.storage.as_ref()).await {
            Ok((state, provider)) => {
                let rows = state.num_rows();
                let _ = self.ctx.deregister_table(name);
                self.ctx.register_table(name, provider)?;
                let mut new_map = (**self.datasets.load()).clone();
                new_map.insert(name.to_string(), Arc::new(state));
                self.datasets.store(Arc::new(new_map));
                self.set_status(name, DatasetStatus::Published);
                log::info!("Lazy first-touch: dataset '{name}' published ({rows} rows)");
                Ok(())
            }
            Err(e) => {
                self.set_status(name, DatasetStatus::Failed);
                Err(e)
            }
        }
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
            let inner = trimmed
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(trimmed);
            return Ok(inner.to_string());
        }

        let first = match st.data.iter().find(|b| b.num_rows() > 0) {
            Some(b) => b,
            None => return Ok("null".into()),
        };
        let arr = serialize(&first.slice(0, 1))?;
        // strip the outer [] to return a single object
        let trimmed = arr.trim();
        let inner = trimmed
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
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
            .read()
            .unwrap()
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

        self.reload_inner(name, &cfg).await
    }

    /// Like [`reload`] but skips if the per-dataset mutex is already held
    /// (returns `Ok(None)` in that case — R3.2 coalescing).
    pub async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
        let cfg = self
            .configs
            .read()
            .unwrap()
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("dataset: {name}")))?
            .clone();

        let lock = {
            let mut locks = self.reload_locks.lock().unwrap();
            locks
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        match lock.try_lock() {
            Err(_) => Ok(None), // already locked — coalesce
            Ok(_guard) => self.reload_inner(name, &cfg).await.map(Some),
        }
    }

    /// Core reload logic shared by [`reload`] and [`try_reload`].
    /// The caller must hold the per-dataset reload lock.
    async fn reload_inner(&self, name: &str, cfg: &DatasetConfig) -> Result<ReloadStats, AppError> {
        let started = std::time::Instant::now();
        self.set_status(name, DatasetStatus::Building);

        if let Some(cache) = self.ctx.runtime_env().cache_manager.get_list_files_cache() {
            cache.clear();
        }

        // 3. Heavy lifting (source read + index build).
        // R4.5: capture dependency snapshots before planning so a concurrent
        // reload of a dependency cannot change the data under this build.
        let _dep_snaps: Vec<_> = cfg
            .source
            .depends_on
            .iter()
            .filter_map(|dep| self.dataset(dep).ok())
            .collect();
        let build_result = build_dataset(cfg, &self.ctx, self.storage.as_ref()).await;
        let (state, provider) = match build_result {
            Ok(v) => v,
            Err(e) => {
                // Keep-last-good: revert to Published if we had a live
                // generation, otherwise Failed.
                if self.datasets.load().contains_key(name) {
                    self.set_status(name, DatasetStatus::Published);
                } else {
                    self.set_status(name, DatasetStatus::Failed);
                }
                return Err(e);
            }
        };
        let rows = state.num_rows();

        // 4. Atomic swap.
        let _ = self.ctx.deregister_table(name)?;
        self.ctx.register_table(name, provider)?;

        let mut new_map = (**self.datasets.load()).clone();
        new_map.insert(name.to_string(), Arc::new(state));
        self.datasets.store(Arc::new(new_map));
        self.set_status(name, DatasetStatus::Published);

        let elapsed_ms = started.elapsed().as_millis();
        log::info!("reloaded dataset '{name}': {rows} rows in {elapsed_ms} ms");
        // R4.3: notify cascade engine of successful publish.
        if let Some(h) = self.cascade_handle.lock().unwrap().as_ref() {
            h.notify_published(name);
        }
        Ok(ReloadStats { rows, elapsed_ms })
    }

    /// Register a brand-new dataset from `cfg` at runtime. Opens the source,
    /// registers a provider in the shared `SessionContext`, and inserts it
    /// into the live snapshot so it is immediately queryable — no restart.
    pub async fn register(&self, cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        cfg.validate_for_register()?;

        // Fast pre-check before taking the (async) per-name lock.
        if self.datasets.load().contains_key(&cfg.name) {
            return Err(AppError::InvalidValue(format!(
                "dataset '{}' already exists",
                cfg.name
            )));
        }

        // Serialise against a concurrent register/reload of the same name.
        let lock = {
            let mut locks = self.reload_locks.lock().unwrap();
            locks
                .entry(cfg.name.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        // Re-check under the lock — another task may have won the race.
        if self.datasets.load().contains_key(&cfg.name) {
            return Err(AppError::InvalidValue(format!(
                "dataset '{}' already exists",
                cfg.name
            )));
        }

        // One-shot init for the deltalake S3 backend, mirroring `load`.
        // Idempotent — safe to call again for a runtime-registered dataset.
        if cfg.source.kind == SourceKind::Delta && cfg.source.is_s3() {
            deltalake::aws::register_handlers(None);
        }

        let started = std::time::Instant::now();
        let (state, provider) = build_dataset(&cfg, &self.ctx, self.storage.as_ref()).await?;
        let rows = state.num_rows();
        let columns = state.schema.columns.len();

        self.ctx.register_table(cfg.name.as_str(), provider)?;

        let mut new_map = (**self.datasets.load()).clone();
        new_map.insert(cfg.name.clone(), Arc::new(state));
        self.datasets.store(Arc::new(new_map));
        self.configs
            .write()
            .unwrap()
            .insert(cfg.name.clone(), cfg.clone());
        // Register status (on_start = Eager for runtime-registered datasets).
        let mut new_statuses = (**self.statuses.load()).clone();
        new_statuses.insert(cfg.name.clone(), (DatasetStatus::Published, OnStart::Eager));
        self.statuses.store(Arc::new(new_statuses));

        let elapsed_ms = started.elapsed().as_millis();
        log::info!(
            "registered dataset '{}' ({} @ {}): {rows} rows in {elapsed_ms} ms",
            cfg.name,
            cfg.source.kind.as_str(),
            cfg.source.location
        );
        Ok(DatasetSummary {
            name: cfg.name,
            columns,
            rows,
            lazy: cfg.lazy,
        })
    }

    /// Run a `QueryRequest` against `name`. Empty predicates → O(1) Arrow
    /// slice. Otherwise → DataFusion SQL on the single registered table.
    /// Lazy datasets skip the in-memory hot paths and always dispatch to SQL.
    pub async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        let batch = self.query_batch(name, req).await?;
        if batch.num_rows() == 0 {
            return Ok("[]".to_string());
        }
        serialize(&batch)
    }

    /// Rewrite registered table / column references in a raw-SQL string to
    /// their canonical, quoted spelling so matching is case-insensitive
    /// (like DuckDB). Builds the lookup from every currently loaded
    /// dataset; unknown identifiers (aliases, CTE names) are left for the
    /// engine's default normalization to handle.
    fn canonicalize_sql(&self, sql: &str) -> String {
        let snap = self.datasets.load();
        let mut tables: HashMap<String, String> = HashMap::with_capacity(snap.len());
        let mut columns: HashMap<String, String> = HashMap::new();
        for (name, state) in snap.iter() {
            tables.insert(name.to_lowercase(), name.clone());
            for col in &state.schema.columns {
                columns
                    .entry(col.name.to_lowercase())
                    .or_insert_with(|| col.name.clone());
            }
        }
        datapress_core::sql::canonicalize_identifiers(sql, &tables, &columns)
    }

    /// Execute a pre-validated raw `SELECT` and return the JSON `data`
    /// array. The statement has already passed
    /// [`datapress_core::sql::validate`]; here it is wrapped in an outer
    /// `LIMIT max_rows` so the result is bounded regardless of the user's
    /// own clauses, executed through the shared `SessionContext`, and run
    /// through the same fast JSON encoder as [`Self::query`].
    ///
    /// `datasets` lists every dataset name the statement touches. An Arc
    /// clone of each is captured before planning (snapshot rule R4.5/T1.2)
    /// so that a concurrent reload cannot partially swap the data that this
    /// query reads.
    pub async fn query_sql(
        &self,
        sql: &str,
        datasets: &[String],
        max_rows: u64,
    ) -> Result<String, AppError> {
        // Snapshot rule (R4.5 / T1.2): capture a strong reference to each
        // referenced dataset's published state BEFORE planning begins. Any
        // reload that fires concurrently will atomically publish a new Arc
        // into the ArcSwap, but our clones keep the previous generation
        // alive through the entire query execution.
        let _snapshots = self.capture_snapshots(datasets)?;
        let cap = max_rows.max(1);
        let sql = self.canonicalize_sql(sql);
        // DESCRIBE yields a schema listing and cannot be nested in a
        // subquery on DataFusion, so run it directly. Its row count is
        // bounded by the column count and the slice below still enforces
        // `cap`; everything else is wrapped in an outer LIMIT.
        let wrapped = if datapress_core::sql::is_describe(&sql) {
            sql
        } else {
            format!("SELECT * FROM ({sql}) AS _datapress_sql LIMIT {cap}")
        };
        let df = self.ctx.sql(&wrapped).await?;
        let batches = df.collect().await?;
        if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
            return Ok("[]".to_string());
        }
        let batch = if batches.len() == 1 {
            batches.into_iter().next().expect("checked len")
        } else {
            compute::concat_batches(&batches[0].schema(), batches.iter())?
        };
        // Defence in depth: the outer LIMIT already bounds the row count,
        // but slice anyway so a planning quirk can never blow the cap.
        let batch = if batch.num_rows() as u64 > cap {
            batch.slice(0, cap as usize)
        } else {
            batch
        };
        serialize(&batch)
    }

    /// Same plan as [`Self::query_sql`], but encode the bounded result as
    /// an Arrow IPC stream instead of JSON. Backs the Arrow
    /// content-negotiated branch of `POST /api/v1/sql`.
    pub async fn query_sql_arrow_stream(
        &self,
        sql: &str,
        datasets: &[String],
        max_rows: u64,
    ) -> Result<ArrowIpcStream, AppError> {
        // Snapshot rule — same as query_sql.
        let _snapshots = self.capture_snapshots(datasets)?;
        let cap = max_rows.max(1);
        let sql = self.canonicalize_sql(sql);
        // DESCRIBE cannot be nested in a subquery on DataFusion (see
        // `query_sql`); run it directly. Its output is bounded by the
        // column count, so the missing outer LIMIT is harmless.
        let wrapped = if datapress_core::sql::is_describe(&sql) {
            sql
        } else {
            format!("SELECT * FROM ({sql}) AS _datapress_sql LIMIT {cap}")
        };
        let df = self.ctx.sql(&wrapped).await?;
        let batches = df.collect().await?;
        Ok(stream_arrow_batches(batches))
    }

    /// Same plan as [`Self::query`], but encode the result page as an
    /// Arrow IPC stream (one schema message + one batch + EOS). Empty
    /// results still produce a valid, self-describing zero-batch stream.
    pub async fn query_arrow(&self, name: &str, req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        let batch = self.query_batch(name, req).await?;
        let schema = batch.schema();
        let mut buf = Vec::with_capacity(8 * 1024);
        {
            let mut w = arrow::ipc::writer::StreamWriter::try_new(&mut buf, schema.as_ref())?;
            if batch.num_rows() > 0 {
                w.write(&batch)?;
            }
            w.finish()?;
        }
        Ok(buf)
    }

    pub async fn query_arrow_stream(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let batches = self.query_batches(name, req).await?;
        Ok(stream_arrow_batches(batches))
    }
    pub async fn query_arrow_stream_all(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let batches = self.query_batches_all(name, req).await?;
        Ok(stream_arrow_batches(batches))
    }

    /// Encode the entire dataset as a single self-contained Parquet file.
    ///
    /// Collects every row (all columns, no predicates, no paging) and runs
    /// it through a single [`parquet::arrow::ArrowWriter`], so the result
    /// carries the row-group + footer metadata a Parquet reader needs to
    /// answer `count(*)` straight from the footer. Powers the cached
    /// `GET /datasets/{name}/parquet` HTTP endpoint.
    pub async fn parquet(&self, name: &str) -> Result<bytes::Bytes, AppError> {
        // All rows, all columns, no predicates / ordering / limit.
        let req = QueryRequest {
            columns: Vec::new(),
            predicates: Vec::new(),
            group_by: Vec::new(),
            aggregations: Vec::new(),
            having: Vec::new(),
            distinct: false,
            order_by: Vec::new(),
            limit: None,
            page: 1,
            page_size: 1,
        };
        let st = self.dataset(name)?;
        let batches = self.query_batches_all(name, &req).await?;
        // Use the actual batch schema when we have rows so the writer schema
        // matches exactly (projection/nullability); fall back to the
        // dataset schema for an empty dataset.
        let schema = batches
            .first()
            .map(|b| b.schema())
            .unwrap_or_else(|| st.arrow_schema.clone());

        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        {
            let props = parquet::file::properties::WriterProperties::builder()
                .set_compression(parquet::basic::Compression::SNAPPY)
                .build();
            let mut writer = parquet::arrow::ArrowWriter::try_new(&mut buf, schema, Some(props))
                .map_err(|e| AppError::Internal(format!("parquet writer init: {e}")))?;
            for batch in &batches {
                if batch.num_rows() > 0 {
                    writer
                        .write(batch)
                        .map_err(|e| AppError::Internal(format!("parquet write: {e}")))?;
                }
            }
            writer
                .close()
                .map_err(|e| AppError::Internal(format!("parquet finish: {e}")))?;
        }
        Ok(bytes::Bytes::from(buf))
    }

    /// Compute the result page as a single `RecordBatch`. Shared between
    /// the JSON and Arrow IPC encoders.
    async fn query_batch(&self, name: &str, req: &QueryRequest) -> Result<RecordBatch, AppError> {
        let batches = self.query_batches(name, req).await?;
        if batches.is_empty() {
            return Ok(RecordBatch::new_empty(Arc::new(
                arrow::datatypes::Schema::empty(),
            )));
        }
        if batches.len() == 1 {
            return Ok(batches.into_iter().next().expect("checked len"));
        }
        if batches.iter().all(|b| b.num_rows() == 0) {
            return Ok(RecordBatch::new_empty(batches[0].schema()));
        }
        let batch = compute::concat_batches(&batches[0].schema(), batches.iter())?;
        Ok(batch)
    }

    /// Compute the result page as Arrow batches. Arrow IPC responses can
    /// write these directly, while JSON callers concatenate via
    /// [`Self::query_batch`] for the existing row conversion path.
    async fn query_batches(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<Vec<RecordBatch>, AppError> {
        let st = self.dataset(name)?;

        let page = req.page.max(1);
        let page_size = req.page_size.clamp(1, self.max_page_size);
        let offset = ((page - 1) * page_size) as usize;
        let limit = page_size as usize;

        self.query_batches_inner(st, req, Some((offset, limit)))
            .await
    }

    /// Compute all matching rows as Arrow batches for the one-request
    /// streaming endpoint. `page` and `page_size` are intentionally ignored;
    /// optional `limit` still caps the total result size.
    async fn query_batches_all(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<Vec<RecordBatch>, AppError> {
        let st = self.dataset(name)?;
        self.query_batches_inner(st, req, None).await
    }

    async fn query_batches_inner(
        &self,
        st: Arc<DatasetState>,
        req: &QueryRequest,
        page_window: Option<(usize, usize)>,
    ) -> Result<Vec<RecordBatch>, AppError> {
        let (offset, limit) = page_window.unwrap_or((0, req.limit.unwrap_or(u64::MAX) as usize));

        // In-memory hot paths only fire when:
        //   - the dataset is materialised,
        //   - the caller did not ask for ordering,
        //   - and did not ask for a hard `limit` cap on a paged request.
        // Both of the latter two require sorting / capping that the SQL
        // engine handles uniformly across all data types.
        let can_fast_path = !st.lazy
            && req.order_by.is_empty()
            && (page_window.is_none() || req.limit.is_none())
            && req.group_by.is_empty()
            && !req.distinct;

        if can_fast_path {
            let total = st.num_rows();

            // No predicates -> O(1) raw Arrow slices over resident batches,
            // no engine overhead.
            if req.predicates.is_empty() {
                if page_window.is_none() && req.limit.is_none() {
                    return st
                        .data
                        .iter()
                        .cloned()
                        .map(|batch| project(&st.schema, batch, &req.columns))
                        .collect();
                }
                let start = offset.min(total);
                let len = limit.min(total - start);
                let batch = slice_global(&st.data, &st.arrow_schema, start, len)?;
                return Ok(vec![project(&st.schema, batch, &req.columns)?]);
            }

            // Index fast path: if every predicate is eq/in on an indexed column,
            // resolve via the pre-built equality index.
            if let Some(rows) = try_index(&st.index, &req.predicates) {
                let batch = take_page(&st.data, &st.arrow_schema, &rows, offset, limit)?;
                return Ok(vec![project(&st.schema, batch, &req.columns)?]);
            }
        }

        // Fallback (and only path for lazy datasets): DataFusion SQL.
        let (sql, params) = match page_window {
            Some(_) => build_query_sql(&st.schema, req, self.max_page_size)?,
            None => build_query_stream_sql(&st.schema, req)?,
        };
        let mut df = self.ctx.sql(&sql).await?;
        if !params.is_empty() {
            df = df.with_param_values(params)?;
        }
        let batches = df.collect().await?;
        if batches.is_empty() || batches.iter().all(|b| b.num_rows() == 0) {
            let schema = batches
                .first()
                .map(|b| b.schema())
                .unwrap_or_else(|| Arc::new(arrow::datatypes::Schema::empty()));
            return Ok(vec![RecordBatch::new_empty(schema)]);
        }
        Ok(batches)
    }
}

fn stream_arrow_batches(batches: Vec<RecordBatch>) -> ArrowIpcStream {
    let schema = batches
        .first()
        .map(|batch| batch.schema())
        .unwrap_or_else(|| Arc::new(arrow::datatypes::Schema::empty()));
    let (mut writer, stream) = arrow_ipc_stream_channel(8);

    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<(), AppError> {
            let mut w = arrow::ipc::writer::StreamWriter::try_new(&mut writer, schema.as_ref())?;
            for batch in batches {
                if batch.num_rows() > 0 {
                    w.write(&batch)?;
                }
            }
            w.finish()?;
            Ok(())
        })();
        if let Err(err) = result {
            log::error!("datafusion arrow stream failed: {err}");
            writer.send_error(err);
        }
    });

    stream
}

impl Store {
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

        // Fallback: DataFusion SQL — same predicate translation as `query`,
        // with predicate values bound as typed parameters.
        let (sql, params) = build_count_sql(&st.schema, &req.predicates)?;
        let mut df = self.ctx.sql(&sql).await?;
        if !params.is_empty() {
            df = df.with_param_values(params)?;
        }
        let batches = df.collect().await?;
        let n = batches
            .first()
            .and_then(|b| {
                b.column(0)
                    .as_any()
                    .downcast_ref::<arrow::array::Int64Array>()
            })
            .filter(|a| !a.is_empty())
            .map(|a| a.value(0))
            .unwrap_or(0);
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Dataset loading
// ---------------------------------------------------------------------------

/// Build a tuned `SessionContext` for the whole `Store`.
///
/// Everything here is opt-in via the `[datafusion]` config block; with the
/// defaults this is equivalent to a plain `SessionContext::new()`. Lazy
/// datasets (`ListingTable` over parquet, possibly on S3) benefit most:
///
/// * `pushdown_filters` — evaluate predicates *during* the parquet decode
///   so filtered-out rows in a surviving row group are never materialised.
/// * `reorder_filters` — let the scan reorder pushed-down predicates by
///   selectivity (only meaningful with `pushdown_filters`).
/// * `list_files_cache` — a `ListFilesCache` on the `RuntimeEnv` so repeated
///   lazy queries reuse object-store `LIST` results instead of re-listing the
///   prefix every time — the dominant per-query cost on S3.
///
/// Row-group / page-index / bloom-filter pruning and `metadata_size_hint`
/// are already on by default in this DataFusion version, so they are left
/// untouched.
///
/// Note: DataFusion ≥ 53 installs a *default* `DefaultListFilesCache` on the
/// `RuntimeEnv` even with the defaults below, so a reload must explicitly
/// invalidate it (see [`Store::reload`]) — otherwise a re-listed S3 prefix
/// would keep serving stale, now-deleted object keys.
fn build_tuned_context(cfg: &DataFusionConfig) -> SessionContext {
    let mut config = SessionConfig::new();
    {
        let opts = config.options_mut();
        opts.execution.parquet.pushdown_filters = cfg.pushdown_filters;
        opts.execution.parquet.reorder_filters = cfg.reorder_filters;
        // Expose the `information_schema.tables`/`columns` virtual tables. BI
        // navigators (Npgsql/Power BI, DBeaver) query them alongside
        // `pg_catalog` to enumerate tables, and the pgwire front-end needs
        // them for schema browsing. They live in their own `information_schema`
        // schema, so they don't affect dataset listing (which reads the
        // Store's own registry, not the catalog) or the HTTP endpoints.
        //
        // We deliberately leave DataFusion's *built-in* `information_schema`
        // short-circuit OFF and register our own provider instead (see
        // `register_information_schema_shim`). The built-in provider only
        // implements seven views and can't be extended; BI tools also probe
        // `table_constraints` & friends, so our provider delegates to the
        // built-in views and adds those missing (empty) constraint relations.
        opts.catalog.information_schema = false;
    }

    // Name of the session's default schema, surfaced by the `current_schema()`
    // compatibility UDF registered below.
    let default_schema = config.options().catalog.default_schema.clone();

    if !cfg.list_files_cache {
        let ctx = SessionContext::new_with_config(config);
        register_compat_udfs(&ctx, default_schema);
        register_information_schema_shim(&ctx);
        return ctx;
    }

    // Cache object-store listings so repeated lazy/S3 queries skip the
    // LIST round-trips. A zero TTL means infinite (never expires).
    let ttl = (cfg.list_files_cache_ttl_secs > 0)
        .then(|| Duration::from_secs(cfg.list_files_cache_ttl_secs));
    let list_cache = Arc::new(DefaultListFilesCache::new(
        cfg.list_files_cache_mb.saturating_mul(1024 * 1024),
        ttl,
    ));
    let cache_manager = CacheManagerConfig::default().with_list_files_cache(Some(list_cache));

    let runtime = RuntimeEnvBuilder::new()
        .with_cache_manager(cache_manager)
        .build_arc()
        .expect("failed to build DataFusion runtime env");

    let ctx = SessionContext::new_with_config_rt(config, runtime);
    register_compat_udfs(&ctx, default_schema);
    register_information_schema_shim(&ctx);
    ctx
}

/// Register scalar UDFs that exist on DuckDB but not on DataFusion, so the
/// same portable smoke-test / introspection SQL works on both backends.
///
/// * `current_schema()` — DuckDB returns the active schema (`main`);
///   DataFusion has no such function, so we return the session's default
///   schema name (`public`).
///
/// Note: when the optional `pgwire` feature is enabled and the listener is
/// started, `datafusion_pg_catalog::setup_pg_catalog` runs *after* this on the
/// same shared context and re-registers `current_schema()` (plus siblings) with
/// its own implementation, which also returns `public`. The library version
/// wins there by construction (later `register_udf` replaces by name); we keep
/// ours here so the behavior is identical whether or not pgwire is compiled in
/// or enabled, and so the DuckDB-parity `/api/v1/sql` contract holds by default.
fn register_compat_udfs(ctx: &SessionContext, default_schema: String) {
    ctx.register_udf(ScalarUDF::from(CurrentSchemaUdf::new(default_schema)));
}

// ---------------------------------------------------------------------------
// information_schema constraint views
// ---------------------------------------------------------------------------

/// Register an `information_schema` schema provider that serves DataFusion's
/// seven built-in virtual views *and* the empty constraint relations BI tools
/// probe when loading a table.
///
/// Why not simply `opts.catalog.information_schema = true`? DataFusion's
/// built-in `information_schema` is special-cased in the planner:
/// `SessionState::schema_for_ref` short-circuits *any* `information_schema`
/// reference straight to a fresh built-in `InformationSchemaProvider`, so a
/// schema registered under that name would be silently ignored. We therefore
/// leave that flag OFF (see `build_tuned_context`) and register our own
/// provider here, which *delegates* to the same built-in provider for its
/// seven views (tables/columns/views/schemata/routines/parameters/df_settings)
/// while adding the three constraint views below.
///
/// Power BI / Npgsql query `information_schema.table_constraints` — and,
/// immediately after, `key_column_usage` and `referential_constraints` — when
/// loading a table. DataFusion implements none of them, so the table load
/// fails with "table 'information_schema.table_constraints' not found". These
/// relations are served empty and correctly-shaped: DataPress datasets have no
/// declared constraints, so zero rows is the *truthful* answer, not a stub. If
/// DataPress ever gains declared keys, these views are where they'd surface.
fn register_information_schema_shim(ctx: &SessionContext) {
    let default_catalog = ctx
        .state()
        .config()
        .options()
        .catalog
        .default_catalog
        .clone();
    let Some(catalog) = ctx.catalog(&default_catalog) else {
        return;
    };
    let catalog_list = Arc::clone(ctx.state().catalog_list());
    let provider = Arc::new(InformationSchemaWithConstraints::new(catalog_list));
    // Registering under an existing schema name replaces it; the default
    // catalog has no `information_schema` (the built-in one is virtual), so
    // this is an insert. Ignore the returned previous provider, if any.
    let _ = catalog.register_schema("information_schema", provider);
}

/// An `information_schema` provider that delegates to DataFusion's built-in
/// [`InformationSchemaProvider`] for most views, adds empty PostgreSQL
/// constraint views, and OVERRIDES `columns` so that `data_type` reports
/// PostgreSQL type names (plus a `udt_name` column) instead of Arrow type
/// names.
///
/// Why override `columns`? The built-in view fills `data_type` with Arrow's
/// `DataType::to_string()` (`"Utf8View"`, `"Int64"`,
/// `"Timestamp(Nanosecond, None)"`, …) and has no `udt_name`. PostgreSQL
/// clients — Power BI DirectQuery in particular — read column metadata from
/// `information_schema.columns` and treat the value as a PostgreSQL type name.
/// `"Boolean"` happens to be valid Postgres, but `"Utf8View"` is not, so a
/// string column silently fails to "fold" (Power BI abandons query pushdown
/// and never sends SQL) while a boolean column works. Translating the Arrow
/// type to its PostgreSQL name — aligned with `arrow-pg`'s `into_pg_type`, the
/// same mapping that drives `RowDescription` OIDs and `pg_attribute.atttypid`
/// — makes all three metadata surfaces agree.
#[derive(Debug)]
struct InformationSchemaWithConstraints {
    /// The built-in provider; serves the standard virtual views on demand from
    /// the session's catalog list.
    inner: InformationSchemaProvider,
    /// Catalog list used to walk registered table schemas when building the
    /// PostgreSQL-typed `columns` view (the same source the built-in uses).
    catalog_list: Arc<dyn CatalogProviderList>,
    /// Empty, correctly-shaped constraint relations keyed by lowercase name.
    constraints: HashMap<&'static str, Arc<dyn TableProvider>>,
}

impl InformationSchemaWithConstraints {
    fn new(catalog_list: Arc<dyn CatalogProviderList>) -> Self {
        let mut constraints: HashMap<&'static str, Arc<dyn TableProvider>> = HashMap::new();
        constraints.insert("table_constraints", empty_table(table_constraints_schema()));
        constraints.insert("key_column_usage", empty_table(key_column_usage_schema()));
        constraints.insert(
            "referential_constraints",
            empty_table(referential_constraints_schema()),
        );
        Self {
            inner: InformationSchemaProvider::new(Arc::clone(&catalog_list)),
            catalog_list,
            constraints,
        }
    }

    /// Build the PostgreSQL-typed `information_schema.columns` relation as a
    /// point-in-time [`MemTable`] by walking every registered schema/table
    /// (except `information_schema` itself, mirroring the built-in). Built
    /// fresh on each lookup so runtime-registered datasets appear without a
    /// restart, exactly like the built-in view.
    async fn build_pg_columns(&self) -> DfResult<Arc<dyn TableProvider>> {
        let mut catalog_names_col: Vec<String> = Vec::new();
        let mut schema_names_col: Vec<String> = Vec::new();
        let mut table_names_col: Vec<String> = Vec::new();
        let mut column_names_col: Vec<String> = Vec::new();
        let mut ordinal_col: Vec<u64> = Vec::new();
        let mut is_nullable_col: Vec<String> = Vec::new();
        let mut data_type_col: Vec<String> = Vec::new();
        let mut numeric_precision_col: Vec<Option<u64>> = Vec::new();
        let mut numeric_scale_col: Vec<Option<u64>> = Vec::new();
        let mut udt_name_col: Vec<String> = Vec::new();

        for catalog_name in self.catalog_list.catalog_names() {
            let Some(catalog) = self.catalog_list.catalog(&catalog_name) else {
                continue;
            };
            for schema_name in catalog.schema_names() {
                // Skip our own schema — the built-in `make_columns` also skips
                // `information_schema`, and walking it would recurse into this
                // very provider.
                if schema_name == "information_schema" {
                    continue;
                }
                let Some(schema) = catalog.schema(&schema_name) else {
                    continue;
                };
                for table_name in schema.table_names() {
                    let Some(table) = schema.table(&table_name).await? else {
                        continue;
                    };
                    for (pos, field) in table.schema().fields().iter().enumerate() {
                        let pg = arrow_to_pg_column_type(field.data_type());
                        catalog_names_col.push(catalog_name.clone());
                        schema_names_col.push(schema_name.clone());
                        table_names_col.push(table_name.clone());
                        column_names_col.push(field.name().clone());
                        ordinal_col.push(pos as u64 + 1);
                        is_nullable_col
                            .push(if field.is_nullable() { "YES" } else { "NO" }.to_string());
                        data_type_col.push(pg.data_type.to_string());
                        numeric_precision_col.push(pg.numeric_precision);
                        numeric_scale_col.push(pg.numeric_scale);
                        udt_name_col.push(pg.udt_name);
                    }
                }
            }
        }

        let n = catalog_names_col.len();
        let none_u64 = || -> Vec<Option<u64>> { vec![None; n] };
        let none_utf8 = || -> Vec<Option<String>> { vec![None; n] };
        let schema = pg_columns_schema();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(StringArray::from(catalog_names_col)),
                Arc::new(StringArray::from(schema_names_col)),
                Arc::new(StringArray::from(table_names_col)),
                Arc::new(StringArray::from(column_names_col)),
                Arc::new(UInt64Array::from(ordinal_col)),
                Arc::new(StringArray::from(none_utf8())), // column_default
                Arc::new(StringArray::from(is_nullable_col)),
                Arc::new(StringArray::from(data_type_col)),
                Arc::new(UInt64Array::from(none_u64())), // character_maximum_length
                Arc::new(UInt64Array::from(none_u64())), // character_octet_length
                Arc::new(UInt64Array::from(numeric_precision_col)),
                Arc::new(UInt64Array::from(none_u64())), // numeric_precision_radix
                Arc::new(UInt64Array::from(numeric_scale_col)),
                Arc::new(UInt64Array::from(none_u64())), // datetime_precision
                Arc::new(StringArray::from(none_utf8())), // interval_type
                Arc::new(StringArray::from(udt_name_col)),
            ],
        )?;
        Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
    }
}

#[async_trait]
impl SchemaProvider for InformationSchemaWithConstraints {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn table_names(&self) -> Vec<String> {
        let mut names = self.inner.table_names();
        names.extend(self.constraints.keys().map(|k| (*k).to_string()));
        names
    }

    async fn table(&self, name: &str) -> DfResult<Option<Arc<dyn TableProvider>>> {
        let lower = name.to_ascii_lowercase();
        // Override `columns` with the PostgreSQL-typed variant.
        if lower == "columns" {
            return Ok(Some(self.build_pg_columns().await?));
        }
        if let Some(table) = self.constraints.get(lower.as_str()) {
            return Ok(Some(Arc::clone(table)));
        }
        self.inner.table(name).await
    }

    fn table_exist(&self, name: &str) -> bool {
        self.constraints
            .contains_key(name.to_ascii_lowercase().as_str())
            || self.inner.table_exist(name)
    }
}

/// PostgreSQL type metadata for one Arrow column: the `data_type` and
/// `udt_name` reported by `information_schema.columns`, plus numeric
/// precision/scale for decimals.
struct PgColumnType {
    data_type: &'static str,
    udt_name: String,
    numeric_precision: Option<u64>,
    numeric_scale: Option<u64>,
}

impl PgColumnType {
    fn simple(data_type: &'static str, udt_name: &str) -> Self {
        Self {
            data_type,
            udt_name: udt_name.to_string(),
            numeric_precision: None,
            numeric_scale: None,
        }
    }
}

/// Translate an Arrow [`DataType`] to its PostgreSQL `information_schema`
/// `data_type` name and `udt_name`, aligned with `arrow-pg`'s `into_pg_type`
/// so that `RowDescription` OIDs, `pg_attribute.atttypid`, and
/// `information_schema.columns` all agree. Unmapped types fall back to `text`
/// with a debug log.
fn arrow_to_pg_column_type(dt: &DataType) -> PgColumnType {
    use DataType::*;
    match dt {
        Utf8 | LargeUtf8 | Utf8View => PgColumnType::simple("text", "text"),
        Boolean => PgColumnType::simple("boolean", "bool"),
        Int8 | Int16 | UInt8 => PgColumnType::simple("smallint", "int2"),
        Int32 | UInt16 => PgColumnType::simple("integer", "int4"),
        Int64 | UInt32 => PgColumnType::simple("bigint", "int8"),
        // arrow-pg maps UInt64 to NUMERIC (no unsigned 64-bit pg integer).
        UInt64 => PgColumnType::simple("numeric", "numeric"),
        Float16 | Float32 => PgColumnType::simple("real", "float4"),
        Float64 => PgColumnType::simple("double precision", "float8"),
        Decimal128(p, s) | Decimal256(p, s) => PgColumnType {
            data_type: "numeric",
            udt_name: "numeric".to_string(),
            numeric_precision: Some(*p as u64),
            numeric_scale: Some(*s as u64),
        },
        Date32 | Date64 => PgColumnType::simple("date", "date"),
        Timestamp(_, None) => PgColumnType::simple("timestamp without time zone", "timestamp"),
        Timestamp(_, Some(_)) => PgColumnType::simple("timestamp with time zone", "timestamptz"),
        Time32(_) | Time64(_) => PgColumnType::simple("time without time zone", "time"),
        Binary | LargeBinary | BinaryView | FixedSizeBinary(_) => {
            PgColumnType::simple("bytea", "bytea")
        }
        Interval(_) | Duration(_) => PgColumnType::simple("interval", "interval"),
        // Dictionary encodes another type; report the value type's pg mapping
        // (matches `into_pg_type`, which recurses into the value type).
        Dictionary(_, value) => arrow_to_pg_column_type(value),
        // Arrays: `data_type` is the SQL-standard literal `ARRAY`; `udt_name`
        // is the element type's udt prefixed with `_` (PostgreSQL convention,
        // e.g. `_int4`).
        List(field) | LargeList(field) | FixedSizeList(field, _) => {
            let elem = arrow_to_pg_column_type(field.data_type());
            PgColumnType {
                data_type: "ARRAY",
                udt_name: format!("_{}", elem.udt_name),
                numeric_precision: None,
                numeric_scale: None,
            }
        }
        other => {
            log::debug!(
                "information_schema.columns: no PostgreSQL type mapping for Arrow type {other:?}; \
                 reporting 'text'"
            );
            PgColumnType::simple("text", "text")
        }
    }
}

/// Output schema for the overridden `information_schema.columns` view: the 15
/// columns DataFusion's built-in view exposes, plus a trailing `udt_name`.
fn pg_columns_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("table_catalog", DataType::Utf8, false),
        Field::new("table_schema", DataType::Utf8, false),
        Field::new("table_name", DataType::Utf8, false),
        Field::new("column_name", DataType::Utf8, false),
        Field::new("ordinal_position", DataType::UInt64, false),
        Field::new("column_default", DataType::Utf8, true),
        Field::new("is_nullable", DataType::Utf8, false),
        Field::new("data_type", DataType::Utf8, false),
        Field::new("character_maximum_length", DataType::UInt64, true),
        Field::new("character_octet_length", DataType::UInt64, true),
        Field::new("numeric_precision", DataType::UInt64, true),
        Field::new("numeric_precision_radix", DataType::UInt64, true),
        Field::new("numeric_scale", DataType::UInt64, true),
        Field::new("datetime_precision", DataType::UInt64, true),
        Field::new("interval_type", DataType::Utf8, true),
        Field::new("udt_name", DataType::Utf8, false),
    ]))
}

/// Build a zero-row [`MemTable`] with the given schema.
fn empty_table(schema: Arc<Schema>) -> Arc<dyn TableProvider> {
    Arc::new(MemTable::try_new(schema, vec![Vec::new()]).expect("empty MemTable schema is valid"))
}

/// `information_schema.table_constraints` columns, per the SQL standard /
/// PostgreSQL. All columns are `character_data`/`sql_identifier`/`yes_or_no`,
/// which we surface as nullable `Utf8`.
fn table_constraints_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("constraint_catalog", DataType::Utf8, true),
        Field::new("constraint_schema", DataType::Utf8, true),
        Field::new("constraint_name", DataType::Utf8, true),
        Field::new("table_catalog", DataType::Utf8, true),
        Field::new("table_schema", DataType::Utf8, true),
        Field::new("table_name", DataType::Utf8, true),
        Field::new("constraint_type", DataType::Utf8, true),
        Field::new("is_deferrable", DataType::Utf8, true),
        Field::new("initially_deferred", DataType::Utf8, true),
        Field::new("enforced", DataType::Utf8, true),
    ]))
}

/// `information_schema.key_column_usage` columns. The two positional columns
/// are `cardinal_number` (a positive integer) → nullable `Int32`.
fn key_column_usage_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("constraint_catalog", DataType::Utf8, true),
        Field::new("constraint_schema", DataType::Utf8, true),
        Field::new("constraint_name", DataType::Utf8, true),
        Field::new("table_catalog", DataType::Utf8, true),
        Field::new("table_schema", DataType::Utf8, true),
        Field::new("table_name", DataType::Utf8, true),
        Field::new("column_name", DataType::Utf8, true),
        Field::new("ordinal_position", DataType::Int32, true),
        Field::new("position_in_unique_constraint", DataType::Int32, true),
    ]))
}

/// `information_schema.referential_constraints` columns. All columns are
/// `sql_identifier`/`character_data` → nullable `Utf8`.
fn referential_constraints_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("constraint_catalog", DataType::Utf8, true),
        Field::new("constraint_schema", DataType::Utf8, true),
        Field::new("constraint_name", DataType::Utf8, true),
        Field::new("unique_constraint_catalog", DataType::Utf8, true),
        Field::new("unique_constraint_schema", DataType::Utf8, true),
        Field::new("unique_constraint_name", DataType::Utf8, true),
        Field::new("match_option", DataType::Utf8, true),
        Field::new("update_rule", DataType::Utf8, true),
        Field::new("delete_rule", DataType::Utf8, true),
    ]))
}

/// `current_schema()` — a no-argument scalar UDF returning the session's
/// default schema name as a constant `Utf8`.
#[derive(Debug, PartialEq, Eq, Hash)]
struct CurrentSchemaUdf {
    signature: Signature,
    schema: String,
}

impl CurrentSchemaUdf {
    fn new(schema: String) -> Self {
        Self {
            signature: Signature::nullary(Volatility::Stable),
            schema,
        }
    }
}

impl ScalarUDFImpl for CurrentSchemaUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "current_schema"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> DfResult<DataType> {
        Ok(DataType::Utf8)
    }

    fn invoke_with_args(&self, _args: ScalarFunctionArgs) -> DfResult<ColumnarValue> {
        Ok(ColumnarValue::Scalar(ScalarValue::Utf8(Some(
            self.schema.clone(),
        ))))
    }
}

async fn build_dataset(
    d: &DatasetConfig,
    ctx: &SessionContext,
    storage: Option<&Arc<MaterializationStorage>>,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    // Query-kind datasets: check residency to decide memory vs storage path.
    if d.source.kind == SourceKind::Query {
        return build_query_dataset(d, ctx, storage).await;
    }

    // Lazy datasets: register a streaming provider straight against the
    // source and skip the materialise / index / partition pipeline below.
    // Parquet uses a `ListingTable`; delta uses deltalake's own DataFusion
    // `TableProvider` (which reads the transaction log once for the file
    // list, then streams parquet row groups per query with predicate
    // pushdown + file skipping).
    if d.lazy {
        match (d.source.kind, d.source.is_s3()) {
            (SourceKind::Parquet, false) => return build_lazy_local_parquet(d, ctx).await,
            (SourceKind::Parquet, true) => return build_lazy_s3_parquet(d, ctx).await,
            (SourceKind::Delta, _) => return build_lazy_delta(d, ctx).await,
            (SourceKind::Query, _) => unreachable!("query kind handled above"),
        }
    }

    // Fetch raw RecordBatches from whichever backing store the dataset
    // is configured to use. All four (parquet, delta) x (local, s3)
    // combinations converge into one Vec<RecordBatch>; the materialisation
    // / indexing / partitioning logic below is shared.
    let raw_batches: Vec<RecordBatch> = match (d.source.kind, d.source.is_s3()) {
        (SourceKind::Parquet, false) => read_local_parquet(d)?,
        (SourceKind::Parquet, true) => read_s3_parquet(d, ctx).await?,
        (SourceKind::Delta, false) => read_delta(d, HashMap::new()).await?,
        (SourceKind::Delta, true) => read_delta(d, delta_s3_options(d)?).await?,
        (SourceKind::Query, _) => unreachable!("query kind handled above"),
    };
    if raw_batches.is_empty() {
        return Err(AppError::EmptyDataset(format!(
            "dataset '{}': source produced no batches",
            d.name
        )));
    }
    // A source can also resolve to one or more *zero-row* batches (e.g. an
    // empty Delta table, or a parquet file with only a schema). Treat that as
    // empty too so it's logged and skipped rather than registered as a 0-row
    // dataset that shows up in discovery / explore.
    if raw_batches.iter().all(|b| b.num_rows() == 0) {
        return Err(AppError::EmptyDataset(format!(
            "dataset '{}': source has a schema but no rows",
            d.name
        )));
    }

    materialise_batches(d, raw_batches)
}

// ---------------------------------------------------------------------------
// Phase 2B — query-dataset materialisation with optional storage spill
// ---------------------------------------------------------------------------

/// Determine the effective residency for a query dataset, applying WARN when
/// `auto` degrades to memory because no storage is configured.
fn effective_residency(
    d: &DatasetConfig,
    storage: Option<&Arc<MaterializationStorage>>,
) -> MaterializeResidency {
    let requested = d
        .materialize
        .as_ref()
        .map(|m| m.residency)
        .unwrap_or(MaterializeResidency::Auto);
    match (requested, storage) {
        (MaterializeResidency::Lazy, _) => MaterializeResidency::Lazy,
        (MaterializeResidency::Memory, _) => MaterializeResidency::Memory,
        (MaterializeResidency::Auto, Some(_)) => MaterializeResidency::Auto,
        (MaterializeResidency::Auto, None) => {
            // Degrade gracefully: no storage backend configured.
            MaterializeResidency::Memory
        }
    }
}

// ---------------------------------------------------------------------------
// Materialization context (spill-capable, separate from serving context)
// ---------------------------------------------------------------------------

/// Build a short-lived `SessionContext` for one materialization build.
///
/// # Why a separate context from the serving `Store.ctx`?
///
/// The shared serving context must **never** have a memory bound: adding one
/// would cause `/query` and `/sql` to start rejecting allocations or spilling
/// under normal load, breaking the serving SLA for file-backed datasets.
/// Materialization contexts are ephemeral (one per build, dropped on
/// completion), so bounding their pool is safe and isolated.
///
/// # Memory pool
///
/// Uses [`FairSpillPool`] sized at `pool_bytes`. When a sort or hash-aggregate
/// exceeds that reservation DataFusion signals the operator to spill to the
/// OS temp directory via [`DiskManagerConfig::NewOs`]. This is what makes the
/// R2B.2 spill bound hold for `sort_by` builds.
///
/// # Catalog and object-store sharing
///
/// The new `RuntimeEnv` is built via [`RuntimeEnvBuilder::from_runtime_env`]
/// which clones the serving runtime's `object_store_registry` (Arc clone —
/// no data copied). S3 stores registered for source datasets are therefore
/// visible to materialization queries without re-registration.
///
/// Table providers are copied from the serving context's default catalog/schema
/// by cloning `Arc<dyn TableProvider>` — the underlying buffers are shared,
/// satisfying R4.5 (snapshot-consistent dependency reads).
async fn build_mat_ctx(
    serving_ctx: &SessionContext,
    pool_bytes: usize,
) -> Result<SessionContext, AppError> {
    // Inherit the object_store_registry from the serving runtime so S3
    // source stores are immediately visible. Then override just the pool
    // and disk manager.
    let runtime = RuntimeEnvBuilder::from_runtime_env(serving_ctx.runtime_env().as_ref())
        .with_memory_pool(Arc::new(FairSpillPool::new(pool_bytes)))
        .with_disk_manager_builder(DiskManagerBuilder::default())
        .build_arc()
        .map_err(|e| AppError::Internal(format!("materialization runtime: {e}")))?;

    // Inherit all session options (parquet pushdown, etc.) from the serving
    // context so the query plan behaves identically — with one exception:
    //
    // `sort_spill_reservation_bytes` is scaled to pool_bytes / 4 so that
    // 75% of the pool remains available for actual sort-batch accumulation.
    // DataFusion's default (10 MiB) relative to the 12 MiB floor pool leaves
    // only 2 MiB for data, which is narrower than a single 8192-row batch over
    // a wide schema. Scaling to pool / 4 ensures batches can always be reserved
    // before the sort operator is forced to spill them. (R2B.2)
    let spill_reservation = (pool_bytes / 4).max(512 * 1024); // floor 512 KiB
    let mut mat_config = serving_ctx.copied_config();
    mat_config
        .options_mut()
        .execution
        .sort_spill_reservation_bytes = spill_reservation;

    let mat_ctx = SessionContext::new_with_config_rt(mat_config, runtime);

    // Copy table providers from the serving context's default catalog/schema.
    let state = serving_ctx.state();
    let cfg = state.config();
    let catalog_name = cfg.options().catalog.default_catalog.as_str();
    let schema_name = cfg.options().catalog.default_schema.as_str();

    #[allow(clippy::collapsible_if)]
    if let Some(catalog) = serving_ctx.catalog(catalog_name) {
        if let Some(schema) = catalog.schema(schema_name) {
            for table_name in schema.table_names() {
                if let Ok(Some(provider)) = schema.table(&table_name).await {
                    let _ = mat_ctx.register_table(table_name.as_str(), provider);
                }
            }
        }
    }

    Ok(mat_ctx)
}

/// Negative-control test helper: build a materialization context that is
/// identical to [`build_mat_ctx`] **except** the disk manager is disabled so
/// no spill files can ever be created.
///
/// This is intentionally only compiled for tests. Production code always uses
/// `DiskManagerBuilder::default()` (OS temp spill). The sole purpose of this
/// helper is to pin pool enforcement: a sort that requires more memory than the
/// pool must fail — if it unexpectedly succeeds the pool has been detached.
///
/// Callers should call `session_context()` on the `Store` to obtain the
/// `serving_ctx` argument; table providers are then copied into the no-spill
/// context so the SQL can reference the same source tables.
// Integration tests live in tests/ and compile the library crate without
// cfg(test), so this cannot be gated with #[cfg(test)].  The _for_test suffix
// and doc(hidden) signal that this function is not part of the public API.
#[doc(hidden)]
pub async fn build_mat_ctx_no_spill_for_test(
    serving_ctx: &SessionContext,
    pool_bytes: usize,
) -> Result<SessionContext, AppError> {
    use datafusion::execution::disk_manager::DiskManagerMode;

    let runtime = RuntimeEnvBuilder::from_runtime_env(serving_ctx.runtime_env().as_ref())
        .with_memory_pool(Arc::new(FairSpillPool::new(pool_bytes)))
        .with_disk_manager_builder(
            DiskManagerBuilder::default().with_mode(DiskManagerMode::Disabled),
        )
        .build_arc()
        .map_err(|e| AppError::Internal(format!("no-spill runtime: {e}")))?;

    // Same proportional spill-reservation reduction as build_mat_ctx so the
    // first sort batches can be reserved before the pool is exhausted.
    let spill_reservation = (pool_bytes / 4).max(512 * 1024);
    let mut mat_config = serving_ctx.copied_config();
    mat_config
        .options_mut()
        .execution
        .sort_spill_reservation_bytes = spill_reservation;
    let mat_ctx = SessionContext::new_with_config_rt(mat_config, runtime);

    let state = serving_ctx.state();
    let cfg = state.config();
    let catalog_name = cfg.options().catalog.default_catalog.as_str();
    let schema_name = cfg.options().catalog.default_schema.as_str();

    #[allow(clippy::collapsible_if)]
    if let Some(catalog) = serving_ctx.catalog(catalog_name) {
        if let Some(schema) = catalog.schema(schema_name) {
            for table_name in schema.table_names() {
                if let Ok(Some(provider)) = schema.table(&table_name).await {
                    let _ = mat_ctx.register_table(table_name.as_str(), provider);
                }
            }
        }
    }

    Ok(mat_ctx)
}

/// Pool size for materialization builds.
///
/// Sized to be large enough to:
/// 1. Satisfy DataFusion's `sort_spill_reservation_bytes` (10 MiB default),
///    which the external sort merge phase reserves before it can even start.
/// 2. Hold the in-flight sort buffer up to `force_lazy_above_mb`.
///
/// When `force_lazy_above_mb > 16`, we use that value so the sort for a
/// result at the demotion threshold fits in memory before spilling. When it
/// is smaller (or 0), we floor at 32 MiB so the sort reservation overhead is
/// always satisfiable.
///
/// When no storage is configured (sort stays in memory), we default to 512 MiB.
///
/// The serving context's pool is always [`UnboundedMemoryPool`]; only the
/// ephemeral materialization context uses this bounded pool.
fn materialization_pool_bytes(storage: Option<&Arc<MaterializationStorage>>) -> usize {
    /// DataFusion's default `sort_spill_reservation_bytes` — the minimum pool
    /// reservation the external sort merge phase requires to initialise.
    const SORT_SPILL_MIN_BYTES: usize = 12 * 1024 * 1024; // 12 MiB (10 MiB + 20% margin)

    storage
        .map(|s| {
            let mb = s.config.force_lazy_above_mb;
            let from_threshold = (mb as usize).saturating_mul(1024 * 1024);
            // Always at least SORT_SPILL_MIN_BYTES so the external sort can
            // initialise its merge reservation even when the threshold is tiny.
            from_threshold.max(SORT_SPILL_MIN_BYTES)
        })
        .unwrap_or(512 * 1024 * 1024)
}

/// Build a query-kind dataset with the correct residency policy (R2B.1-R2B.6).
async fn build_query_dataset(
    d: &DatasetConfig,
    ctx: &SessionContext,
    storage: Option<&Arc<MaterializationStorage>>,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    let sql = d.source.sql.as_deref().ok_or_else(|| {
        AppError::Internal(format!(
            "dataset '{}': source.sql missing for kind = query",
            d.name
        ))
    })?;

    let residency = effective_residency(d, storage);

    // Try reuse_on_start first if requested (R2B.6). Uses the serving context
    // for the stored generation ListingTable — no execution, no pool needed.
    #[allow(clippy::collapsible_if)]
    if d.materialize.as_ref().is_some_and(|m| m.reuse_on_start) {
        if let Some(stor) = storage {
            if let Some(reused) = try_reuse_generation(d, sql, stor, ctx).await {
                return Ok(reused);
            }
        }
    }

    // Build a separate materialization context with a bounded FairSpillPool
    // so ORDER BY (sort_by, R2B.5) and large aggregations can spill to disk
    // instead of growing unboundedly. The serving context (ctx) is unchanged.
    let pool_bytes = materialization_pool_bytes(storage);
    let mat_ctx = build_mat_ctx(ctx, pool_bytes).await?;

    match residency {
        MaterializeResidency::Memory => {
            // Classic in-memory path. Apply sort_by ORDER BY (R2B.5).
            let sort_by: Vec<String> = d
                .materialize
                .as_ref()
                .map(|m| m.sort_by.clone())
                .unwrap_or_default();
            let effective_sql = apply_sort_by(sql, &sort_by);
            let df = mat_ctx
                .sql(&effective_sql)
                .await
                .map_err(|e| AppError::Internal(format!("dataset '{}': SQL plan: {e}", d.name)))?;
            let raw_batches = df.collect().await.map_err(|e| {
                AppError::Internal(format!("dataset '{}': SQL execute: {e}", d.name))
            })?;
            if raw_batches.is_empty() || raw_batches.iter().all(|b| b.num_rows() == 0) {
                return Err(AppError::EmptyDataset(format!(
                    "dataset '{}': query source produced no rows",
                    d.name
                )));
            }
            materialise_batches(d, raw_batches)
        }
        MaterializeResidency::Lazy => {
            // Always write to storage; serve from ListingTable.
            let stor = storage.ok_or_else(|| {
                AppError::Internal(format!(
                    "dataset '{}': residency = lazy requires [server.storage]",
                    d.name
                ))
            })?;
            build_query_to_storage(d, sql, &mat_ctx, stor).await
        }
        MaterializeResidency::Auto => {
            // Streaming spill: buffer until threshold, then demote to storage.
            let stor = storage.ok_or_else(|| {
                // Should not happen: effective_residency already degraded to Memory.
                AppError::Internal(format!(
                    "dataset '{}': internal: auto residency without storage",
                    d.name
                ))
            })?;
            build_query_auto(d, sql, &mat_ctx, stor).await
        }
    }
}

/// Wrap `sql` with `ORDER BY` when `sort_by` columns are configured (R2B.5).
///
/// The result is `SELECT * FROM (<sql>) AS _m ORDER BY "<col1>", ...` which
/// DataFusion executes with its standard sorted execution plan. Memory is
/// bounded by the runtime's configured memory pool; DataFusion spills to the
/// configured temp directory when the pool is exhausted.
fn apply_sort_by(sql: &str, sort_by: &[String]) -> String {
    if sort_by.is_empty() {
        return sql.to_string();
    }
    let order_cols = sort_by
        .iter()
        .map(|c| DatasetSchema::quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    format!("SELECT * FROM ({sql}) AS _m ORDER BY {order_cols}")
}

/// Streaming spill for `residency = lazy`: write all batches to parquet on the
/// storage backend from the first batch (no buffering).
async fn build_query_to_storage(
    d: &DatasetConfig,
    sql: &str,
    ctx: &SessionContext,
    stor: &Arc<MaterializationStorage>,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    use datafusion::execution::SendableRecordBatchStream;
    use futures_util::StreamExt;

    // Apply ORDER BY wrapping for sort_by (R2B.5).
    let sort_by: Vec<String> = d
        .materialize
        .as_ref()
        .map(|m| m.sort_by.clone())
        .unwrap_or_default();
    let effective_sql = apply_sort_by(sql, &sort_by);
    let sql_ref = effective_sql.as_str();

    let df = ctx
        .sql(sql_ref)
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': SQL plan: {e}", d.name)))?;
    let mut stream: SendableRecordBatchStream = df.execute_stream().await.map_err(|e| {
        AppError::Internal(format!("dataset '{}': SQL execute stream: {e}", d.name))
    })?;

    // Peek the first batch for the schema.
    let first_batch = match stream.next().await {
        Some(Ok(b)) if b.num_rows() > 0 => b,
        Some(Ok(_)) => {
            // zero-row batch: collect more
            let mut found: Option<RecordBatch> = None;
            while let Some(next) = stream.next().await {
                let b = next.map_err(|e| {
                    AppError::Internal(format!("dataset '{}': stream error: {e}", d.name))
                })?;
                if b.num_rows() > 0 {
                    found = Some(b);
                    break;
                }
            }
            found.ok_or_else(|| {
                AppError::EmptyDataset(format!(
                    "dataset '{}': query source produced no rows",
                    d.name
                ))
            })?
        }
        Some(Err(e)) => {
            return Err(AppError::Internal(format!(
                "dataset '{}': stream error: {e}",
                d.name
            )));
        }
        None => {
            return Err(AppError::EmptyDataset(format!(
                "dataset '{}': query source produced no rows",
                d.name
            )));
        }
    };

    let arrow_schema = first_batch.schema();
    let gen_id = new_ulid();

    // Write to storage, streaming batch-by-batch.
    let (files, row_count, byte_size) =
        write_batches_to_storage(d, &gen_id, first_batch, &mut stream, stor, &sort_by).await?;

    // Write manifest (atomicity seal). Use the original sql for the hash so
    // reuse_on_start matches even if sort_by changes the effective_sql wrapper.
    let sql_hash = fnv1a_hash(sql);
    let schema_hash = fnv1a_hash(&schema_fingerprint(&arrow_schema));
    let manifest = GenerationManifest {
        sql_hash,
        schema_hash,
        row_count: row_count as u64,
        byte_size: byte_size as u64,
        created_at: now_rfc3339(),
        files: files.clone(),
    };
    write_manifest_to_storage(d, &gen_id, &manifest, stor).await?;

    // GC old generations.
    gc_storage_generations(d, &gen_id, stor);

    // Build ListingTable over the new generation.
    build_listing_state_from_storage(d, &gen_id, arrow_schema, stor, ctx).await
}

/// Auto residency: buffer up to threshold in memory, then spill to storage.
async fn build_query_auto(
    d: &DatasetConfig,
    sql: &str,
    ctx: &SessionContext,
    stor: &Arc<MaterializationStorage>,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    use datafusion::execution::SendableRecordBatchStream;
    use futures_util::StreamExt;

    let threshold_bytes = stor.config.force_lazy_above_mb.saturating_mul(1024 * 1024) as usize;

    // Apply sort_by ORDER BY wrapper (R2B.5); effective even when result stays in memory.
    let sort_by: Vec<String> = d
        .materialize
        .as_ref()
        .map(|m| m.sort_by.clone())
        .unwrap_or_default();
    let effective_sql = apply_sort_by(sql, &sort_by);

    let df = ctx
        .sql(&effective_sql)
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': SQL plan: {e}", d.name)))?;
    let mut stream: SendableRecordBatchStream = df.execute_stream().await.map_err(|e| {
        AppError::Internal(format!("dataset '{}': SQL execute stream: {e}", d.name))
    })?;

    let mut buffer: Vec<RecordBatch> = Vec::new();
    let mut buffered_bytes: usize = 0;
    let mut demoted = false;
    let mut spill_first_batch: Option<RecordBatch> = None;

    while let Some(result) = stream.next().await {
        let batch = result
            .map_err(|e| AppError::Internal(format!("dataset '{}': stream error: {e}", d.name)))?;
        if batch.num_rows() == 0 {
            continue;
        }
        let batch_bytes: usize = batch
            .columns()
            .iter()
            .map(|c| c.get_buffer_memory_size())
            .sum();
        buffered_bytes += batch_bytes;
        buffer.push(batch);

        if buffered_bytes > threshold_bytes {
            // Demote: take the first batch as the "already seen" batch for streaming.
            demoted = true;
            spill_first_batch = Some(buffer.remove(0));
            break;
        }
    }

    if !demoted {
        // Stayed in memory.
        if buffer.is_empty() || buffer.iter().all(|b| b.num_rows() == 0) {
            return Err(AppError::EmptyDataset(format!(
                "dataset '{}': query source produced no rows",
                d.name
            )));
        }
        // Check if explicit memory override is set but we're over threshold.
        if d.materialize
            .as_ref()
            .is_some_and(|mc| mc.residency == MaterializeResidency::Memory)
            && buffered_bytes > threshold_bytes
        {
            log::warn!(
                "dataset '{}': materialized result ({} MiB) exceeds force_lazy_above_mb \
                     = {} but residency = memory overrides demotion",
                d.name,
                buffered_bytes / (1024 * 1024),
                stor.config.force_lazy_above_mb,
            );
        }
        return materialise_batches(d, buffer);
    }

    // Demoted: write buffer + remaining stream to storage.
    log::info!(
        "dataset '{}': auto-demoting to storage ({} MiB exceeded {} MiB threshold)",
        d.name,
        buffered_bytes / (1024 * 1024),
        stor.config.force_lazy_above_mb,
    );

    let first = spill_first_batch.unwrap();
    let arrow_schema = first.schema();
    let gen_id = new_ulid();

    // We have `buffer` (already accumulated) + `first` (just spilled) + remaining `stream`.
    let (mut files, mut row_count, mut byte_size) =
        write_buffer_to_storage(d, &gen_id, &buffer, stor).await?;
    let (more_files, more_rows, more_bytes) =
        write_batches_to_storage(d, &gen_id, first, &mut stream, stor, &sort_by).await?;
    files.extend(more_files);
    row_count += more_rows;
    byte_size += more_bytes;

    let sql_hash = fnv1a_hash(sql);
    let schema_hash = fnv1a_hash(&schema_fingerprint(&arrow_schema));
    let manifest = GenerationManifest {
        sql_hash,
        schema_hash,
        row_count: row_count as u64,
        byte_size: byte_size as u64,
        created_at: now_rfc3339(),
        files,
    };
    write_manifest_to_storage(d, &gen_id, &manifest, stor).await?;
    gc_storage_generations(d, &gen_id, stor);

    build_listing_state_from_storage(d, &gen_id, arrow_schema, stor, ctx).await
}

/// Write buffered batches (the pre-demote accumulation) to storage as file 0.
async fn write_buffer_to_storage(
    d: &DatasetConfig,
    gen_id: &str,
    buffer: &[RecordBatch],
    stor: &Arc<MaterializationStorage>,
) -> Result<(Vec<String>, usize, usize), AppError> {
    if buffer.is_empty() {
        return Ok((Vec::new(), 0, 0));
    }
    let file_name = "data-0.parquet";
    let obj_path = obj_store_path(stor, &d.name, gen_id, file_name);
    let schema = buffer[0].schema();
    let props = WriterProperties::builder().build();
    let obj_writer = ParquetObjectWriter::new(stor.object_store.clone(), obj_path);
    let mut writer = AsyncArrowWriter::try_new(obj_writer, schema, Some(props)).map_err(|e| {
        AppError::Internal(format!("dataset '{}': parquet writer init: {e}", d.name))
    })?;

    let mut rows = 0usize;
    for batch in buffer {
        rows += batch.num_rows();
        writer
            .write(batch)
            .await
            .map_err(|e| AppError::Internal(format!("dataset '{}': parquet write: {e}", d.name)))?;
    }
    let file_meta = writer
        .close()
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': parquet close: {e}", d.name)))?;
    // Approximate byte size from row group metadata.
    let bytes: usize = file_meta
        .row_groups()
        .iter()
        .map(|rg| rg.compressed_size() as usize)
        .sum();
    Ok((vec![file_name.to_string()], rows, bytes))
}

/// Write first_batch + remaining stream to storage, one file per call.
/// `sort_by` is reserved for ORDER BY at write time (R2B.5); not yet applied
/// since we are writing streaming (can't sort without collecting).
async fn write_batches_to_storage(
    d: &DatasetConfig,
    gen_id: &str,
    first_batch: RecordBatch,
    stream: &mut datafusion::execution::SendableRecordBatchStream,
    stor: &Arc<MaterializationStorage>,
    sort_by: &[String],
) -> Result<(Vec<String>, usize, usize), AppError> {
    use futures_util::StreamExt;
    let file_name = "data-main.parquet";
    let obj_path = obj_store_path(stor, &d.name, gen_id, file_name);
    let schema = first_batch.schema();
    // When sort_by is set, use a smaller row-group size so row-group min/max
    // statistics partition the sort key into ranges that DataFusion can prune
    // at query time. Without this, a single 1M-row default group spans the
    // whole dataset and pruning is never triggered. (R2B.5 MUST)
    let props = if sort_by.is_empty() {
        WriterProperties::builder().build()
    } else {
        WriterProperties::builder()
            .set_max_row_group_row_count(Some(MAT_SORTED_ROW_GROUP_SIZE))
            .build()
    };
    let obj_writer = ParquetObjectWriter::new(stor.object_store.clone(), obj_path);
    let mut writer = AsyncArrowWriter::try_new(obj_writer, schema, Some(props)).map_err(|e| {
        AppError::Internal(format!("dataset '{}': parquet writer init: {e}", d.name))
    })?;

    let mut rows = first_batch.num_rows();
    writer
        .write(&first_batch)
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': parquet write: {e}", d.name)))?;
    while let Some(result) = stream.next().await {
        let batch = result
            .map_err(|e| AppError::Internal(format!("dataset '{}': stream error: {e}", d.name)))?;
        if batch.num_rows() == 0 {
            continue;
        }
        rows += batch.num_rows();
        writer
            .write(&batch)
            .await
            .map_err(|e| AppError::Internal(format!("dataset '{}': parquet write: {e}", d.name)))?;
    }
    let file_meta = writer
        .close()
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': parquet close: {e}", d.name)))?;
    let bytes: usize = file_meta
        .row_groups()
        .iter()
        .map(|rg| rg.compressed_size() as usize)
        .sum();
    Ok((vec![file_name.to_string()], rows, bytes))
}

/// Build the object store path for a file within a generation.
/// Retained as a thin wrapper since call sites use the free function.
fn obj_store_path(
    stor: &MaterializationStorage,
    dataset: &str,
    gen_id: &str,
    file_name: &str,
) -> ObjPath {
    stor.obj_path(dataset, gen_id, file_name)
}

/// Write manifest.json to the storage backend.
async fn write_manifest_to_storage(
    d: &DatasetConfig,
    gen_id: &str,
    manifest: &GenerationManifest,
    stor: &Arc<MaterializationStorage>,
) -> Result<(), AppError> {
    if let Some(ref local_root) = stor.local_root {
        // Local: use stdlib fs (cheaper, avoids async overhead).
        let gen_dir = datapress_core::storage::generation_dir(local_root, &d.name, gen_id);
        std::fs::create_dir_all(&gen_dir).ok();
        manifest
            .write(&gen_dir)
            .map_err(|e| AppError::Internal(format!("dataset '{}': write manifest: {e}", d.name)))
    } else {
        // S3: write via object_store.
        let json = serde_json::to_vec_pretty(manifest).map_err(|e| {
            AppError::Internal(format!("dataset '{}': manifest serialize: {e}", d.name))
        })?;
        let path = obj_store_path(stor, &d.name, gen_id, "manifest.json");
        stor.object_store
            .put(&path, object_store::PutPayload::from(json))
            .await
            .map_err(|e| {
                AppError::Internal(format!("dataset '{}': write manifest to S3: {e}", d.name))
            })?;
        Ok(())
    }
}

/// GC old storage generations, keeping current + previous (N-2 rule).
fn gc_storage_generations(
    d: &DatasetConfig,
    current_gen_id: &str,
    stor: &Arc<MaterializationStorage>,
) {
    if let Some(ref local_root) = stor.local_root {
        let gens = list_complete_generations(local_root, &d.name);
        // Sort is chronological; keep last two including current.
        let keep: Vec<&str> = gens
            .iter()
            .rev()
            .take(2)
            .map(|(id, _, _)| id.as_str())
            .collect();
        // Always include current (it may not be in gens yet if manifest not written).
        let mut keep_ids: Vec<&str> = keep;
        if !keep_ids.contains(&current_gen_id) {
            keep_ids.push(current_gen_id);
        }
        gc_generations(local_root, &d.name, &keep_ids);
    }
    // For S3 backend, GC is logged but not performed in this phase
    // (S3 listing + delete is more complex; deferred to Phase 2B follow-up).
}

/// Boot GC: scan all dataset directories and remove incomplete (manifest-less)
/// generation directories and generations older than N-2.
fn boot_gc_storage(stor: &Arc<MaterializationStorage>, datasets: &[DatasetConfig]) {
    let local_root = match &stor.local_root {
        Some(r) => r,
        None => return, // S3 GC deferred
    };
    for d in datasets {
        if d.source.kind != SourceKind::Query {
            continue;
        }
        let gens = list_complete_generations(local_root, &d.name);
        // Keep the two most recent complete generations.
        let keep_ids: Vec<&str> = gens
            .iter()
            .rev()
            .take(2)
            .map(|(id, _, _)| id.as_str())
            .collect();
        gc_generations(local_root, &d.name, &keep_ids);
    }
}

/// Build a DatasetState + ListingTable provider over the written generation.
async fn build_listing_state_from_storage(
    d: &DatasetConfig,
    gen_id: &str,
    arrow_schema: Arc<Schema>,
    stor: &Arc<MaterializationStorage>,
    ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    // Register the object store on the context so ListingTable can use it.
    let listing_url = if let Some(ref local_root) = stor.local_root {
        let gen_dir = datapress_core::storage::generation_dir(local_root, &d.name, gen_id);
        let url_str = format!("file://{}", gen_dir.display());
        ListingTableUrl::parse(&url_str).map_err(|e| {
            AppError::Internal(format!("dataset '{}': listing URL parse: {e}", d.name))
        })?
    } else {
        let url_str = format!("{}/{}/{gen_id}/", stor.config.root, d.name);
        ListingTableUrl::parse(&url_str).map_err(|e| {
            AppError::Internal(format!("dataset '{}': listing URL parse: {e}", d.name))
        })?
    };

    let listing_opts =
        ListingOptions::new(Arc::new(ParquetFormat::default())).with_file_extension(".parquet");
    let session_state = ctx.state();
    let file_schema = listing_opts
        .infer_schema(&session_state, &listing_url)
        .await
        .unwrap_or_else(|_| arrow_schema.clone());

    let listing_cfg = ListingTableConfig::new(listing_url)
        .with_listing_options(listing_opts)
        .with_schema(file_schema.clone());
    let provider: Arc<dyn TableProvider> = Arc::new(
        ListingTable::try_new(listing_cfg)
            .map_err(|e| AppError::Internal(format!("dataset '{}': ListingTable: {e}", d.name)))?,
    );

    let columns: Vec<ColumnInfo> = file_schema
        .fields()
        .iter()
        .map(|f| {
            let dt = f.data_type();
            ColumnInfo {
                name: f.name().clone(),
                logical: arrow_to_logical(dt),
                sql_type: format!("{dt:?}"),
                nullable: f.is_nullable(),
            }
        })
        .collect();
    let schema = DatasetSchema::new(&d.name, columns)
        .with_filters(d.predicate_filter.clone(), d.projection_filter.clone())?;

    // Skip index for lazy/storage generations (R2B.5).
    if d.index.mode != IndexMode::Auto {
        log::warn!(
            "dataset '{}': skipping eq-index for storage-backed (lazy) generation",
            d.name
        );
    }

    log::info!(
        "dataset '{}' [query, lazy/storage]: {} cols, generation {}",
        d.name,
        schema.columns.len(),
        gen_id
    );

    Ok((
        DatasetState {
            schema,
            data: vec![],
            arrow_schema: file_schema,
            index: Default::default(),
            lazy: true,
        },
        provider,
    ))
}

/// Try to reuse the newest complete generation on storage if sql+schema hashes match.
/// Returns `Some(state, provider)` on a cache hit, `None` to proceed with a normal build.
async fn try_reuse_generation(
    d: &DatasetConfig,
    sql: &str,
    stor: &Arc<MaterializationStorage>,
    ctx: &SessionContext,
) -> Option<(DatasetState, Arc<dyn TableProvider>)> {
    let local_root = stor.local_root.as_ref()?;
    let gens = list_complete_generations(local_root, &d.name);
    let (gen_id, manifest, _gen_dir) = gens.into_iter().last()?;

    let sql_hash = fnv1a_hash(sql);
    // We can't compute schema_hash without running the query, so compare
    // only the sql hash for the reuse check.
    if manifest.sql_hash != sql_hash {
        log::debug!(
            "dataset '{}': reuse_on_start: sql hash mismatch — rebuilding",
            d.name
        );
        return None;
    }

    log::info!(
        "dataset '{}': reuse_on_start: reusing generation {} ({} rows, {} bytes)",
        d.name,
        gen_id,
        manifest.row_count,
        manifest.byte_size,
    );

    // Infer schema from the stored parquet.
    let arrow_schema = Arc::new(Schema::empty()); // placeholder; inferred by listing
    match build_listing_state_from_storage(d, &gen_id, arrow_schema, stor, ctx).await {
        Ok(result) => Some(result),
        Err(e) => {
            log::warn!(
                "dataset '{}': reuse_on_start: failed to open stored generation: {e}",
                d.name
            );
            None
        }
    }
}

/// Compute a simple schema fingerprint string for hashing.
fn schema_fingerprint(schema: &Arc<Schema>) -> String {
    schema
        .fields()
        .iter()
        .map(|f| format!("{}:{:?}", f.name(), f.data_type()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Convert a `Vec<RecordBatch>` into a materialised [`DatasetState`] +
/// [`MemTable`] provider. Shared by the file-backed and query-kind paths.
fn materialise_batches(
    d: &DatasetConfig,
    chunks: Vec<RecordBatch>,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    let arrow_sch = chunks[0].schema();

    // Build DatasetSchema from the Arrow schema.
    let columns: Vec<ColumnInfo> = arrow_sch
        .fields()
        .iter()
        .map(|f| {
            let dt = f.data_type();
            ColumnInfo {
                name: f.name().clone(),
                logical: arrow_to_logical(dt),
                sql_type: format!("{dt:?}"),
                nullable: f.is_nullable(),
            }
        })
        .collect();
    let schema = DatasetSchema::new(&d.name, columns)
        .with_filters(d.predicate_filter.clone(), d.projection_filter.clone())?;

    // Build the equality index per the per-dataset policy. Operates on the
    // chunked representation directly so we never have to materialise a
    // single concatenated batch (which would double peak RSS on wide
    // schemas — see `DatasetState` docs).
    let index = build_eq_index_with_policy(&chunks, &d.index);

    // Partition for parallel scans by the SQL fallback path. We distribute
    // the existing batches round-robin across `n_parts` partitions instead
    // of re-slicing a concatenated batch — `clone()` on a RecordBatch is
    // an Arc-clone of the column buffers, not a copy.
    let n_parts = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut parts: Vec<Vec<RecordBatch>> = (0..n_parts).map(|_| Vec::new()).collect();
    for (i, b) in chunks.iter().enumerate() {
        if b.num_rows() == 0 {
            continue;
        }
        parts[i % n_parts].push(b.clone());
    }
    parts.retain(|p| !p.is_empty());
    let provider: Arc<dyn TableProvider> = Arc::new(MemTable::try_new(arrow_sch.clone(), parts)?);

    let total_rows: usize = chunks.iter().map(|b| b.num_rows()).sum();
    let mem_mb: usize = chunks
        .iter()
        .flat_map(|b| b.columns().iter())
        .map(|c| c.get_buffer_memory_size())
        .sum::<usize>()
        / 1_048_576;
    log::info!(
        "dataset '{}' [{}]: {} rows, {} cols, {} MB, {} chunks, {} indexed cols",
        d.name,
        d.source.kind.as_str(),
        total_rows,
        schema.columns.len(),
        mem_mb,
        chunks.len(),
        index.len()
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
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    let (url, part_keys) = lazy_local_listing(d)?;

    let mut opts =
        ListingOptions::new(Arc::new(ParquetFormat::default())).with_file_extension(".parquet");
    if !part_keys.is_empty() {
        opts = opts.with_table_partition_cols(
            part_keys
                .iter()
                .map(|k| (k.clone(), DataType::Utf8))
                .collect(),
        );
    }

    let session_state = ctx.state();
    // `infer_schema` returns the *file* schema (without partition columns);
    // `ListingTable` appends the declared partition columns on top.
    let file_schema = opts.infer_schema(&session_state, &url).await.map_err(|e| {
        AppError::Internal(format!("dataset '{}': infer parquet schema: {e}", d.name))
    })?;

    // An empty listing yields an empty schema (DataFusion does not error on a
    // prefix/glob that matches no files). Treat that as an empty dataset so
    // the load loop logs and skips it rather than registering 0 columns.
    if file_schema.fields().is_empty() {
        return Err(AppError::EmptyDataset(format!(
            "dataset '{}': no .parquet files at {}",
            d.name, d.source.location
        )));
    }

    let cfg = ListingTableConfig::new(url)
        .with_listing_options(opts)
        .with_schema(file_schema.clone());
    let table = ListingTable::try_new(cfg).map_err(|e| {
        AppError::Internal(format!("dataset '{}': ListingTable::try_new: {e}", d.name))
    })?;
    let provider: Arc<dyn TableProvider> = Arc::new(table);

    // Discovery schema = file columns + partition columns (Utf8).
    let mut fields: Vec<Field> = file_schema
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    for k in &part_keys {
        if !fields.iter().any(|f| f.name() == k) {
            fields.push(Field::new(k, DataType::Utf8, false));
        }
    }
    let arrow_sch = Arc::new(Schema::new(fields));

    let columns: Vec<ColumnInfo> = arrow_sch
        .fields()
        .iter()
        .map(|f| {
            let dt = f.data_type();
            ColumnInfo {
                name: f.name().clone(),
                logical: arrow_to_logical(dt),
                sql_type: format!("{dt:?}"),
                nullable: f.is_nullable(),
            }
        })
        .collect();
    let schema = DatasetSchema::new(&d.name, columns)
        .with_filters(d.predicate_filter.clone(), d.projection_filter.clone())?;

    log::info!(
        "dataset '{}' [{}, lazy]: {} cols ({} partition), no materialise, no index",
        d.name,
        d.source.kind.as_str(),
        schema.columns.len(),
        part_keys.len()
    );

    Ok((
        DatasetState {
            schema,
            data: Vec::new(),
            arrow_schema: arrow_sch,
            index: EqIndex::default(),
            lazy: true,
        },
        provider,
    ))
}

/// Resolve a local lazy-parquet location into a single `ListingTableUrl`
/// rooted at the dataset base plus the ordered hive partition keys (if any).
/// Handles three shapes: a glob (`root/city=*/*.parquet`), a directory
/// (hive root or flat folder of parquets), and a single `*.parquet` file.
fn lazy_local_listing(d: &DatasetConfig) -> Result<(ListingTableUrl, Vec<String>), AppError> {
    let loc = &d.source.location;

    if loc.contains('*') || loc.contains('?') || loc.contains('[') {
        let parts: Vec<&str> = loc.split('/').collect();
        let first_wild = parts
            .iter()
            .position(|c| c.contains('*') || c.contains('?') || c.contains('['))
            .unwrap_or(parts.len());
        let base = parts[..first_wild].join("/");
        let base = if base.is_empty() {
            "/".to_string()
        } else {
            base
        };
        // Partition keys: `key=…` components between the base and the file
        // pattern (the final component).
        let upper = parts.len().saturating_sub(1);
        let keys: Vec<String> = parts[first_wild.min(upper)..upper]
            .iter()
            .filter_map(|c| c.split_once('=').map(|(k, _)| k.to_string()))
            .filter(|k| !k.is_empty())
            .collect();
        return Ok((dir_url(std::path::Path::new(&base), d)?, keys));
    }

    let path = std::path::Path::new(loc);
    if path.is_dir() {
        let keys = discover_hive_keys(path);
        return Ok((dir_url(path, d)?, keys));
    }

    let url = ListingTableUrl::parse(loc)
        .map_err(|e| AppError::Internal(format!("dataset '{}': bad url '{loc}': {e}", d.name)))?;
    Ok((url, Vec::new()))
}

/// Parse a directory path into a `ListingTableUrl` (trailing slash so
/// DataFusion treats it as a directory root, not a single object).
fn dir_url(path: &std::path::Path, d: &DatasetConfig) -> Result<ListingTableUrl, AppError> {
    let s = path.to_str().ok_or_else(|| {
        AppError::Internal(format!(
            "dataset '{}': non-utf8 path {}",
            d.name,
            path.display()
        ))
    })?;
    let s = if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    };
    ListingTableUrl::parse(&s)
        .map_err(|e| AppError::Internal(format!("dataset '{}': bad url '{s}': {e}", d.name)))
}

/// Walk down a directory following the first `key=value` subdirectory at
/// each level to discover the ordered hive partition keys. Returns an empty
/// vec for a flat (non-partitioned) folder.
fn discover_hive_keys(base: &std::path::Path) -> Vec<String> {
    let mut keys = Vec::new();
    let mut cur = base.to_path_buf();
    loop {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            break;
        };
        let mut next: Option<(String, std::path::PathBuf)> = None;
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some((k, v)) = name.split_once('=')
                && !k.is_empty()
                && !v.is_empty()
            {
                next = Some((k.to_string(), p));
                break;
            }
        }
        match next {
            Some((k, p)) => {
                keys.push(k);
                cur = p;
            }
            None => break,
        }
    }
    keys
}

/// Lazy S3 parquet: register the dataset's S3 object store on `ctx`, then
/// build a `ListingTable` rooted at the `s3://bucket/prefix/` location with
/// any discovered hive partition columns. DataFusion does the directory
/// listing through the registered store and streams row groups on each
/// query — no local enumeration needed.
async fn build_lazy_s3_parquet(
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    register_s3_object_store(d, ctx)?;

    let (provider, file_schema, part_keys) = build_s3_listing_table(d, ctx).await?;

    // An empty S3 prefix yields an empty inferred schema (no error). Treat it
    // as an empty dataset so the load loop logs and skips it.
    if file_schema.fields().is_empty() {
        return Err(AppError::EmptyDataset(format!(
            "dataset '{}': no .parquet files at {}",
            d.name, d.source.location
        )));
    }

    // Discovery schema = file columns + partition columns (Utf8).
    let mut fields: Vec<Field> = file_schema
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    for k in &part_keys {
        if !fields.iter().any(|f| f.name() == k) {
            fields.push(Field::new(k, DataType::Utf8, false));
        }
    }
    let arrow_sch = Arc::new(Schema::new(fields));

    let columns: Vec<ColumnInfo> = arrow_sch
        .fields()
        .iter()
        .map(|f| {
            let dt = f.data_type();
            ColumnInfo {
                name: f.name().clone(),
                logical: arrow_to_logical(dt),
                sql_type: format!("{dt:?}"),
                nullable: f.is_nullable(),
            }
        })
        .collect();
    let schema = DatasetSchema::new(&d.name, columns)
        .with_filters(d.predicate_filter.clone(), d.projection_filter.clone())?;

    log::info!(
        "dataset '{}' [{}, lazy, s3]: {} cols ({} partition, no materialise, no index)",
        d.name,
        d.source.kind.as_str(),
        schema.columns.len(),
        part_keys.len()
    );

    Ok((
        DatasetState {
            schema,
            data: Vec::new(),
            arrow_schema: arrow_sch,
            index: EqIndex::default(),
            lazy: true,
        },
        provider,
    ))
}

/// Build a `ListingTable` provider for an S3 parquet source, resolving the
/// base prefix and hive partition keys via [`s3_listing`]. The registered
/// S3 object store must already be present on `ctx`. Returns the provider,
/// the inferred *file* schema (without partition columns), and the ordered
/// partition keys.
async fn build_s3_listing_table(
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(Arc<dyn TableProvider>, Arc<Schema>, Vec<String>), AppError> {
    let (url, part_keys) = s3_listing(d, ctx).await?;

    let mut opts =
        ListingOptions::new(Arc::new(ParquetFormat::default())).with_file_extension(".parquet");
    if !part_keys.is_empty() {
        opts = opts.with_table_partition_cols(
            part_keys
                .iter()
                .map(|k| (k.clone(), DataType::Utf8))
                .collect(),
        );
    }

    let session_state = ctx.state();
    let file_schema = opts.infer_schema(&session_state, &url).await.map_err(|e| {
        AppError::Internal(format!(
            "dataset '{}': infer parquet schema on s3: {e}",
            d.name
        ))
    })?;

    let cfg = ListingTableConfig::new(url)
        .with_listing_options(opts)
        .with_schema(file_schema.clone());
    let table = ListingTable::try_new(cfg).map_err(|e| {
        AppError::Internal(format!(
            "dataset '{}': ListingTable::try_new (s3): {e}",
            d.name
        ))
    })?;
    Ok((Arc::new(table), file_schema, part_keys))
}

/// Resolve an S3 parquet source into a `(base ListingTableUrl, partition
/// keys)` pair. Hive keys come from the location glob when present
/// (`s3://bucket/events/year=*/...`), otherwise — for a plain prefix — they
/// are discovered by listing the registered object store. Honours the
/// dataset's `partitioning` mode (`auto` / `hive` / `none`).
async fn s3_listing(
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<(ListingTableUrl, Vec<String>), AppError> {
    let s3 = d.s3.clone().unwrap_or_default();
    let want_partitions = !matches!(s3.partitioning, Partitioning::None);
    let loc = &d.source.location;

    if d.source.has_glob() {
        let (base, keys) = split_glob_base_keys(loc);
        let base = format!("{}/", base.trim_end_matches('/'));
        let url = ListingTableUrl::parse(&base).map_err(|e| {
            AppError::Internal(format!("dataset '{}': bad s3 url '{base}': {e}", d.name))
        })?;
        let keys = if want_partitions { keys } else { Vec::new() };
        return Ok((url, keys));
    }

    let base = if loc.ends_with('/') {
        loc.clone()
    } else {
        format!("{loc}/")
    };
    let url = ListingTableUrl::parse(&base).map_err(|e| {
        AppError::Internal(format!("dataset '{}': bad s3 url '{base}': {e}", d.name))
    })?;
    let keys = if want_partitions {
        discover_s3_hive_keys(ctx, &url).await
    } else {
        Vec::new()
    };
    Ok((url, keys))
}

/// Split a glob location into its non-wildcard base path and the ordered
/// hive partition keys (`key=…` directory components between the base and
/// the final file pattern). Works for both local and `s3://` paths.
fn split_glob_base_keys(loc: &str) -> (String, Vec<String>) {
    let parts: Vec<&str> = loc.split('/').collect();
    let first_wild = parts
        .iter()
        .position(|c| c.contains('*') || c.contains('?') || c.contains('['))
        .unwrap_or(parts.len());
    let base = parts[..first_wild].join("/");
    let base = if base.is_empty() {
        "/".to_string()
    } else {
        base
    };
    let upper = parts.len().saturating_sub(1);
    let keys: Vec<String> = parts[first_wild.min(upper)..upper]
        .iter()
        .filter_map(|c| c.split_once('=').map(|(k, _)| k.to_string()))
        .filter(|k| !k.is_empty())
        .collect();
    (base, keys)
}

/// Discover ordered hive partition keys for an S3 prefix by walking the
/// object store one `key=value/` level at a time via delimiter listings.
/// Best-effort: any listing error stops discovery and returns what was found
/// so far (empty = treat as a flat folder).
async fn discover_s3_hive_keys(ctx: &SessionContext, url: &ListingTableUrl) -> Vec<String> {
    let store = match ctx.runtime_env().object_store(url.object_store()) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut keys = Vec::new();
    let mut prefix = url.prefix().clone();
    loop {
        let listing = match store.list_with_delimiter(Some(&prefix)).await {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut next: Option<object_store::path::Path> = None;
        for cp in &listing.common_prefixes {
            if let Some(seg) = cp.parts().next_back() {
                let seg = seg.as_ref().to_string();
                if let Some((k, v)) = seg.split_once('=')
                    && !k.is_empty()
                    && !v.is_empty()
                {
                    keys.push(k.to_string());
                    next = Some(cp.clone());
                    break;
                }
            }
        }
        match next {
            Some(p) => prefix = p,
            None => break,
        }
    }
    keys
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
        let probe = ParquetRecordBatchReaderBuilder::try_new(
            file.try_clone()
                .map_err(|e| AppError::Internal(format!("dup fd {}: {e}", f.display())))?,
        )?;
        let parquet_schema = probe.parquet_schema().clone();
        let arrow_schema = probe.schema().clone();
        let metadata = probe.metadata().clone();
        drop(probe);

        // Column projection (top-level / leaf indices for flat schemas).
        let projection = if let Some(w) = &wanted {
            let indices: Vec<usize> = arrow_schema
                .fields()
                .iter()
                .enumerate()
                .filter(|(_, fld)| w.contains(&fld.name().to_lowercase()))
                .map(|(i, _)| i)
                .collect();
            if indices.is_empty() {
                return Err(AppError::Internal(format!(
                    "dataset '{}': no columns from `columns = {:?}` match parquet schema for {}",
                    d.name,
                    d.columns,
                    f.display()
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
        let mut new_fields: Vec<Field> = arrow_schema
            .fields()
            .iter()
            .map(|f| f.as_ref().clone())
            .collect();
        if d.dict_encode
            && let Some(rg0) = metadata.row_groups().first()
        {
            for (i, fld) in arrow_schema.fields().iter().enumerate() {
                if !matches!(
                    fld.data_type(),
                    DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
                ) {
                    continue;
                }
                if let Some(col) = rg0.columns().get(i)
                    && col.dictionary_page_offset().is_some()
                {
                    new_fields[i] = Field::new(
                        fld.name(),
                        DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                        fld.is_nullable(),
                    );
                }
            }
        }
        let forced_schema = Arc::new(Schema::new(new_fields));

        let opts = ArrowReaderOptions::new().with_schema(forced_schema);
        let reader = ParquetRecordBatchReaderBuilder::try_new_with_options(file, opts)?
            .with_batch_size(65_536)
            .with_projection(projection)
            .build()?;
        // Hive-style partition columns (`city=NYC/…`) live in the path, not
        // the file. Fold them in as constant Utf8 columns so they show up in
        // the schema and are queryable — matching the DuckDB backend.
        let pairs = hive_pairs(f);
        for batch in reader {
            let batch = batch.map_err(|e| AppError::Internal(e.to_string()))?;
            all.push(if pairs.is_empty() {
                batch
            } else {
                append_partition_cols(&batch, &pairs)?
            });
        }
    }
    if all.is_empty() {
        return Err(AppError::Internal(format!(
            "dataset '{}': parquet source is empty",
            d.name
        )));
    }
    Ok(all)
}

/// Ordered hive-style partition `(key, value)` pairs encoded in a path, i.e.
/// directory components shaped like `key=value` (e.g. `year=2024/city=NYC`).
fn hive_pairs(path: &std::path::Path) -> Vec<(String, String)> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter_map(|seg| {
            let (k, v) = seg.split_once('=')?;
            if k.is_empty() || v.is_empty() || v.contains('=') {
                return None;
            }
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Append constant Utf8 columns for each hive partition pair. A partition
/// key that collides with a real file column is skipped (the file wins).
fn append_partition_cols(
    batch: &RecordBatch,
    pairs: &[(String, String)],
) -> Result<RecordBatch, AppError> {
    let n = batch.num_rows();
    let mut fields: Vec<Field> = batch
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    let mut cols: Vec<ArrayRef> = batch.columns().to_vec();
    for (k, v) in pairs {
        if fields.iter().any(|f| f.name() == k) {
            continue;
        }
        fields.push(Field::new(k, DataType::Utf8, false));
        cols.push(Arc::new(StringArray::from(vec![v.as_str(); n])));
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)
        .map_err(|e| AppError::Internal(e.to_string()))
}

/// Register an `AmazonS3` object store on the SessionContext, build a
/// `ListingTable` (with any hive partition columns) over the dataset prefix,
/// and stream the whole dataset back through `DataFrame::collect`. Using a
/// ListingTable here — rather than a bare `read_parquet` — means S3 sources
/// get the same hive-partition handling as local parquet.
async fn read_s3_parquet(
    d: &DatasetConfig,
    ctx: &SessionContext,
) -> Result<Vec<RecordBatch>, AppError> {
    register_s3_object_store(d, ctx)?;
    let (provider, _file_schema, _keys) = build_s3_listing_table(d, ctx).await?;
    let df = ctx
        .read_table(provider)
        .map_err(|e| AppError::Internal(format!("dataset '{}': s3 read_table: {e}", d.name)))?;
    Ok(df.collect().await?)
}

/// Open a Delta table (local or S3) and return the deltalake `DeltaTable`
/// handle. The transaction log is read here to resolve the current file
/// list + schema; the table's object store is registered on the session
/// `RuntimeEnv` lazily at scan time, so callers don't need to register a
/// store themselves. A location that doesn't exist, is empty, or was never
/// committed maps to [`AppError::EmptyDataset`] so startup can log-and-skip.
async fn open_delta_table(
    d: &DatasetConfig,
    opts: HashMap<String, String>,
) -> Result<deltalake::DeltaTable, AppError> {
    let url = deltalake::ensure_table_uri(&d.source.location).map_err(|e| {
        AppError::Internal(format!(
            "dataset '{}': bad delta location '{}': {e}",
            d.name, d.source.location
        ))
    })?;
    deltalake::open_table_with_storage_options(url, opts)
        .await
        .map_err(|e| {
            // A Delta location that doesn't exist, is empty, or was never
            // committed has no files in its log segment. deltalake (kernel
            // 0.32) surfaces every one of these as:
            //   "Not a Delta table: Generic delta kernel error: No files in
            //    log segment"
            // — covering a missing local dir AND a non-existent S3 prefix
            // alike. Match case-insensitively (the kernel capitalises the
            // phrases) and treat it like any other empty dataset so startup
            // logs and skips instead of aborting the whole process.
            let msg = e.to_string();
            let low = msg.to_lowercase();
            if low.contains("no files in log segment") || low.contains("not a delta table") {
                AppError::EmptyDataset(format!(
                    "delta location '{}' has no committed files ({msg})",
                    d.source.location
                ))
            } else {
                AppError::Internal(format!(
                    "dataset '{}': delta open '{}': {msg}",
                    d.name, d.source.location
                ))
            }
        })
}

/// Open a Delta table (local or S3) and return its DataFusion
/// `TableProvider`. The provider reads the transaction log to resolve the
/// current file list and registers the table's object store on the session
/// `RuntimeEnv` lazily at scan time, so callers don't need to register a
/// store themselves.
async fn open_delta_provider(
    d: &DatasetConfig,
    opts: HashMap<String, String>,
) -> Result<Arc<dyn TableProvider>, AppError> {
    let table = open_delta_table(d, opts).await?;
    table
        .table_provider()
        .await
        .map_err(|e| AppError::Internal(format!("dataset '{}': delta table_provider: {e}", d.name)))
}

/// Resolve the deltalake storage-options for a dataset: empty for local
/// tables, S3 credentials/endpoint for S3-backed ones.
fn delta_storage_options(d: &DatasetConfig) -> Result<HashMap<String, String>, AppError> {
    if d.source.is_s3() {
        delta_s3_options(d)
    } else {
        Ok(HashMap::new())
    }
}

/// Open a Delta table (local or S3) and stream every row back as a Vec of
/// `RecordBatch`. We materialise eagerly so the rest of the backend can
/// treat all datasets uniformly (single in-memory batch + eq-index).
async fn read_delta(
    d: &DatasetConfig,
    opts: HashMap<String, String>,
) -> Result<Vec<RecordBatch>, AppError> {
    let provider = open_delta_provider(d, opts).await?;
    // Drive a full scan via a throwaway SessionContext so we end up with
    // an in-memory Vec<RecordBatch> the shared materialise path can use.
    //
    // A Delta table can open cleanly (valid log + schema) yet still fail to
    // scan: its add actions may reference data files that no longer exist in
    // storage (e.g. vacuumed away), or are otherwise unreadable. Rather than
    // abort the whole registry for one broken table, map scan failures to
    // `EmptyDataset` so `Store::load` logs and skips it — mirroring the
    // bounded-probe behaviour on the lazy path.
    let scan_ctx = SessionContext::new();
    let df = scan_ctx.read_table(provider).map_err(|e| {
        AppError::EmptyDataset(format!(
            "delta location '{}' could not be scanned, skipping ({e})",
            d.source.location
        ))
    })?;
    df.collect().await.map_err(|e| {
        AppError::EmptyDataset(format!(
            "delta location '{}' could not be scanned, skipping ({e})",
            d.source.location
        ))
    })
}

/// Build a lazy state + deltalake `TableProvider` for a Delta dataset
/// (local or S3). The table is never read into RAM: deltalake reads the
/// transaction log once here to resolve the file list and discovery schema,
/// then streams parquet row groups on each query (with predicate pushdown
/// and Delta file skipping). The returned `DatasetState.data` is empty.
async fn build_lazy_delta(
    d: &DatasetConfig,
    _ctx: &SessionContext,
) -> Result<(DatasetState, Arc<dyn TableProvider>), AppError> {
    let table = open_delta_table(d, delta_storage_options(d)?).await?;

    // An empty Delta table carries a valid schema in its log but resolves to
    // zero data files. The eager path catches this naturally (a full scan
    // yields no batches), but the lazy path never scans — so check the file
    // list explicitly and skip, matching the eager behaviour. Without this an
    // empty Delta table would register as a 0-row dataset and show up in
    // discovery / explore.
    let file_count = table
        .get_file_uris()
        .map(|it| it.count())
        .map_err(|e| AppError::Internal(format!("dataset '{}': delta file list: {e}", d.name)))?;
    if file_count == 0 {
        return Err(AppError::EmptyDataset(format!(
            "delta location '{}' has a schema but no data files",
            d.source.location
        )));
    }

    let provider = table.table_provider().await.map_err(|e| {
        AppError::Internal(format!("dataset '{}': delta table_provider: {e}", d.name))
    })?;

    // The file-list check above only catches a Delta table with *no* data
    // files. A table can still be effectively empty in ways the log doesn't
    // make obvious: its add actions may reference files that contain zero
    // rows, or files that no longer exist in storage (e.g. vacuumed). Those
    // register fine here but then return nothing — or hard-error — the moment
    // they're queried, leaving a broken dataset visible in discovery/explore.
    //
    // The eager path catches all of this for free (a full scan yields no rows
    // or surfaces the read error up front), but the lazy path never scans. So
    // probe with a bounded one-row scan: it reads at most a single row group
    // from a single file, costing almost nothing on a healthy table, but lets
    // us log-and-skip an empty or unreadable table instead of registering it.
    {
        let probe_ctx = SessionContext::new();
        let probe = probe_ctx
            .read_table(provider.clone())
            .and_then(|df| df.limit(0, Some(1)));
        match probe {
            Ok(df) => match df.collect().await {
                Ok(batches) if batches.iter().all(|b| b.num_rows() == 0) => {
                    return Err(AppError::EmptyDataset(format!(
                        "delta location '{}' resolves to no rows",
                        d.source.location
                    )));
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(AppError::EmptyDataset(format!(
                        "delta location '{}' could not be scanned, skipping ({e})",
                        d.source.location
                    )));
                }
            },
            Err(e) => {
                return Err(AppError::EmptyDataset(format!(
                    "delta location '{}' could not be scanned, skipping ({e})",
                    d.source.location
                )));
            }
        }
    }

    // The provider's schema is the full logical schema (data + partition
    // columns), which is exactly the discovery schema we want.
    let arrow_sch = provider.schema();
    let columns: Vec<ColumnInfo> = arrow_sch
        .fields()
        .iter()
        .map(|f| {
            let dt = f.data_type();
            ColumnInfo {
                name: f.name().clone(),
                logical: arrow_to_logical(dt),
                sql_type: format!("{dt:?}"),
                nullable: f.is_nullable(),
            }
        })
        .collect();
    let schema = DatasetSchema::new(&d.name, columns)
        .with_filters(d.predicate_filter.clone(), d.projection_filter.clone())?;

    log::info!(
        "dataset '{}' [{}, lazy]: {} cols, no materialise, no index",
        d.name,
        d.source.kind.as_str(),
        schema.columns.len()
    );

    Ok((
        DatasetState {
            schema,
            data: Vec::new(),
            arrow_schema: arrow_sch,
            index: EqIndex::default(),
            lazy: true,
        },
        provider,
    ))
}

/// Build the storage-options HashMap that `deltalake::open_table_with_storage_options`
/// expects for S3 access. Keys mirror the AWS env-var names; deltalake
/// passes them through to object_store internally.
fn delta_s3_options(d: &DatasetConfig) -> Result<HashMap<String, String>, AppError> {
    let creds = d.resolved_creds();
    let region = d.resolved_region();
    let s3 = d.s3.clone().unwrap_or_default();
    let (bucket, _) = d.source.s3_bucket()?;

    let mut opts = HashMap::new();
    opts.insert("AWS_REGION".into(), region);
    if let Some(ep) = s3.effective_endpoint(bucket) {
        opts.insert("AWS_ENDPOINT_URL".into(), ep);
    }
    if s3.allow_http {
        opts.insert("AWS_ALLOW_HTTP".into(), "true".into());
    }
    opts.insert(
        "AWS_VIRTUAL_HOSTED_STYLE_REQUEST".into(),
        (s3.addressing_style == AddressingStyle::Virtual).to_string(),
    );
    if let Some(k) = creds.access_key_id {
        opts.insert("AWS_ACCESS_KEY_ID".into(), k);
    }
    if let Some(s) = creds.secret_access_key {
        opts.insert("AWS_SECRET_ACCESS_KEY".into(), s);
    }
    if let Some(t) = creds.session_token {
        opts.insert("AWS_SESSION_TOKEN".into(), t);
    }
    // Read-only paths don't need the S3 lock-provider plumbing.
    opts.insert("AWS_S3_ALLOW_UNSAFE_RENAME".into(), "true".into());
    Ok(opts)
}

/// Construct an `AmazonS3` object_store from the dataset's `[dataset.s3]`
/// block + resolved credentials and register it on `ctx` under
/// `s3://bucket/`.
fn register_s3_object_store(d: &DatasetConfig, ctx: &SessionContext) -> Result<(), AppError> {
    let (bucket, _key) = d.source.s3_bucket()?;
    let creds = d.resolved_creds();
    let region = d.resolved_region();
    let s3 = d.s3.clone().unwrap_or_default();

    let store = build_s3(bucket, &region, &s3, &creds).map_err(|e| {
        AppError::Internal(format!(
            "dataset '{}': build S3 store for '{bucket}': {e}",
            d.name
        ))
    })?;

    let url = Url::parse(&format!("s3://{bucket}"))
        .map_err(|e| AppError::Internal(format!("invalid s3 URL for bucket {bucket}: {e}")))?;
    ctx.register_object_store(&url, Arc::new(store));
    Ok(())
}

/// Best-effort detection of an S3 authorization failure (HTTP 403) from an
/// error message. object_store, the AWS SDK, deltalake, and MinIO all
/// surface a denied read as some mix of "Access Denied" / "AccessDenied"
/// (the S3 error code), "Forbidden", or a bare "403" in the rendered error
/// chain. Matched case-insensitively. Only consulted for sources we already
/// know are S3, so a stray "forbidden" in unrelated text can't misfire.
fn is_s3_access_denied(msg: &str) -> bool {
    let low = msg.to_lowercase();
    low.contains("access denied")
        || low.contains("accessdenied")
        || low.contains("forbidden")
        || low.contains("403")
}

///
/// Local sources are sized with a cheap filesystem stat (`estimate_local_bytes`);
/// S3 sources are sized by listing the object store under their prefix. S3
/// sizing is best-effort: a listing failure logs a warning and returns `None`
/// (don't force) so a transient S3 error never blocks startup.
async fn should_force_lazy(d: &DatasetConfig, server: &ServerConfig) -> Option<u64> {
    if d.lazy || server.force_lazy_above_mb == 0 {
        return None;
    }
    let threshold = server.force_lazy_above_mb.saturating_mul(1024 * 1024);

    let bytes = if d.source.is_s3() {
        match estimate_s3_bytes(d).await {
            Ok(b) => b,
            Err(e) => {
                log::warn!(
                    "dataset '{}': could not measure S3 size for force_lazy_above_mb: {e}",
                    d.name
                );
                return None;
            }
        }
    } else {
        d.estimate_local_bytes()?
    };

    (bytes > threshold).then_some(bytes)
}

/// Sum the byte size of a dataset's S3 backing data by listing the object
/// store under the source prefix and adding up every `.parquet` object.
///
/// Works for both parquet and delta sources: delta data files are parquet
/// objects under the table root, so listing the root and counting parquet
/// gives a coarse (possibly over-counting un-vacuumed files) but cheap size
/// suitable for the force-lazy gate. Any glob wildcard tail is stripped so the
/// listing starts at the non-wildcard base prefix.
async fn estimate_s3_bytes(d: &DatasetConfig) -> Result<u64, AppError> {
    use futures_util::StreamExt;
    use object_store::ObjectStore;

    let (bucket, _key) = d.source.s3_bucket()?;
    let creds = d.resolved_creds();
    let region = d.resolved_region();
    let s3 = d.s3.clone().unwrap_or_default();
    let store = build_s3(bucket, &region, &s3, &creds).map_err(|e| {
        AppError::Internal(format!(
            "dataset '{}': build S3 store for '{bucket}': {e}",
            d.name
        ))
    })?;

    // Strip any glob wildcard tail, then reduce the base to a bucket-relative
    // key for object_store's `list` (which takes a key prefix, not a URL).
    let (base, _keys) = split_glob_base_keys(&d.source.location);
    let prefix_key = base
        .strip_prefix("s3://")
        .and_then(|rest| rest.split_once('/').map(|(_bucket, key)| key))
        .unwrap_or("")
        .trim_end_matches('/');
    let prefix = (!prefix_key.is_empty()).then(|| object_store::path::Path::from(prefix_key));

    let mut total: u64 = 0;
    let mut stream = store.list(prefix.as_ref());
    while let Some(meta) = stream.next().await {
        let meta = meta.map_err(|e| {
            AppError::Internal(format!(
                "dataset '{}': s3 list under '{prefix_key}': {e}",
                d.name
            ))
        })?;
        if meta.location.as_ref().ends_with(".parquet") {
            total = total.saturating_add(meta.size);
        }
    }
    Ok(total)
}

fn build_s3(
    bucket: &str,
    region: &str,
    s3: &S3Config,
    creds: &ResolvedCreds,
) -> Result<object_store::aws::AmazonS3, object_store::Error> {
    let mut b = AmazonS3Builder::new()
        .with_bucket_name(bucket)
        .with_region(region)
        .with_allow_http(s3.allow_http)
        .with_virtual_hosted_style_request(s3.addressing_style == AddressingStyle::Virtual);
    if let Some(ep) = s3.effective_endpoint(bucket) {
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
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => LogicalType::Int,
        DataType::Float16 | DataType::Float32 | DataType::Float64 => LogicalType::Float,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => LogicalType::Utf8,
        // Dictionary-encoded strings are reported as plain strings — clients
        // (and the rest of the backend) shouldn't have to care that we keep
        // a compressed representation in memory.
        DataType::Dictionary(_, v)
            if matches!(
                v.as_ref(),
                DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
            ) =>
        {
            LogicalType::Utf8
        }
        DataType::Date32
        | DataType::Date64
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Timestamp(_, _)
        | DataType::Duration(_)
        | DataType::Interval(_) => LogicalType::Temporal,
        _ => LogicalType::Other,
    }
}

// ---------------------------------------------------------------------------
// Per-batch projection
// ---------------------------------------------------------------------------

fn project(
    schema: &DatasetSchema,
    batch: RecordBatch,
    columns: &[String],
) -> Result<RecordBatch, AppError> {
    if columns.is_empty() {
        return Ok(batch);
    }
    let indices: Vec<usize> = columns
        .iter()
        .map(|c| {
            schema
                .find(c)
                .map(|info| schema.by_name[&info.name.to_lowercase()])
        })
        .collect::<Result<_, _>>()?;
    let fields: Vec<Field> = indices
        .iter()
        .map(|&i| batch.schema().field(i).clone())
        .collect();
    let cols: Vec<ArrayRef> = indices.iter().map(|&i| batch.column(i).clone()).collect();
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
}

// ---------------------------------------------------------------------------
// SQL builder
// ---------------------------------------------------------------------------

/// Accumulates the typed literal values for a parameterised query.
///
/// Predicate values are never interpolated into the SQL text. Instead each
/// value is pushed here and the builder emits a positional placeholder
/// (`$1`, `$2`, …) referencing it. The collected [`ScalarValue`]s are bound
/// to the logical plan via [`DataFrame::with_param_values`], so user input
/// reaches the engine as typed scalars and can never alter the query
/// structure (no SQL injection surface, no escaping to get wrong).
#[derive(Default)]
struct Params {
    values: Vec<ScalarValue>,
}

impl Params {
    fn new() -> Self {
        Self::default()
    }

    /// Bind `v` and return its `$N` placeholder token.
    fn bind(&mut self, v: ScalarValue) -> String {
        self.values.push(v);
        format!("${}", self.values.len())
    }

    fn into_values(self) -> Vec<ScalarValue> {
        self.values
    }
}

fn build_query_sql(
    schema: &DatasetSchema,
    req: &QueryRequest,
    max_page_size: u64,
) -> Result<(String, Vec<ScalarValue>), AppError> {
    let (limit, offset) = req.effective_limit_offset(max_page_size);
    build_query_sql_with_suffix(schema, req, &format!(" LIMIT {limit} OFFSET {offset}"))
}

fn build_query_stream_sql(
    schema: &DatasetSchema,
    req: &QueryRequest,
) -> Result<(String, Vec<ScalarValue>), AppError> {
    let suffix = req
        .limit
        .map(|limit| format!(" LIMIT {limit}"))
        .unwrap_or_default();
    build_query_sql_with_suffix(schema, req, &suffix)
}

fn build_query_sql_with_suffix(
    schema: &DatasetSchema,
    req: &QueryRequest,
    suffix: &str,
) -> Result<(String, Vec<ScalarValue>), AppError> {
    let agg_plan = req.agg_plan(schema)?;

    let cols = if let Some(plan) = &agg_plan {
        // Group cols, then aggregations, each aliased to the JSON output key.
        let mut parts: Vec<String> = plan
            .group_cols
            .iter()
            .map(|c| DatasetSchema::quote_ident(c))
            .collect();
        for a in &plan.aggs {
            let expr = a.sql_expr()?;
            parts.push(format!(
                "{expr} AS {}",
                DatasetSchema::quote_ident(&a.alias)
            ));
        }
        parts.join(", ")
    } else if req.columns.is_empty() {
        if req.distinct {
            "DISTINCT *".to_string()
        } else {
            "*".to_string()
        }
    } else {
        let list = req
            .columns
            .iter()
            .map(|c| {
                schema
                    .find(c)
                    .map(|info| DatasetSchema::quote_ident(&info.name))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        if req.distinct {
            format!("DISTINCT {list}")
        } else {
            list
        }
    };

    let mut params = Params::new();
    let clauses: Vec<String> = req
        .predicates
        .iter()
        .map(|p| pred_to_sql(schema, p, &mut params))
        .collect::<Result<_, _>>()?;

    let table = DatasetSchema::quote_ident(&schema.name);
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
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
    let having_clause = {
        let resolved = req.having_plan(agg_plan.as_ref())?;
        if resolved.is_empty() {
            String::new()
        } else {
            let clauses: Vec<String> = resolved
                .iter()
                .map(|(lhs, p)| pred_to_sql_with_lhs(lhs, p, &mut params))
                .collect::<Result<_, _>>()?;
            format!(" HAVING {}", clauses.join(" AND "))
        }
    };
    let order_clause = match req.order_by_sql(schema, agg_plan.as_ref())? {
        Some(s) => format!(" ORDER BY {s}"),
        None => String::new(),
    };
    let sql = format!(
        "SELECT {cols} FROM {table}{where_clause}{group_clause}{having_clause}{order_clause}{suffix}"
    );
    Ok((sql, params.into_values()))
}

fn build_count_sql(
    schema: &DatasetSchema,
    predicates: &[Predicate],
) -> Result<(String, Vec<ScalarValue>), AppError> {
    let mut params = Params::new();
    let clauses: Vec<String> = predicates
        .iter()
        .map(|p| pred_to_sql(schema, p, &mut params))
        .collect::<Result<_, _>>()?;
    let table = DatasetSchema::quote_ident(&schema.name);
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let sql = format!("SELECT COUNT(*) FROM {table}{where_clause}");
    Ok((sql, params.into_values()))
}

fn pred_to_sql(
    schema: &DatasetSchema,
    pred: &Predicate,
    params: &mut Params,
) -> Result<String, AppError> {
    let info = schema.find(&pred.col)?;
    let col = DatasetSchema::quote_ident(&info.name);
    pred_to_sql_with_lhs(&col, pred, params)
}

/// Render a predicate against a pre-resolved left-hand-side SQL
/// expression. The dataset-`WHERE` path resolves a quoted column name as
/// the LHS (see [`pred_to_sql`]); the `HAVING` path passes an aggregate
/// expression such as `COUNT(*)` instead. Both share the operator and
/// value-binding logic here.
fn pred_to_sql_with_lhs(
    col: &str,
    pred: &Predicate,
    params: &mut Params,
) -> Result<String, AppError> {
    match pred.op.as_str() {
        "is_null" => return Ok(format!("{col} IS NULL")),
        "is_not_null" => return Ok(format!("{col} IS NOT NULL")),
        _ => {}
    }

    let val = pred
        .val
        .as_ref()
        .ok_or_else(|| AppError::InvalidValue(format!("'{}' requires a value", pred.op)))?;

    if pred.op == "in" {
        let items = val
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| AppError::InvalidValue("'in' needs a non-empty array".into()))?;
        let placeholders: Vec<String> = items
            .iter()
            .map(|item| Ok(params.bind(json_to_scalar(item)?)))
            .collect::<Result<_, AppError>>()?;
        return Ok(format!("{col} IN ({})", placeholders.join(", ")));
    }

    let sql_op = match pred.op.as_str() {
        "eq" => "=",
        "neq" => "!=",
        "gt" => ">",
        "gte" => ">=",
        "lt" => "<",
        "lte" => "<=",
        "like" => "LIKE",
        "ilike" => "ILIKE",
        other => return Err(AppError::UnknownOperator(other.into())),
    };
    let placeholder = params.bind(json_to_scalar(val)?);
    Ok(format!("{col} {sql_op} {placeholder}"))
}

/// Convert a JSON predicate value into a typed Arrow [`ScalarValue`] for
/// binding as a query parameter. The engine applies the usual numeric
/// widening / comparison coercion against the target column type.
fn json_to_scalar(val: &JsonValue) -> Result<ScalarValue, AppError> {
    match val {
        JsonValue::String(s) => Ok(ScalarValue::Utf8(Some(s.clone()))),
        JsonValue::Bool(b) => Ok(ScalarValue::Boolean(Some(*b))),
        JsonValue::Null => Ok(ScalarValue::Null),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ScalarValue::Int64(Some(i)))
            } else if let Some(u) = n.as_u64() {
                Ok(ScalarValue::UInt64(Some(u)))
            } else if let Some(f) = n.as_f64() {
                Ok(ScalarValue::Float64(Some(f)))
            } else {
                Err(AppError::InvalidValue(
                    "unsupported numeric literal in predicate".into(),
                ))
            }
        }
        _ => Err(AppError::InvalidValue(
            "unsupported literal type in predicate".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Equality index — built once at startup, queried on every predicate request
// ---------------------------------------------------------------------------

fn json_index_key(val: &JsonValue) -> Option<String> {
    match val {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn intersect_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            Ordering::Less => i += 1,
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
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn try_index<'a>(index: &'a EqIndex, predicates: &[Predicate]) -> Option<Cow<'a, [u32]>> {
    if predicates.is_empty() || index.is_empty() {
        return None;
    }

    // Fast path: a single `eq` predicate resolves to exactly one index bucket,
    // so borrow it directly instead of cloning the row-id vector.
    if let [pred] = predicates
        && pred.op.as_str() == "eq"
    {
        let col_lower = pred.col.to_lowercase();
        let col_map = index.get(&col_lower)?;
        let key = json_index_key(pred.val.as_ref()?)?;
        return Some(match col_map.get(&key) {
            Some(rows) => Cow::Borrowed(rows.as_slice()),
            None => Cow::Owned(Vec::new()),
        });
    }

    let mut result: Option<Vec<u32>> = None;
    for pred in predicates {
        let col_lower = pred.col.to_lowercase();
        let col_map = index.get(&col_lower)?;

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
            None => rows,
            Some(r) => intersect_sorted(&r, &rows),
        });
    }
    result.map(Cow::Owned)
}

/// Benchmark-only hooks for the equality-index hot path. Hidden from docs and
/// not part of the stable API — exposed solely so `benches/index.rs` can build
/// an `EqIndex` and exercise `try_index` directly.
#[doc(hidden)]
pub mod bench {
    use super::{EqIndex, FastMap, json_index_key, try_index};
    use datapress_core::models::Predicate;
    use serde_json::Value as JsonValue;
    use std::borrow::Cow;

    /// Opaque equality index wrapper for benches (the inner alias is private).
    pub struct BenchIndex(EqIndex);

    /// Build an index with a single `col` whose `val` bucket holds `rows`.
    /// The bucket key is derived with the same `json_index_key` the query path
    /// uses, so a matching `eq` predicate is guaranteed to hit.
    pub fn single_bucket_index(col: &str, val: &JsonValue, rows: Vec<u32>) -> BenchIndex {
        let key = json_index_key(val).expect("benchable index key");
        let mut col_map: FastMap<String, Vec<u32>> = FastMap::default();
        col_map.insert(key, rows);
        let mut index: EqIndex = EqIndex::default();
        index.insert(col.to_string(), col_map);
        BenchIndex(index)
    }

    /// Resolve `predicates` against the index — the timed operation.
    pub fn lookup<'a>(idx: &'a BenchIndex, predicates: &[Predicate]) -> Option<Cow<'a, [u32]>> {
        try_index(&idx.0, predicates)
    }

    /// Reference implementation of the pre-`Cow` behaviour: always clones the
    /// matched bucket into an owned `Vec` (what `try_index` did before the
    /// single-`eq` borrow fast path). Used as the `clone-before` baseline so
    /// the borrow win is measured against the old allocation cost in-process.
    pub fn lookup_cloning(idx: &BenchIndex, predicates: &[Predicate]) -> Option<Vec<u32>> {
        let [pred] = predicates else { return None };
        if pred.op.as_str() != "eq" {
            return None;
        }
        let col_lower = pred.col.to_lowercase();
        let col_map = idx.0.get(&col_lower)?;
        let key = json_index_key(pred.val.as_ref()?)?;
        Some(col_map.get(&key).cloned().unwrap_or_default())
    }
}

/// Return rows `[offset, offset+limit)` from a chunked dataset by slicing
/// the underlying batches (zero-copy) and concatenating the (small) page.
fn slice_global(
    chunks: &[RecordBatch],
    schema: &Arc<Schema>,
    offset: usize,
    limit: usize,
) -> Result<RecordBatch, AppError> {
    if limit == 0 || chunks.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }
    let mut out = Vec::new();
    let mut to_skip = offset;
    let mut remaining = limit;
    for b in chunks {
        if remaining == 0 {
            break;
        }
        let n = b.num_rows();
        if to_skip >= n {
            to_skip -= n;
            continue;
        }
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
    rows: &[u32],
    offset: usize,
    limit: usize,
) -> Result<RecordBatch, AppError> {
    let start = offset.min(rows.len());
    let len = limit.min(rows.len() - start);
    if len == 0 || chunks.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }

    // Prefix-sum table: `offsets[i]` is the first global row id of chunk `i`,
    // and `offsets.last()` is the total row count.
    let mut offsets: Vec<u32> = Vec::with_capacity(chunks.len() + 1);
    let mut acc: u32 = 0;
    offsets.push(0);
    for b in chunks {
        acc = acc
            .checked_add(b.num_rows() as u32)
            .expect("row count exceeds u32::MAX");
        offsets.push(acc);
    }

    // Bucket each global row id into the chunk that contains it, remembering
    // the original output position so we can restore page order at the end.
    let mut buckets: Vec<Vec<(u32, u32)>> = (0..chunks.len()).map(|_| Vec::new()).collect();
    for (out_pos, &gid) in rows[start..start + len].iter().enumerate() {
        let bi = offsets.partition_point(|&x| x <= gid).saturating_sub(1);
        let local = gid - offsets[bi];
        buckets[bi].push((out_pos as u32, local));
    }

    // Per-chunk take, recording the destination index for each emitted row.
    let mut takens: Vec<RecordBatch> = Vec::new();
    let mut dest: Vec<u32> = Vec::with_capacity(len);
    for (bi, bucket) in buckets.iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let idx = UInt32Array::from(bucket.iter().map(|(_, l)| *l).collect::<Vec<u32>>());
        let cols: Vec<ArrayRef> = chunks[bi]
            .columns()
            .iter()
            .map(|c| {
                arrow::compute::take(c.as_ref(), &idx, None::<arrow::compute::TakeOptions>)
                    .map_err(AppError::from)
            })
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
    let cols: Vec<ArrayRef> = stitched
        .columns()
        .iter()
        .map(|c| {
            arrow::compute::take(c.as_ref(), &perm, None::<arrow::compute::TakeOptions>)
                .map_err(AppError::from)
        })
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(stitched.schema(), cols).map_err(AppError::from)
}

/// Build the equality index per the dataset's policy, against the chunked
/// representation. Row ids are global across the concatenation of all
/// chunks (so they remain compatible with `take_page` / `slice_global`).
fn build_eq_index_with_policy(chunks: &[RecordBatch], cfg: &IndexConfig) -> EqIndex {
    use rayon::prelude::*;

    if cfg.mode == IndexMode::None || chunks.is_empty() {
        return EqIndex::default();
    }

    let allow: Option<HashMap<String, ()>> = if cfg.mode == IndexMode::List {
        Some(cfg.columns.iter().map(|c| (c.to_lowercase(), ())).collect())
    } else {
        None
    };

    let max_card = if cfg.mode == IndexMode::Auto {
        Some(cfg.max_cardinality)
    } else {
        None
    };

    // Per-chunk starting global row id.
    let mut batch_offsets: Vec<u32> = Vec::with_capacity(chunks.len());
    let mut acc: u32 = 0;
    for b in chunks {
        batch_offsets.push(acc);
        acc = acc
            .checked_add(b.num_rows() as u32)
            .expect("row count exceeds u32::MAX");
    }

    let schema = chunks[0].schema();

    schema
        .fields()
        .par_iter()
        .enumerate()
        .filter_map(|(ci, field)| {
            let col_lower = field.name().to_lowercase();
            if let Some(a) = &allow
                && !a.contains_key(&col_lower)
            {
                return None;
            }

            // Only build for index-friendly types; skip everything else
            // up-front so we don't pay the per-chunk dispatch cost.
            let dtype = field.data_type();
            let dict_utf8 = matches!(dtype,
                DataType::Dictionary(k, v)
                    if matches!(k.as_ref(), DataType::Int32)
                    && matches!(v.as_ref(), DataType::Utf8));
            match dtype {
                DataType::Utf8
                | DataType::Utf8View
                | DataType::Boolean
                | DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64 => {}
                _ if dict_utf8 => {}
                _ => return None,
            }

            let mut map: FastMap<String, Vec<u32>> = FastMap::default();

            for (bi, batch) in chunks.iter().enumerate() {
                let base = batch_offsets[bi];
                let col = batch.column(ci);

                macro_rules! index_col {
                    ($arr_ty:ty) => {{
                        let arr = col.as_any().downcast_ref::<$arr_ty>()?;
                        for row in 0..arr.len() {
                            if arr.is_null(row) {
                                continue;
                            }
                            let key = arr.value(row).to_string();
                            let gid = base + row as u32;
                            if let Some(v) = map.get_mut(&key) {
                                v.push(gid);
                            } else {
                                if let Some(mc) = max_card {
                                    if map.len() >= mc {
                                        return None;
                                    }
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
                    let arr = col
                        .as_any()
                        .downcast_ref::<arrow::array::DictionaryArray<arrow::datatypes::Int32Type>>(
                        )?;
                    let keys = arr.keys();
                    let values = arr.values().as_any().downcast_ref::<StringArray>()?;
                    for row in 0..arr.len() {
                        if arr.is_null(row) {
                            continue;
                        }
                        let k = keys.value(row) as usize;
                        let s = values.value(k);
                        let gid = base + row as u32;
                        if let Some(v) = map.get_mut(s) {
                            v.push(gid);
                        } else {
                            if let Some(mc) = max_card
                                && map.len() >= mc
                            {
                                return None;
                            }
                            map.insert(s.to_string(), vec![gid]);
                        }
                    }
                } else {
                    match dtype {
                        DataType::Utf8 => index_col!(StringArray),
                        DataType::Utf8View => index_col!(StringViewArray),
                        DataType::Boolean => index_col!(BooleanArray),
                        DataType::Int8 => index_col!(Int8Array),
                        DataType::Int16 => index_col!(Int16Array),
                        DataType::Int32 => index_col!(Int32Array),
                        DataType::Int64 => index_col!(Int64Array),
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
// columns would balloon resident RAM. The cast is applied per returned page
// after pagination, so the cost is paid only for rows the caller requested.
// ---------------------------------------------------------------------------

/// Returns true for Arrow types that `write_value` can render directly. Any
/// type returning false is pre-cast to Utf8 in [`cast_for_serialize`] so the
/// JSON output is faithful rather than silently `null`.
fn writable_inline(dt: &DataType) -> bool {
    match dt {
        DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Boolean
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => true,
        DataType::Dictionary(k, v)
            if matches!(k.as_ref(), DataType::Int32) && matches!(v.as_ref(), DataType::Utf8) =>
        {
            true
        }
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
    let to_cast: Vec<usize> = schema
        .fields()
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            if writable_inline(f.data_type()) {
                None
            } else {
                Some(i)
            }
        })
        .collect();
    if to_cast.is_empty() {
        return Ok(batch.clone());
    }
    let new_fields: Vec<Field> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if to_cast.contains(&i) {
                Field::new(f.name(), DataType::Utf8, f.is_nullable())
            } else {
                f.as_ref().clone()
            }
        })
        .collect();
    let new_schema = Arc::new(Schema::new(new_fields));
    let cols: Vec<ArrayRef> = batch
        .columns()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if to_cast.contains(&i) {
                compute::cast(c.as_ref(), &DataType::Utf8).map_err(AppError::from)
            } else {
                Ok(c.clone())
            }
        })
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(new_schema, cols).map_err(AppError::from)
}

// ---------------------------------------------------------------------------
// Compute helpers — retained for symmetry; reserved for future inline scan
// path. Currently the engine fallback handles all non-index queries.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum CmpOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
    ILike,
}

#[allow(dead_code)]
fn eq_str(col: &ArrayRef, val: &str) -> Result<BooleanArray, AppError> {
    let arr = col
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| AppError::InvalidValue("equality: column is not a string".into()))?;
    let s = Scalar::new(StringArray::from(vec![val]));
    Ok(eq(arr, &s)?)
}

#[allow(dead_code)]
fn cmp_scalar(col: &ArrayRef, op: CmpOp, val: &JsonValue) -> Result<BooleanArray, AppError> {
    macro_rules! num_cmp {
        ($arr_type:ty, $cast:ty) => {{
            let n = val
                .as_f64()
                .ok_or_else(|| AppError::InvalidValue("expected number".into()))?
                as $cast;
            let arr = col.as_any().downcast_ref::<$arr_type>().unwrap();
            let s = Scalar::new(<$arr_type>::from(vec![n]));
            Ok(match op {
                CmpOp::Eq => eq(arr, &s)?,
                CmpOp::Neq => neq(arr, &s)?,
                CmpOp::Gt => gt(arr, &s)?,
                CmpOp::Gte => gt_eq(arr, &s)?,
                CmpOp::Lt => lt(arr, &s)?,
                CmpOp::Lte => lt_eq(arr, &s)?,
                CmpOp::Like | CmpOp::ILike => {
                    return Err(AppError::InvalidValue(
                        "LIKE requires a string column".into(),
                    ));
                }
            })
        }};
    }
    match col.data_type() {
        DataType::Utf8 => {
            let s = val
                .as_str()
                .ok_or_else(|| AppError::InvalidValue("expected string".into()))?;
            let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
            let sc = Scalar::new(StringArray::from(vec![s]));
            Ok(match op {
                CmpOp::Eq => eq(arr, &sc)?,
                CmpOp::Neq => neq(arr, &sc)?,
                CmpOp::Gt => gt(arr, &sc)?,
                CmpOp::Gte => gt_eq(arr, &sc)?,
                CmpOp::Lt => lt(arr, &sc)?,
                CmpOp::Lte => lt_eq(arr, &sc)?,
                CmpOp::Like => compute::like(arr, &sc)?,
                CmpOp::ILike => compute::ilike(arr, &sc)?,
            })
        }
        DataType::Int8 => num_cmp!(Int8Array, i8),
        DataType::Int16 => num_cmp!(Int16Array, i16),
        DataType::Int32 => num_cmp!(Int32Array, i32),
        DataType::Int64 => num_cmp!(Int64Array, i64),
        DataType::Float32 => num_cmp!(Float32Array, f32),
        DataType::Float64 => num_cmp!(Float64Array, f64),
        dt => Err(AppError::InvalidValue(format!(
            "unsupported type for comparison: {dt:?}"
        ))),
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
    let batch = cast_for_serialize(batch)?;
    let schema = batch.schema();
    let n_rows = batch.num_rows();

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

    // Resolve every column's concrete Arrow array type exactly ONCE here,
    // instead of paying a `data_type()` match + `downcast_ref` for every
    // single cell. The inner row loop then dispatches over a small enum,
    // which the compiler turns into a cheap jump table.
    let encoders: Vec<ColEnc> = batch
        .columns()
        .iter()
        .map(|c| ColEnc::new(c.as_ref()))
        .collect();

    let mut buf: Vec<u8> = Vec::with_capacity(n_rows.max(1) * 300);
    let mut itoa_buf = itoa::Buffer::new();
    let mut ryu_buf = ryu::Buffer::new();
    buf.push(b'[');

    for row in 0..n_rows {
        if row > 0 {
            buf.push(b',');
        }
        buf.push(b'{');
        for (i, (key, enc)) in keys.iter().zip(encoders.iter()).enumerate() {
            if i > 0 {
                buf.push(b',');
            }
            buf.extend_from_slice(key);
            enc.write(&mut buf, row, &mut itoa_buf, &mut ryu_buf);
        }
        buf.push(b'}');
    }

    buf.push(b']');
    Ok(unsafe { String::from_utf8_unchecked(buf) })
}

/// Per-column JSON encoder. The concrete Arrow array type is resolved once
/// (in [`ColEnc::new`]) so the hot row loop dispatches over this small enum
/// instead of repeating `data_type()` matching + `downcast_ref` for every
/// cell. Each variant borrows the already-downcast typed array.
enum ColEnc<'a> {
    Utf8(&'a StringArray),
    LargeUtf8(&'a LargeStringArray),
    Utf8View(&'a StringViewArray),
    /// Dictionary-encoded UTF-8 (Int32 keys). Holds the keys array and the
    /// pre-downcast string values so neither is re-resolved per row.
    DictI32Utf8(
        &'a arrow::array::DictionaryArray<arrow::datatypes::Int32Type>,
        &'a StringArray,
    ),
    Bool(&'a BooleanArray),
    I8(&'a Int8Array),
    I16(&'a Int16Array),
    I32(&'a Int32Array),
    I64(&'a Int64Array),
    U8(&'a UInt8Array),
    U16(&'a UInt16Array),
    U32(&'a UInt32Array),
    U64(&'a UInt64Array),
    Dec128(&'a Decimal128Array),
    Dec256(&'a Decimal256Array),
    F32(&'a Float32Array),
    F64(&'a Float64Array),
    /// Anything else: fall back to the generic `write_value` dispatch.
    Other(&'a dyn Array),
}

impl<'a> ColEnc<'a> {
    fn new(col: &'a dyn Array) -> ColEnc<'a> {
        macro_rules! dc {
            ($t:ty) => {
                col.as_any().downcast_ref::<$t>().unwrap()
            };
        }
        match col.data_type() {
            DataType::Utf8 => ColEnc::Utf8(dc!(StringArray)),
            DataType::LargeUtf8 => ColEnc::LargeUtf8(dc!(LargeStringArray)),
            DataType::Utf8View => ColEnc::Utf8View(dc!(StringViewArray)),
            DataType::Dictionary(key, value)
                if matches!(key.as_ref(), DataType::Int32)
                    && matches!(value.as_ref(), DataType::Utf8) =>
            {
                let dict = dc!(arrow::array::DictionaryArray<arrow::datatypes::Int32Type>);
                let values = dict
                    .values()
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                ColEnc::DictI32Utf8(dict, values)
            }
            DataType::Boolean => ColEnc::Bool(dc!(BooleanArray)),
            DataType::Int8 => ColEnc::I8(dc!(Int8Array)),
            DataType::Int16 => ColEnc::I16(dc!(Int16Array)),
            DataType::Int32 => ColEnc::I32(dc!(Int32Array)),
            DataType::Int64 => ColEnc::I64(dc!(Int64Array)),
            DataType::UInt8 => ColEnc::U8(dc!(UInt8Array)),
            DataType::UInt16 => ColEnc::U16(dc!(UInt16Array)),
            DataType::UInt32 => ColEnc::U32(dc!(UInt32Array)),
            DataType::UInt64 => ColEnc::U64(dc!(UInt64Array)),
            DataType::Decimal128(_, _) => ColEnc::Dec128(dc!(Decimal128Array)),
            DataType::Decimal256(_, _) => ColEnc::Dec256(dc!(Decimal256Array)),
            DataType::Float32 => ColEnc::F32(dc!(Float32Array)),
            DataType::Float64 => ColEnc::F64(dc!(Float64Array)),
            _ => ColEnc::Other(col),
        }
    }

    #[inline]
    fn write(
        &self,
        buf: &mut Vec<u8>,
        row: usize,
        itoa_buf: &mut itoa::Buffer,
        ryu_buf: &mut ryu::Buffer,
    ) {
        macro_rules! int {
            ($arr:expr) => {{
                if $arr.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    buf.extend_from_slice(itoa_buf.format($arr.value(row)).as_bytes());
                }
            }};
        }
        match self {
            ColEnc::Utf8(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_str(buf, a.value(row));
                }
            }
            ColEnc::LargeUtf8(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_str(buf, a.value(row));
                }
            }
            ColEnc::Utf8View(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_str(buf, a.value(row));
                }
            }
            ColEnc::DictI32Utf8(keys, values) => {
                if keys.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    let k = keys.keys().value(row) as usize;
                    write_str(buf, values.value(k));
                }
            }
            ColEnc::Bool(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    buf.extend_from_slice(if a.value(row) { b"true" } else { b"false" });
                }
            }
            ColEnc::I8(a) => int!(a),
            ColEnc::I16(a) => int!(a),
            ColEnc::I32(a) => int!(a),
            ColEnc::I64(a) => int!(a),
            ColEnc::U8(a) => int!(a),
            ColEnc::U16(a) => int!(a),
            ColEnc::U32(a) => int!(a),
            ColEnc::U64(a) => int!(a),
            ColEnc::Dec128(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_str(buf, &a.value_as_string(row));
                }
            }
            ColEnc::Dec256(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_str(buf, &a.value_as_string(row));
                }
            }
            ColEnc::F32(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    let v = a.value(row);
                    if v.is_finite() {
                        buf.extend_from_slice(ryu_buf.format_finite(v).as_bytes());
                    } else {
                        buf.extend_from_slice(b"null");
                    }
                }
            }
            ColEnc::F64(a) => {
                if a.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    let v = a.value(row);
                    if v.is_finite() {
                        buf.extend_from_slice(ryu_buf.format_finite(v).as_bytes());
                    } else {
                        buf.extend_from_slice(b"null");
                    }
                }
            }
            ColEnc::Other(col) => {
                if col.is_null(row) {
                    buf.extend_from_slice(b"null");
                } else {
                    write_value(buf, *col, row);
                }
            }
        }
    }
}

#[inline]
fn write_value(buf: &mut Vec<u8>, col: &dyn Array, row: usize) {
    match col.data_type() {
        DataType::Utf8 => write_str(
            buf,
            col.as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(row),
        ),
        DataType::LargeUtf8 => write_str(
            buf,
            col.as_any()
                .downcast_ref::<LargeStringArray>()
                .unwrap()
                .value(row),
        ),
        DataType::Utf8View => write_str(
            buf,
            col.as_any()
                .downcast_ref::<StringViewArray>()
                .unwrap()
                .value(row),
        ),
        DataType::Dictionary(key, value)
            if matches!(key.as_ref(), DataType::Int32)
                && matches!(value.as_ref(), DataType::Utf8) =>
        {
            let dict = col
                .as_any()
                .downcast_ref::<arrow::array::DictionaryArray<arrow::datatypes::Int32Type>>()
                .unwrap();
            let keys = dict.keys();
            let values = dict
                .values()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let k = keys.value(row) as usize;
            write_str(buf, values.value(k));
        }
        DataType::Boolean => {
            let v = col
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .value(row);
            buf.extend_from_slice(if v { b"true" } else { b"false" });
        }
        DataType::Int8 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(col.as_any().downcast_ref::<Int8Array>().unwrap().value(row))
                    .as_bytes(),
            );
        }
        DataType::Int16 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<Int16Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::Int32 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<Int32Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::Int64 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<Int64Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::UInt8 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<UInt8Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::UInt16 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<UInt16Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::UInt32 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<UInt32Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::UInt64 => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(
                b.format(
                    col.as_any()
                        .downcast_ref::<UInt64Array>()
                        .unwrap()
                        .value(row),
                )
                .as_bytes(),
            );
        }
        DataType::Decimal128(_, _) => {
            let arr = col.as_any().downcast_ref::<Decimal128Array>().unwrap();
            write_str(buf, &arr.value_as_string(row));
        }
        DataType::Decimal256(_, _) => {
            let arr = col.as_any().downcast_ref::<Decimal256Array>().unwrap();
            write_str(buf, &arr.value_as_string(row));
        }
        DataType::Float32 => {
            let v = col
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .value(row);
            if v.is_finite() {
                let mut b = ryu::Buffer::new();
                buf.extend_from_slice(b.format_finite(v).as_bytes());
            } else {
                buf.extend_from_slice(b"null");
            }
        }
        DataType::Float64 => {
            let v = col
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(row);
            if v.is_finite() {
                let mut b = ryu::Buffer::new();
                buf.extend_from_slice(b.format_finite(v).as_bytes());
            } else {
                buf.extend_from_slice(b"null");
            }
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
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'\n' => buf.extend_from_slice(b"\\n"),
            b'\r' => buf.extend_from_slice(b"\\r"),
            b'\t' => buf.extend_from_slice(b"\\t"),
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

// ---------------------------------------------------------------------------
// Backend trait impl — wires the store into the generic core handlers.
// ---------------------------------------------------------------------------

#[async_trait]
impl Backend for Store {
    fn names(&self) -> Vec<String> {
        Store::names(self)
    }

    fn dataset_statuses(&self) -> Vec<datapress_core::backend::DatasetStatusEntry> {
        let status_snap = self.statuses.load();
        let ds_snap = self.datasets.load();
        let mut entries: Vec<_> = status_snap
            .iter()
            .map(|(name, (status, on_start))| {
                let (rows, lazy, columns) = if *status == DatasetStatus::Published {
                    if let Some(ds) = ds_snap.get(name) {
                        (ds.num_rows(), ds.lazy, ds.schema.columns.len())
                    } else {
                        (0, false, 0)
                    }
                } else {
                    (0, false, 0)
                };
                datapress_core::backend::DatasetStatusEntry {
                    name: name.clone(),
                    status: status.clone(),
                    on_start: on_start.clone(),
                    columns,
                    rows,
                    lazy,
                }
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        let st = self.dataset(name)?;
        Ok(DatasetSummary {
            name: st.schema.name.clone(),
            columns: st.schema.columns.len(),
            rows: st.num_rows(),
            lazy: st.lazy,
        })
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        let st = self.dataset(name)?;
        Ok(Arc::new(st.schema.clone()))
    }

    fn indexed_columns(&self, name: &str) -> Result<Vec<String>, AppError> {
        let st = self.dataset(name)?;
        // Report indexed columns in the dataset's declared schema order
        // so the `/schema` response is deterministic.
        let mut cols: Vec<String> = st
            .schema
            .columns
            .iter()
            .map(|c| c.name.clone())
            .filter(|n| st.index.contains_key(n))
            .collect();
        // Any indexed columns not in `schema.columns` (shouldn't happen,
        // but be defensive) get appended sorted.
        let mut extras: Vec<String> = st
            .index
            .keys()
            .filter(|n| !cols.iter().any(|c| c == *n))
            .cloned()
            .collect();
        extras.sort();
        cols.extend(extras);
        Ok(cols)
    }

    async fn sample(&self, name: &str) -> Result<String, AppError> {
        self.ensure_ready(name).await?;
        Store::sample(self, name).await
    }

    async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        self.ensure_ready(name).await?;
        Store::query(self, name, req).await
    }

    async fn query_arrow(&self, name: &str, req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        self.ensure_ready(name).await?;
        Store::query_arrow(self, name, req).await
    }

    async fn query_arrow_stream(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        self.ensure_ready(name).await?;
        Store::query_arrow_stream(self, name, req).await
    }

    async fn query_arrow_stream_all(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        self.ensure_ready(name).await?;
        Store::query_arrow_stream_all(self, name, req).await
    }

    async fn count(&self, name: &str, req: &CountRequest) -> Result<i64, AppError> {
        self.ensure_ready(name).await?;
        Store::count(self, name, req).await
    }

    async fn query_sql(
        &self,
        sql: &str,
        datasets: &[String],
        max_rows: u64,
    ) -> Result<String, AppError> {
        Store::query_sql(self, sql, datasets, max_rows).await
    }

    async fn query_sql_arrow_stream(
        &self,
        sql: &str,
        datasets: &[String],
        max_rows: u64,
    ) -> Result<ArrowIpcStream, AppError> {
        Store::query_sql_arrow_stream(self, sql, datasets, max_rows).await
    }

    async fn parquet(&self, name: &str) -> Result<bytes::Bytes, AppError> {
        self.ensure_ready(name).await?;
        Store::parquet(self, name).await
    }

    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        Store::reload(self, name).await
    }

    async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
        Store::try_reload(self, name).await
    }

    async fn register(&self, cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        Store::register(self, cfg).await
    }

    fn set_cascade_handle(&self, handle: CascadeHandle) {
        *self.cascade_handle.lock().unwrap() = Some(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::is_s3_access_denied;

    #[test]
    fn detects_s3_access_denied_variants() {
        // Representative messages from object_store / AWS / deltalake / MinIO.
        for msg in [
            "Generic S3 error: Error performing get request: response error \"<Error><Code>AccessDenied</Code></Error>\", status: 403",
            "Client error with status 403 Forbidden",
            "S3 error: Access Denied",
            "request failed: 403 Forbidden",
        ] {
            assert!(is_s3_access_denied(msg), "should flag: {msg}");
        }
    }

    #[test]
    fn ignores_unrelated_errors() {
        for msg in [
            "Not a Delta table: Generic delta kernel error: No files in log segment",
            "object at location data/part.parquet not found",
            "failed to infer parquet schema: invalid magic bytes",
        ] {
            assert!(!is_s3_access_denied(msg), "should not flag: {msg}");
        }
    }
}
