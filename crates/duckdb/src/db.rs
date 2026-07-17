use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};

use async_trait::async_trait;
use duckdb::Connection;

use datapress_core::backend::{
    ArrowIpcStream, Backend, CascadeHandle, DatasetStatus, DatasetStatusEntry, DatasetSummary,
    ReloadStats, arrow_ipc_stream_channel,
};
use datapress_core::config::{
    AddressingStyle, AppConfig, DatasetConfig, MaterializeResidency, OnStart, Partitioning,
    QuackConfig, SourceKind, StorageBackendKind,
};
use datapress_core::errors::AppError;
use datapress_core::models::{CountRequest, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};
use datapress_core::storage::{
    MaterializationStorage, build_materialization_storage, fnv1a_hash, gc_generations,
    list_complete_generations, new_ulid, now_rfc3339,
};

use crate::repository::DatasetRepository;

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------

pub struct DbPool {
    conns: Mutex<Vec<Connection>>,
    available: Condvar,
}

/// RAII guard — returns the connection to the pool on drop.
pub struct PooledConn {
    pool: Arc<DbPool>,
    conn: Option<Connection>,
}

impl std::ops::Deref for PooledConn {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.conns.lock().unwrap().push(conn);
            self.pool.available.notify_one();
        }
    }
}

impl DbPool {
    /// Check out a connection, blocking until one is available.
    pub fn get(pool: &Arc<Self>) -> PooledConn {
        let mut guard = pool.conns.lock().unwrap();
        loop {
            if let Some(conn) = guard.pop() {
                return PooledConn {
                    pool: Arc::clone(pool),
                    conn: Some(conn),
                };
            }
            guard = pool.available.wait(guard).unwrap();
        }
    }
}

pub type DbPoolRef = Arc<DbPool>;

// ---------------------------------------------------------------------------
// Registry — one schema per dataset, shared connection pool
// ---------------------------------------------------------------------------

pub struct Registry {
    pub pool: DbPoolRef,
    max_page_size: u64,
    /// Original dataset configs, indexed by name. Reload reads the source
    /// path from here — clients can't redirect a reload at an arbitrary file.
    /// Behind an `RwLock` so datasets registered at runtime can be added
    /// without a restart.
    configs: RwLock<HashMap<String, DatasetConfig>>,
    /// Hot-swappable schema map. `RwLock` is enough here: reads are very
    /// short (clone an `Arc`); writes happen only on reload.
    datasets: RwLock<HashMap<String, Arc<DatasetSchema>>>,
    /// Cached row counts per dataset, kept in lock-step with `datasets`.
    /// Populated at load and refreshed on reload — DuckDB's `count(*)`
    /// against a parquet file or native table is metadata-only and very
    /// cheap, but caching avoids repeating it for every `/api/datasets`
    /// listing call.
    row_counts: RwLock<HashMap<String, i64>>,
    /// Per-name reload mutex. Serialises concurrent reloads of the same
    /// dataset; reloads of different datasets proceed in parallel.
    reload_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-dataset lifecycle state (Pending / Building / Published / Failed)
    /// and startup policy. All configured datasets are present.
    statuses: RwLock<HashMap<String, (DatasetStatus, OnStart)>>,
    /// Cascade notification handle (R4.3). Set once by the server after
    /// building the cascade engine; `None` when no cascade is configured.
    cascade_handle: Mutex<Option<CascadeHandle>>,
    /// Phase 2B: optional server-level storage backend for query-dataset
    /// materialization. `None` → all query datasets stay in DuckDB memory.
    storage: Option<Arc<MaterializationStorage>>,
    /// T5.1 / T5.2: per-dataset refresh observability records.
    refresh_records: RwLock<HashMap<String, datapress_core::backend::RefreshRecord>>,
}

impl Registry {
    /// Resolve a dataset by name. Returns 404 on miss.
    pub fn get(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        self.datasets
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("dataset: {name}")))
    }

    pub fn names(&self) -> Vec<String> {
        let snap = self.datasets.read().unwrap();
        let mut v: Vec<String> = snap.keys().cloned().collect();
        v.sort();
        v
    }

    fn set_status(&self, name: &str, new_status: DatasetStatus) {
        if let Some(entry) = self.statuses.write().unwrap().get_mut(name) {
            entry.0 = new_status;
        }
    }

    /// Record a successful publish for `name`.
    fn record_publish(
        &self,
        name: &str,
        elapsed_ms: u128,
        source: datapress_core::backend::RefreshSource,
        generation_id: Option<String>,
    ) {
        use datapress_core::storage::now_rfc3339;
        let mut map = self.refresh_records.write().unwrap();
        let rec = map.entry(name.to_string()).or_default();
        rec.last_refresh_at = Some(now_rfc3339());
        rec.last_refresh_duration_ms = Some(elapsed_ms);
        rec.refresh_source = Some(source);
        rec.generation_id = generation_id;
        rec.consecutive_failures = 0;
        rec.last_error = None;
    }

    fn get_status_entry(&self, name: &str) -> Option<(DatasetStatus, OnStart)> {
        self.statuses.read().unwrap().get(name).cloned()
    }

    /// Ensure `name` is ready. Returns `NotReady` for non-published datasets
    /// (DuckDB doesn't support lazy first-touch in Phase 2A; all datasets are
    /// built at startup — if one is still Pending/Building it means the server
    /// hasn't finished its background startup yet).
    pub fn ensure_ready_sync(&self, name: &str) -> Result<(), AppError> {
        match self.get_status_entry(name) {
            Some((DatasetStatus::Published, _)) => Ok(()),
            Some((status, _)) => Err(AppError::NotReady {
                dataset: name.to_string(),
                state: format!("{status:?}").to_lowercase(),
            }),
            None => Err(AppError::NotFound(format!("dataset: {name}"))),
        }
    }

    /// Rebuild `name` from disk and atomically swap it in. DuckDB's
    /// `CREATE OR REPLACE TABLE` runs in a single transaction — in-flight
    /// SELECTs against the old table see snapshot-consistent data through
    /// MVCC, and the next query sees the new table.
    pub async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
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
        let _guard = lock.lock().await;

        self.reload_inner(name, &cfg).await
    }

    /// Like [`reload`] but skips if the per-dataset mutex is already held
    /// (returns `Ok(None)` — R3.2 coalescing). The caller holds the global
    /// refresh semaphore permit.
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

    /// Core reload logic. The caller must hold the per-dataset reload lock.
    ///
    /// DuckDB cancellation note (R3.3): `actix_web::web::block` runs on a
    /// dedicated blocking thread pool; there is no safe mid-operation cancel
    /// for DuckDB connections. When the scheduler's timeout fires, the
    /// scheduler records the failure and releases the semaphore permit, but
    /// the underlying blocking task continues until DuckDB returns. This is
    /// documented rather than leaking the permit (G-rule).
    async fn reload_inner(&self, name: &str, cfg: &DatasetConfig) -> Result<ReloadStats, AppError> {
        let started = std::time::Instant::now();
        // Capture the tokio clock at build start for cascade-clearing (R8.11).
        let build_start = tokio::time::Instant::now();
        self.set_status(name, DatasetStatus::Building);
        let pool = self.pool.clone();
        let cfg_clone = cfg.clone();
        let storage = self.storage.clone();

        let result = actix_web::web::block(
            move || -> Result<(DatasetSchema, i64, bool, bool), AppError> {
                let conn = DbPool::get(&pool);
                let (demoted, memory_override) =
                    replace_table(&conn, &cfg_clone, storage.as_deref())?;
                let schema = introspect_schema(&conn, &cfg_clone)?;
                let rows = count_rows(&conn, &cfg_clone.name)?;
                Ok((schema, rows, demoted, memory_override))
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?;

        let (schema, rows, demoted_to_storage, memory_override_exceeded) = match result {
            Ok(v) => v,
            Err(e) => {
                // Keep-last-good: revert status if we had a live generation.
                if self.datasets.read().unwrap().contains_key(name) {
                    self.set_status(name, DatasetStatus::Published);
                } else {
                    self.set_status(name, DatasetStatus::Failed);
                }
                return Err(e);
            }
        };

        self.datasets
            .write()
            .unwrap()
            .insert(name.to_string(), Arc::new(schema));
        self.row_counts
            .write()
            .unwrap()
            .insert(name.to_string(), rows);
        self.set_status(name, DatasetStatus::Published);

        let elapsed_ms = started.elapsed().as_millis();
        self.record_publish(
            name,
            elapsed_ms,
            datapress_core::backend::RefreshSource::Manual,
            None,
        );
        log::info!(
            "[publish] dataset='{}' trigger=manual rows={} elapsed_ms={}",
            name,
            rows,
            elapsed_ms
        );
        // R4.3: notify cascade engine of successful publish.
        // Pass build_start so the engine can clear stale cascade entries (R8.11).
        if let Some(h) = self.cascade_handle.lock().unwrap().as_ref() {
            h.notify_published_at(name, build_start);
        }
        Ok(ReloadStats {
            rows: rows as usize,
            elapsed_ms,
            demoted_to_storage,
            memory_override_exceeded,
        })
    }

    /// Register a brand-new dataset from `cfg` at runtime. Opens the source,
    /// creates the backing table (eager) or view (lazy), and inserts it into
    /// the registry so it is immediately queryable — no restart required.
    pub async fn register(&self, cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        cfg.validate_for_register()?;

        // Fast pre-check before taking the (async) per-name lock.
        if self.datasets.read().unwrap().contains_key(&cfg.name) {
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
        if self.datasets.read().unwrap().contains_key(&cfg.name) {
            return Err(AppError::InvalidValue(format!(
                "dataset '{}' already exists",
                cfg.name
            )));
        }

        let started = std::time::Instant::now();
        let pool = self.pool.clone();
        let build_cfg = cfg.clone();
        let storage = self.storage.clone();

        let (schema, rows) =
            actix_web::web::block(move || -> Result<(DatasetSchema, i64), AppError> {
                let conn = DbPool::get(&pool);
                // Load the extensions this source needs on demand. DuckDB
                // loads them into the shared database instance, so every
                // pooled connection sees them. Idempotent when cached.
                if build_cfg.source.is_s3() {
                    conn.execute_batch("INSTALL httpfs; LOAD httpfs;")?;
                    apply_s3_secret(&conn, &build_cfg)?;
                }
                if build_cfg.source.kind == SourceKind::Delta {
                    conn.execute_batch("INSTALL delta; LOAD delta;")?;
                }
                let schema = register_dataset(&conn, &build_cfg, storage.as_deref())?;
                let rows = count_rows(&conn, &build_cfg.name)?;
                Ok((schema, rows))
            })
            .await
            .map_err(|e| AppError::Internal(format!("join error: {e}")))??;

        let columns = schema.columns.len();
        self.datasets
            .write()
            .unwrap()
            .insert(cfg.name.clone(), Arc::new(schema));
        self.row_counts
            .write()
            .unwrap()
            .insert(cfg.name.clone(), rows);
        self.configs
            .write()
            .unwrap()
            .insert(cfg.name.clone(), cfg.clone());
        // Register status so subsequent queries can find the dataset via
        // ensure_ready_sync. DataFusion does this in its register impl;
        // this was missing from the DuckDB path (bug found by Phase 6A tests).
        self.statuses
            .write()
            .unwrap()
            .insert(cfg.name.clone(), (DatasetStatus::Published, OnStart::Eager));

        let elapsed_ms = started.elapsed().as_millis();
        self.record_publish(
            &cfg.name,
            elapsed_ms,
            datapress_core::backend::RefreshSource::Manual,
            None,
        );
        log::info!(
            "[publish] dataset='{}' trigger=manual rows={} elapsed_ms={}",
            cfg.name,
            rows,
            elapsed_ms
        );
        Ok(DatasetSummary {
            name: cfg.name,
            columns,
            rows: rows.max(0) as usize,
            lazy: cfg.lazy,
        })
    }

    /// Remove a runtime-managed dataset from the registry (Phase 6 R8.4).
    ///
    /// Drops the DuckDB table/view, removes from the schema map, row-counts
    /// map, configs map, and statuses. Returns `Err(NotFound)` if unknown;
    /// `Err(Forbidden)` if not managed.
    pub async fn unregister(&self, name: &str) -> Result<(), AppError> {
        // Check managed first.
        {
            let cfgs = self.configs.read().unwrap();
            match cfgs.get(name) {
                None => {
                    return Err(AppError::NotFound(format!("dataset '{name}' not found")));
                }
                Some(c) if !c.managed => {
                    return Err(AppError::Forbidden(format!(
                        "dataset '{name}' is not managed and cannot be unregistered"
                    )));
                }
                Some(_) => {}
            }
        }

        // Acquire the per-name lock.
        let lock = {
            let mut locks = self.reload_locks.lock().unwrap();
            locks
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        // Drop the table/view from DuckDB (best-effort; ignore errors).
        let pool = self.pool.clone();
        let name_owned = name.to_string();
        actix_web::web::block(move || {
            let conn = DbPool::get(&pool);
            // Try both TABLE and VIEW so we don't need to know which it is.
            let _ = conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS \"{name_owned}\"; DROP VIEW IF EXISTS \"{name_owned}\";"
            ));
        })
        .await
        .ok(); // ignore JoinError — the in-process state cleanup below still runs

        // Remove from in-memory maps.
        self.datasets.write().unwrap().remove(name);
        self.row_counts.write().unwrap().remove(name);
        self.configs.write().unwrap().remove(name);
        self.refresh_records.write().unwrap().remove(name);

        // Remove status.
        self.statuses.write().unwrap().remove(name);

        // Clean up the reload lock entry.
        self.reload_locks.lock().unwrap().remove(name);

        // GC storage generations (R8.4): remove the entire dataset directory
        // so no orphaned parquet files are left on disk after deletion.
        if let Some(ref storage) = self.storage
            && let Some(ref local_root) = storage.local_root
        {
            let ds_dir = local_root.join(name);
            if ds_dir.is_dir()
                && let Err(e) = std::fs::remove_dir_all(&ds_dir)
            {
                log::warn!("[unregister] storage GC failed for '{}': {e}", name);
            }
        }

        log::info!("[unregister] dataset='{}' removed", name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Startup: register every dataset (an in-memory table, or a streaming
// view when the dataset is configured `lazy`).
// ---------------------------------------------------------------------------

pub fn load_registry(cfg: &AppConfig) -> Result<Registry, AppError> {
    let conn = Connection::open_in_memory()?;

    // Install the extensions we'll need across the dataset list. Each
    // INSTALL is a no-op when the extension is already cached on disk;
    // the first run downloads from the DuckDB extension repo.
    let needs_httpfs = cfg.datasets.iter().any(|d| d.source.is_s3())
        || cfg
            .server
            .storage
            .as_ref()
            .is_some_and(|sc| sc.backend == StorageBackendKind::S3);
    let needs_delta = cfg
        .datasets
        .iter()
        .any(|d| d.source.kind == SourceKind::Delta);
    if needs_httpfs {
        log::info!("DuckDB: installing/loading httpfs extension (S3 support)");
        conn.execute_batch("INSTALL httpfs; LOAD httpfs;")?;
    }
    if needs_delta {
        log::info!("DuckDB: installing/loading delta extension");
        conn.execute_batch("INSTALL delta; LOAD delta;")?;
    }

    // Register a scoped SECRET per S3 dataset so different buckets / accounts
    // never clash. Secrets are scoped to the dataset's location prefix.
    for d in &cfg.datasets {
        if d.source.is_s3() {
            apply_s3_secret(&conn, d)?;
        }
    }

    // Phase 2B: build storage backend early so dataset builds can use it.
    // For S3: create a DuckDB SECRET scoped to the storage bucket once.
    let storage: Option<Arc<MaterializationStorage>> = if let Some(sc) = &cfg.server.storage {
        let stor = build_materialization_storage(sc).map(Arc::new)?;
        if sc.backend == StorageBackendKind::S3 {
            apply_storage_s3_secret(&conn, sc)?;
        }
        Some(stor)
    } else {
        None
    };

    let mut datasets = HashMap::new();
    let mut configs: HashMap<String, DatasetConfig> = cfg
        .datasets
        .iter()
        .map(|d| (d.name.clone(), d.clone()))
        .collect();
    let mut row_counts = HashMap::new();

    // Pre-populate statuses for ALL configured datasets (Published / Failed
    // are filled in below; start as Pending so the registry always has a
    // complete picture for /readyz and dataset listing).
    let mut statuses: HashMap<String, (DatasetStatus, OnStart)> = cfg
        .datasets
        .iter()
        .map(|d| (d.name.clone(), (DatasetStatus::Pending, d.on_start.clone())))
        .collect();

    // R4.2: track datasets that failed so their eager dependents can be
    // skipped without attempting a build.
    let mut failed_at_startup: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build datasets in topological dependency order (R2.4): query datasets
    // that depend on others must be registered after their dependencies.
    let build_order = cfg
        .topological_dataset_order()
        .map_err(|e| AppError::Internal(format!("startup order error: {e}")))?;

    for idx in build_order {
        let d = &cfg.datasets[idx];

        // R4.2: skip if any direct dependency failed (upstream_unavailable).
        let failed_dep = d
            .source
            .depends_on
            .iter()
            .find(|dep| failed_at_startup.contains(*dep))
            .cloned();
        if let Some(upstream) = failed_dep {
            log::warn!(
                "startup: skipping '{}' — upstream '{}' failed (upstream_unavailable)",
                d.name,
                upstream
            );
            if let Some(entry) = statuses.get_mut(&d.name) {
                entry.0 = DatasetStatus::Failed;
            }
            failed_at_startup.insert(d.name.clone());
            continue;
        }

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
        // Local sources only on the DuckDB backend — S3 sizing requires an
        // object-store client that lives in the DataFusion backend, so S3
        // datasets here are only forced lazy when explicitly configured.
        // query-kind datasets are never force-lazy (no file backing).
        let d: std::borrow::Cow<'_, DatasetConfig> = match d.force_lazy_bytes(&cfg.server) {
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
        let schema = match register_dataset(&conn, d, storage.as_deref()) {
            Ok(schema) => schema,
            Err(AppError::EmptyDataset(msg)) => {
                log::warn!("skipping empty dataset '{}': {msg}", d.name);
                if let Some(entry) = statuses.get_mut(&d.name) {
                    entry.0 = DatasetStatus::Failed;
                }
                failed_at_startup.insert(d.name.clone());
                continue;
            }
            Err(e) => {
                if d.source.kind == SourceKind::Query {
                    // Query datasets: log and continue with Failed status so
                    // the server can still start and serve other datasets.
                    log::error!("startup: failed to build query dataset '{}': {e}", d.name);
                    if let Some(entry) = statuses.get_mut(&d.name) {
                        entry.0 = DatasetStatus::Failed;
                    }
                    failed_at_startup.insert(d.name.clone());
                    continue;
                } else {
                    // Non-query datasets: fail startup on error.
                    return Err(e);
                }
            }
        };
        let rows = count_rows(&conn, &d.name)?;
        if d.source.kind == SourceKind::Query {
            log::info!(
                "  → {} columns ({} rows, query-materialised)",
                schema.columns.len(),
                rows
            );
        } else if d.lazy {
            log::info!(
                "  → {} columns ({} rows, lazy — streamed from source, not held in RAM)",
                schema.columns.len(),
                rows,
            );
        } else {
            log::info!(
                "  → {} columns ({} rows in-memory)",
                schema.columns.len(),
                rows
            );
        }
        datasets.insert(d.name.clone(), Arc::new(schema));
        // Update the effective config if lazy was forced.
        configs.insert(d.name.clone(), d.clone());
        row_counts.insert(d.name.clone(), rows);
        if let Some(entry) = statuses.get_mut(&d.name) {
            entry.0 = DatasetStatus::Published;
        }
    }

    if cfg.server.quack.enabled {
        start_quack_server(&conn, &cfg.server.quack)?;
    }

    let pool = init_pool(conn)?;

    // For S3 storage: create the httpfs SECRET on a pooled connection too,
    // since pooled connections are used by reload/register. Idempotent.
    if matches!(&cfg.server.storage, Some(sc) if sc.backend == StorageBackendKind::S3) {
        let sc = cfg.server.storage.as_ref().unwrap();
        let boot_conn = DbPool::get(&pool);
        apply_storage_s3_secret(&boot_conn, sc)?;
    }

    Ok(Registry {
        pool,
        max_page_size: cfg.server.max_page_size.max(1),
        configs: RwLock::new(configs),
        datasets: RwLock::new(datasets),
        row_counts: RwLock::new(row_counts),
        reload_locks: Mutex::new(HashMap::new()),
        statuses: RwLock::new(statuses),
        cascade_handle: Mutex::new(None),
        storage,
        refresh_records: RwLock::new(
            // Seed a RefreshRecord for every dataset that was published at
            // startup so last_refresh_at and X-Dataset-Refreshed-At work
            // from the first request.
            {
                use datapress_core::backend::{RefreshRecord, RefreshSource};
                use datapress_core::storage::now_rfc3339;
                let now = now_rfc3339();
                cfg.datasets
                    .iter()
                    .map(|d| {
                        (
                            d.name.clone(),
                            RefreshRecord {
                                last_refresh_at: Some(now.clone()),
                                last_refresh_duration_ms: Some(0),
                                refresh_source: Some(RefreshSource::Startup),
                                ..Default::default()
                            },
                        )
                    })
                    .collect()
            },
        ),
    })
}

fn start_quack_server(conn: &Connection, cfg: &QuackConfig) -> Result<(), AppError> {
    cfg.validate_enabled()?;
    log::warn!(
        "DuckDB Quack is experimental and exposes the DuckDB SQL surface; starting {}",
        cfg.uri
    );
    conn.execute_batch("INSTALL quack; LOAD quack;")?;

    if cfg.read_only {
        conn.execute_batch(
            "CREATE OR REPLACE MACRO datapress_quack_read_only(sid, query) AS \
             regexp_matches(upper(trim(query)), '^ATTACH\\s+''QUACK:') OR NOT regexp_matches(\
             upper(trim(query)),\
             '^(ATTACH|CREATE|INSERT|UPDATE|DELETE|COPY|DROP|ALTER|TRUNCATE|MERGE|VACUUM|EXPORT|IMPORT|LOAD|INSTALL)\\b'\
             );\
             SET GLOBAL quack_authorization_function = 'datapress_quack_read_only';",
        )?;
    }

    let uri = sql_string(&cfg.uri);
    let allow_other_hostname = if cfg.allow_other_hostname {
        "true"
    } else {
        "false"
    };
    let sql = match cfg.token.as_deref() {
        Some(token) => format!(
            "CALL quack_serve({uri}, token => {}, allow_other_hostname => {allow_other_hostname})",
            sql_string(token)
        ),
        None => format!("CALL quack_serve({uri}, allow_other_hostname => {allow_other_hostname})"),
    };

    let mut stmt = conn.prepare(&sql)?;
    let (listen_uri, http_url, auth_token): (String, String, String) =
        stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    if cfg.token.is_some() {
        log::info!("DuckDB Quack listening at {listen_uri} ({http_url})");
    } else {
        log::warn!(
            "DuckDB Quack listening at {listen_uri} ({http_url}); generated auth token: {auth_token}"
        );
    }
    Ok(())
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Build the `SELECT` source clause for `read_parquet(…)` or
/// `delta_scan(…)` from a dataset config. For local parquet this expands
/// to an explicit list of files (so DuckDB doesn't have to re-glob on
/// every reload); for S3 / Delta we pass the URL string through unchanged.
fn build_scan_clause(cfg: &DatasetConfig) -> Result<String, AppError> {
    match (cfg.source.kind, cfg.source.is_s3()) {
        (SourceKind::Parquet, false) => {
            let files = cfg.resolve_local_parquet_files()?;
            let file_list = files
                .iter()
                .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("read_parquet([{file_list}])"))
        }
        (SourceKind::Parquet, true) => {
            // DuckDB passes the URL straight to httpfs, so a bare prefix won't
            // expand on its own. Auto-append a recursive `**/*.parquet` glob for
            // plain prefixes (mirrors DataFusion's ListingTable behaviour);
            // configs that already carry a glob pass through unchanged.
            let loc = cfg.source.s3_recursive_parquet_glob().replace('\'', "''");
            let hive = match cfg.s3.as_ref().map(|s| s.partitioning).unwrap_or_default() {
                Partitioning::Hive => ", hive_partitioning => true",
                Partitioning::None => ", hive_partitioning => false",
                // Auto: let DuckDB infer from the path layout (its default).
                Partitioning::Auto => "",
            };
            Ok(format!("read_parquet('{loc}'{hive})"))
        }
        (SourceKind::Delta, _) => {
            let loc = cfg.source.location.replace('\'', "''");
            Ok(format!("delta_scan('{loc}')"))
        }
        // Query sources never go through build_scan_clause; they are handled
        // directly in register_dataset / replace_table.
        (SourceKind::Query, _) => Err(AppError::Internal(format!(
            "dataset '{}': build_scan_clause called on query-kind source",
            cfg.name
        ))),
    }
}

/// Issue a `CREATE OR REPLACE SECRET` for one S3 dataset. The secret is
/// scoped to the dataset bucket so globs, partitioned paths, and reloads all
/// match the same DuckDB secret without leaking across buckets.
fn apply_s3_secret(conn: &Connection, cfg: &DatasetConfig) -> Result<(), AppError> {
    let sql = build_s3_secret_sql(cfg)?;
    conn.execute_batch(&sql)?;
    Ok(())
}

fn build_s3_secret_sql(cfg: &DatasetConfig) -> Result<String, AppError> {
    let creds = cfg.resolved_creds();
    // If we have no explicit creds, leave DuckDB to use its own provider
    // chain (env, IMDS, ~/.aws/credentials). We still want region/endpoint
    // applied though, so we always emit a secret if non-credential S3
    // settings are present.
    let s3 = cfg.s3.clone().unwrap_or_default();
    let region = cfg.resolved_region();

    let mut parts: Vec<String> = vec!["TYPE s3".to_string()];
    if let (Some(k), Some(s)) = (
        creds.access_key_id.as_deref(),
        creds.secret_access_key.as_deref(),
    ) {
        parts.push("PROVIDER config".to_string());
        if let Some(ep) = s3.endpoint.as_deref().filter(|s| !s.is_empty()) {
            // DuckDB wants endpoint *without* the scheme.
            let bare = ep
                .trim_start_matches("http://")
                .trim_start_matches("https://");
            parts.push(format!("ENDPOINT '{}'", bare.replace('\'', "''")));
        }
        parts.push(format!("KEY_ID '{}'", k.replace('\'', "''")));
        parts.push(format!("SECRET '{}'", s.replace('\'', "''")));
        if let Some(t) = creds.session_token.as_deref() {
            parts.push(format!("SESSION_TOKEN '{}'", t.replace('\'', "''")));
        }
    } else if creds.access_key_id.is_some() || creds.secret_access_key.is_some() {
        return Err(AppError::Internal(format!(
            "dataset '{}': partial S3 credentials — need both access_key_id and secret_access_key",
            cfg.name
        )));
    } else {
        // No explicit keys — ask DuckDB to use env/profile credentials.
        // Avoid instance-metadata probing by default; that path can surface
        // as a confusing 503 on local machines and many S3-compatible stores.
        parts.push("PROVIDER credential_chain".to_string());
        parts.push("CHAIN 'env;config'".to_string());
        if let Some(ep) = s3.endpoint.as_deref().filter(|s| !s.is_empty()) {
            // DuckDB wants endpoint *without* the scheme.
            let bare = ep
                .trim_start_matches("http://")
                .trim_start_matches("https://");
            parts.push(format!("ENDPOINT '{}'", bare.replace('\'', "''")));
        }
    }
    parts.push(format!("REGION '{}'", region.replace('\'', "''")));
    parts.push(format!(
        "URL_STYLE '{}'",
        duckdb_s3_url_style(s3.addressing_style)
    ));
    parts.push(format!(
        "USE_SSL {}",
        if s3.allow_http { "false" } else { "true" }
    ));
    parts.push(format!(
        "SCOPE '{}'",
        s3_secret_scope(cfg)?.replace('\'', "''")
    ));

    // Secret name: dataset name normalised. DuckDB identifiers are
    // case-insensitive and accept alphanum + underscore.
    let secret_name = format!(
        "ds_{}",
        cfg.name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    );
    let sql = format!(
        "CREATE OR REPLACE SECRET {secret_name} ({});",
        parts.join(", ")
    );
    Ok(sql)
}

fn s3_secret_scope(cfg: &DatasetConfig) -> Result<String, AppError> {
    let (bucket, _) = cfg.source.s3_bucket()?;
    Ok(format!("s3://{bucket}"))
}

fn duckdb_s3_url_style(style: AddressingStyle) -> &'static str {
    match style {
        AddressingStyle::Virtual => "vhost",
        AddressingStyle::Path => "path",
    }
}

/// Atomically replace the dataset's table by re-reading its source.
/// `CREATE OR REPLACE TABLE ... AS SELECT ...` is a single DuckDB transaction:
/// if the source read fails, the existing table is preserved.
fn replace_table(
    conn: &Connection,
    cfg: &DatasetConfig,
    storage: Option<&MaterializationStorage>,
) -> Result<(bool, bool), AppError> {
    // Returns (demoted_to_storage, memory_override_exceeded).
    let table = DatasetSchema::quote_ident(&cfg.name);
    if cfg.source.kind == SourceKind::Query {
        let sql = cfg.source.sql.as_deref().ok_or_else(|| {
            AppError::Internal(format!(
                "dataset '{}': source.sql missing for kind = query",
                cfg.name
            ))
        })?;
        // Phase 2B: try storage path first.
        let (schema_opt, demoted, _override_ignored) =
            register_query_with_storage(conn, cfg, sql, storage)?;
        if schema_opt.is_some() {
            return Ok((demoted, false));
        }
        // In-memory replace with optional ORDER BY for sort_by (R2B.5).
        let order_by: String = cfg
            .materialize
            .as_ref()
            .map(|m| &m.sort_by)
            .filter(|v| !v.is_empty())
            .map(|cols| {
                format!(
                    " ORDER BY {}",
                    cols.iter()
                        .map(|c| DatasetSchema::quote_ident(c))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_default();
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TABLE {table} AS {sql}{order_by};"
        ))?;
        // R2B.1: check if residency=memory result exceeds force_lazy_above_mb.
        let memory_override_exceeded = check_memory_override(conn, cfg, &table, storage);
        return Ok((false, memory_override_exceeded));
    }
    let scan = build_scan_clause(cfg)?;
    // Lazy datasets are views over the source scan, so a reload just
    // re-points the view; eager datasets re-materialise into a table.
    let relation = if cfg.lazy { "VIEW" } else { "TABLE" };
    conn.execute_batch(&format!(
        "CREATE OR REPLACE {relation} {table} AS SELECT * FROM {scan};"
    ))?;
    Ok((false, false))
}

/// Register the source as a queryable relation named `cfg.name` and
/// introspect its schema via DuckDB's `DESCRIBE`.
///
/// For `kind = "query"` datasets with `residency = lazy` or auto-demotion,
/// the result is written to parquet files on the storage backend and served
/// via a `CREATE VIEW … AS SELECT * FROM read_parquet(...)`.
fn register_dataset(
    conn: &Connection,
    cfg: &DatasetConfig,
    storage: Option<&MaterializationStorage>,
) -> Result<DatasetSchema, AppError> {
    let table = DatasetSchema::quote_ident(&cfg.name);
    if cfg.source.kind == SourceKind::Query {
        let sql = cfg.source.sql.as_deref().ok_or_else(|| {
            AppError::Internal(format!(
                "dataset '{}': source.sql missing for kind = query",
                cfg.name
            ))
        })?;
        let (schema_opt, _demoted, _override) =
            register_query_with_storage(conn, cfg, sql, storage)?;
        if let Some(schema) = schema_opt {
            return Ok(schema);
        }
        // In-memory path (residency = memory or auto without storage).
        // Apply sort_by ORDER BY for the memory case too (R2B.5).
        let order_by: String = cfg
            .materialize
            .as_ref()
            .map(|m| &m.sort_by)
            .filter(|v| !v.is_empty())
            .map(|cols| {
                format!(
                    " ORDER BY {}",
                    cols.iter()
                        .map(|c| DatasetSchema::quote_ident(c))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_default();
        conn.execute_batch(&format!("CREATE TABLE {table} AS {sql}{order_by};"))
            .map_err(|e| {
                AppError::Internal(format!("dataset '{}': query execute: {e}", cfg.name))
            })?;
        // R2B.1 override check (startup; flags are discarded — startup doesn't emit ReloadStats).
        check_memory_override(conn, cfg, &table, storage);
        return introspect_schema(conn, cfg);
    }
    let scan = build_scan_clause(cfg)?;
    let relation = if cfg.lazy { "VIEW" } else { "TABLE" };
    conn.execute_batch(&format!(
        "CREATE {relation} {table} AS SELECT * FROM {scan};"
    ))
    .map_err(|e| classify_scan_error(cfg, e))?;
    introspect_schema(conn, cfg)
}

/// R2B.1: check if a `residency = memory` query result exceeds `force_lazy_above_mb`.
/// Emits a WARN log and returns `true` when the threshold is crossed.
/// The caller is responsible for incrementing the metric counter.
fn check_memory_override(
    conn: &Connection,
    cfg: &DatasetConfig,
    table: &str,
    storage: Option<&MaterializationStorage>,
) -> bool {
    let Some(stor) = storage else {
        return false;
    };
    let threshold = stor.config.force_lazy_above_mb.saturating_mul(1024 * 1024);
    if cfg
        .materialize
        .as_ref()
        .is_none_or(|m| m.residency != MaterializeResidency::Memory)
    {
        return false;
    }
    let estimated = estimated_table_bytes(conn, table);
    if estimated > threshold {
        log::warn!(
            "dataset '{}': materialized result ({} MiB) exceeds force_lazy_above_mb \
             = {} but residency = memory overrides demotion",
            cfg.name,
            estimated / (1024 * 1024),
            stor.config.force_lazy_above_mb,
        );
        return true;
    }
    false
}

/// Phase 2B: Try to register a query dataset with storage spill (R2B.3).
///
/// Returns `Some(schema)` when the lazy/storage path was taken, `None` to
/// fall through to the in-memory path.
///
/// Handles both `local` and `s3` storage backends, and `auto` residency
/// by measuring the built temp-table size via `duckdb_tables()` (R2B.3).
fn register_query_with_storage(
    conn: &Connection,
    cfg: &DatasetConfig,
    sql: &str,
    storage: Option<&MaterializationStorage>,
) -> Result<(Option<DatasetSchema>, bool, bool), AppError> {
    // Returns (schema, demoted_to_storage, memory_override_exceeded).
    let residency = cfg
        .materialize
        .as_ref()
        .map(|m| m.residency)
        .unwrap_or(MaterializeResidency::Auto);

    // Apply sort_by ORDER BY (R2B.5).
    let sort_by: Vec<String> = cfg
        .materialize
        .as_ref()
        .map(|m| m.sort_by.clone())
        .unwrap_or_default();
    let order_by_clause: String = if sort_by.is_empty() {
        String::new()
    } else {
        format!(
            " ORDER BY {}",
            sort_by
                .iter()
                .map(|c| DatasetSchema::quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    // For lazy residency: storage is required.
    if residency == MaterializeResidency::Lazy && storage.is_none() {
        return Err(AppError::Internal(format!(
            "dataset '{}': residency = lazy requires [server.storage]",
            cfg.name
        )));
    }
    // Memory: no storage path — caller handles in-memory and checks for override.
    if residency == MaterializeResidency::Memory {
        return Ok((None, false, false));
    }
    // Auto without storage: fall through to in-memory.
    if residency == MaterializeResidency::Auto && storage.is_none() {
        return Ok((None, false, false));
    }

    let stor = match storage {
        Some(s) => s,
        None => return Ok((None, false, false)),
    };

    let gen_id = new_ulid();

    // Tmp table name: unique per build so concurrent calls don't collide.
    let tmp_name = format!(
        "__dp_tmp_{}_{}",
        cfg.name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>(),
        &gen_id[..8]
    );
    let tmp_table = DatasetSchema::quote_ident(&tmp_name);

    // Build into a temp table first.
    conn.execute_batch(&format!(
        "CREATE TABLE {tmp_table} AS {sql}{order_by_clause};"
    ))
    .map_err(|e| AppError::Internal(format!("dataset '{}': query execute: {e}", cfg.name)))?;

    // Auto residency: measure the estimated size and decide.
    let use_storage = match residency {
        MaterializeResidency::Lazy => true,
        MaterializeResidency::Auto => {
            let threshold = stor.config.force_lazy_above_mb.saturating_mul(1024 * 1024);
            let estimated = estimated_table_bytes(conn, &tmp_name);
            if estimated > threshold {
                log::info!(
                    "dataset '{}': auto-demoting to storage (estimated {} MiB > {} MiB threshold)",
                    cfg.name,
                    estimated / (1024 * 1024),
                    stor.config.force_lazy_above_mb,
                );
                true
            } else {
                false
            }
        }
        MaterializeResidency::Memory => false, // already returned above
    };

    if !use_storage {
        // Rename temp table to the real name (stays in memory as engine table).
        let table = DatasetSchema::quote_ident(&cfg.name);
        // CREATE OR REPLACE semantics: drop old table/view if it exists, then rename.
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {table}; DROP VIEW IF EXISTS {table};"
        ))
        .ok();
        conn.execute_batch(&format!("ALTER TABLE {tmp_table} RENAME TO {table};"))
            .map_err(|e| AppError::Internal(format!("dataset '{}': rename: {e}", cfg.name)))?;
        // Stayed in memory (auto below threshold): demoted=false.
        return introspect_schema(conn, cfg).map(|s| (Some(s), false, false));
    }

    // Spill to storage.
    let rows = count_rows(conn, &tmp_name).unwrap_or(0) as u64;

    let (parquet_dest, view_glob) = storage_paths(stor, &cfg.name, &gen_id);

    conn.execute_batch(&format!(
        "COPY {tmp_table} TO '{parquet_dest}' (FORMAT PARQUET);"
    ))
    .map_err(|e| AppError::Internal(format!("dataset '{}': COPY TO storage: {e}", cfg.name)))?;

    // Drop temp table — parquet file is the durable copy.
    conn.execute_batch(&format!("DROP TABLE IF EXISTS {tmp_table};"))
        .ok();

    // Create view over the parquet files.
    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW {table} AS SELECT * FROM read_parquet('{view_glob}');"
    ))
    .map_err(|e| AppError::Internal(format!("dataset '{}': create view: {e}", cfg.name)))?;

    let schema = introspect_schema(conn, cfg)?;

    // Write manifest (atomicity seal). For S3, use tokio runtime to call async put.
    let sql_hash = fnv1a_hash(sql);
    let byte_size = manifest_byte_size(stor, &cfg.name, &gen_id);
    let manifest = datapress_core::storage::GenerationManifest {
        sql_hash,
        schema_hash: fnv1a_hash(
            &schema
                .columns
                .iter()
                .map(|c| format!("{}:{}", c.name, c.sql_type))
                .collect::<Vec<_>>()
                .join(","),
        ),
        row_count: rows,
        byte_size,
        created_at: now_rfc3339(),
        files: vec!["data-0.parquet".to_string()],
    };
    write_manifest_for_storage(stor, &cfg.name, &gen_id, &manifest)?;

    // GC old generations: keep current + previous (N-2 rule).
    gc_storage(stor, &cfg.name);

    log::info!(
        "dataset '{}' [query, lazy/storage]: {} rows, gen {}",
        cfg.name,
        rows,
        gen_id
    );

    // demoted_to_storage = true for both Lazy and Auto-over-threshold.
    Ok((Some(schema), true, false))
}

/// Compute the COPY-TO path and the view glob for a generation.
/// Returns `(copy_to_path, view_glob)`.
fn storage_paths(stor: &MaterializationStorage, dataset: &str, gen_id: &str) -> (String, String) {
    match &stor.s3_bucket {
        Some(bucket) => {
            let prefix = if stor.root_prefix.is_empty() {
                format!("{dataset}/{gen_id}")
            } else {
                format!("{}/{dataset}/{gen_id}", stor.root_prefix)
            };
            let copy_to = format!("s3://{bucket}/{prefix}/data-0.parquet");
            let view_glob = format!("s3://{bucket}/{prefix}/*.parquet");
            (copy_to, view_glob)
        }
        None => {
            let local = stor
                .local_root
                .as_deref()
                .unwrap_or(std::path::Path::new(&stor.config.root));
            let gen_dir = local.join(dataset).join(gen_id);
            std::fs::create_dir_all(&gen_dir).ok();
            let parquet = gen_dir.join("data-0.parquet");
            let glob = gen_dir
                .join("*.parquet")
                .display()
                .to_string()
                .replace('\'', "''");
            (parquet.display().to_string().replace('\'', "''"), glob)
        }
    }
}

/// Approximate parquet file byte size for the manifest. Local: stat; S3: 0 (unknown pre-write).
fn manifest_byte_size(stor: &MaterializationStorage, dataset: &str, gen_id: &str) -> u64 {
    if let Some(ref local_root) = stor.local_root {
        let p = local_root.join(dataset).join(gen_id).join("data-0.parquet");
        p.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0 // S3 size not known synchronously post-COPY
    }
}

/// Write manifest. Local: filesystem. S3: tokio block_on.
fn write_manifest_for_storage(
    stor: &MaterializationStorage,
    dataset: &str,
    gen_id: &str,
    manifest: &datapress_core::storage::GenerationManifest,
) -> Result<(), AppError> {
    if let Some(ref local_root) = stor.local_root {
        let gen_dir = datapress_core::storage::generation_dir(local_root, dataset, gen_id);
        std::fs::create_dir_all(&gen_dir).ok();
        manifest
            .write(&gen_dir)
            .map_err(|e| AppError::Internal(format!("dataset '{dataset}': write manifest: {e}")))
    } else {
        // S3: must run async in a blocking context. Use tokio's block_on.
        let rt = tokio::runtime::Handle::current();
        let stor_clone = stor.object_store.clone();
        let path = stor.obj_path(dataset, gen_id, "manifest.json");
        let json = serde_json::to_vec_pretty(manifest).map_err(|e| {
            AppError::Internal(format!("dataset '{dataset}': manifest serialize: {e}"))
        })?;
        rt.block_on(async {
            use object_store::ObjectStoreExt;
            stor_clone
                .put(&path, object_store::PutPayload::from(json))
                .await
                .map_err(|e| {
                    AppError::Internal(format!("dataset '{dataset}': write manifest to S3: {e}"))
                })
        })
        .map(|_| ())
    }
}

/// GC old generations after a successful publish.
fn gc_storage(stor: &MaterializationStorage, dataset: &str) {
    if let Some(ref local_root) = stor.local_root {
        let gens = list_complete_generations(local_root, dataset);
        let keep_ids: Vec<&str> = gens
            .iter()
            .rev()
            .take(2)
            .map(|(id, _, _)| id.as_str())
            .collect();
        gc_generations(local_root, dataset, &keep_ids);
    } else {
        // S3 GC: fire-and-forget via tokio background task.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let obj = stor.object_store.clone();
            let dataset = dataset.to_string();
            let root_prefix = stor.root_prefix.clone();
            // Detach: GC failure is non-fatal; errors are logged inside.
            drop(rt.spawn(async move {
                gc_s3_generations_inner(&obj, &dataset, &root_prefix).await;
            }));
        }
    }
}

async fn gc_s3_generations_inner(
    store: &Arc<dyn object_store::ObjectStore>,
    dataset: &str,
    root_prefix: &str,
) {
    use futures_util::StreamExt;
    use object_store::ObjectStoreExt;
    let prefix_str = if root_prefix.is_empty() {
        format!("{dataset}/")
    } else {
        format!("{root_prefix}/{dataset}/")
    };
    let prefix = object_store::path::Path::from(prefix_str);
    let listed = match store.list_with_delimiter(Some(&prefix)).await {
        Ok(l) => l,
        Err(_) => return,
    };
    // Collect gen_ids that have manifests.
    let mut gen_ids_with_manifests: Vec<String> = Vec::new();
    for cp in &listed.common_prefixes {
        let full = cp.to_string();
        let gen_id = full
            .trim_end_matches('/')
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string();
        // Check if manifest exists by trying a HEAD.
        let manifest_path = if root_prefix.is_empty() {
            object_store::path::Path::from(format!("{dataset}/{gen_id}/manifest.json"))
        } else {
            object_store::path::Path::from(format!(
                "{root_prefix}/{dataset}/{gen_id}/manifest.json"
            ))
        };
        if store.head(&manifest_path).await.is_ok() {
            gen_ids_with_manifests.push(gen_id);
        }
    }
    gen_ids_with_manifests.sort();
    let keep: std::collections::HashSet<String> = gen_ids_with_manifests
        .iter()
        .rev()
        .take(2)
        .cloned()
        .collect();
    // Delete generations not in keep.
    for cp in &listed.common_prefixes {
        let full = cp.to_string();
        let gen_id = full
            .trim_end_matches('/')
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string();
        if keep.contains(&gen_id) {
            continue;
        }
        let gen_prefix = object_store::path::Path::from(full.trim_end_matches('/').to_string());
        let mut objects = store.list(Some(&gen_prefix));
        while let Some(item) = objects.next().await {
            if let Ok(meta) = item {
                let _ = store.delete(&meta.location).await;
            }
        }
    }
}

/// Query DuckDB's `duckdb_tables()` view for the estimated_size of a table.
/// Returns 0 if the table is not found or the query fails.
fn estimated_table_bytes(conn: &Connection, table_name: &str) -> u64 {
    let escaped = table_name.replace('\'', "''");
    let sql = format!(
        "SELECT coalesce(estimated_size, 0) FROM duckdb_tables() WHERE table_name = '{escaped}'"
    );
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map(|v| v.max(0) as u64)
        .unwrap_or(0)
}

/// Build a DuckDB SECRET for the server-level storage S3 backend.
/// Called once at startup; the secret is scoped to the storage bucket.
/// Values are read from env vars (R2B.7); nothing is logged.
fn apply_storage_s3_secret(
    conn: &Connection,
    sc: &datapress_core::config::StorageConfig,
) -> Result<(), AppError> {
    let creds = sc.s3.resolved_creds()?;
    let (bucket, _) = sc
        .root
        .strip_prefix("s3://")
        .unwrap_or("")
        .split_once('/')
        .unwrap_or(("", ""));
    if bucket.is_empty() {
        return Err(AppError::Internal(
            "server.storage: S3 root must start with s3://<bucket>/".into(),
        ));
    }

    let mut parts: Vec<String> = vec!["TYPE s3".to_string()];
    if let (Some(k), Some(s)) = (
        creds.access_key_id.as_deref(),
        creds.secret_access_key.as_deref(),
    ) {
        parts.push("PROVIDER config".to_string());
        if let Some(ep) = sc.s3.endpoint.as_deref().filter(|e| !e.is_empty()) {
            let bare = ep
                .trim_start_matches("http://")
                .trim_start_matches("https://");
            parts.push(format!("ENDPOINT '{}'", bare.replace('\'', "''")));
        }
        // Values come from env vars — not logged.
        parts.push(format!("KEY_ID '{}'", k.replace('\'', "''")));
        parts.push(format!("SECRET '{}'", s.replace('\'', "''")));
    } else {
        parts.push("PROVIDER credential_chain".to_string());
        parts.push("CHAIN 'env;config'".to_string());
        if let Some(ep) = sc.s3.endpoint.as_deref().filter(|e| !e.is_empty()) {
            let bare = ep
                .trim_start_matches("http://")
                .trim_start_matches("https://");
            parts.push(format!("ENDPOINT '{}'", bare.replace('\'', "''")));
        }
    }
    if let Some(r) = &sc.s3.region {
        parts.push(format!("REGION '{}'", r.replace('\'', "''")));
    }
    parts.push(format!(
        "URL_STYLE '{}'",
        duckdb_s3_url_style(sc.s3.addressing_style)
    ));
    parts.push(format!(
        "USE_SSL {}",
        if sc.s3.allow_http { "false" } else { "true" }
    ));
    parts.push(format!("SCOPE 's3://{}'", bucket.replace('\'', "''")));

    let sql = format!(
        "CREATE OR REPLACE SECRET __dp_storage ({});",
        parts.join(", ")
    );
    conn.execute_batch(&sql)?;
    Ok(())
}

/// Classify a DuckDB source-scan failure. An S3 / glob source that matches
/// no files surfaces as an "IO Error: No files found …" — that means the
/// dataset is currently empty, which the load loop logs and skips rather
/// than treating as fatal. Everything else stays an internal error.
fn classify_scan_error(cfg: &DatasetConfig, e: duckdb::Error) -> AppError {
    let msg = e.to_string();
    if msg.to_lowercase().contains("no files found") {
        AppError::EmptyDataset(format!(
            "dataset '{}': source matched no files: {}",
            cfg.name, cfg.source.location
        ))
    } else {
        AppError::Internal(msg)
    }
}

fn introspect_schema(conn: &Connection, cfg: &DatasetConfig) -> Result<DatasetSchema, AppError> {
    let table = &cfg.name;
    let mut stmt = conn.prepare(&format!("DESCRIBE {}", DatasetSchema::quote_ident(table)))?;
    let rows = stmt.query_map([], |row| {
        // DESCRIBE columns: column_name, column_type, null, key, default, extra
        let name: String = row.get(0)?;
        let sql_type: String = row.get(1)?;
        let nullable: String = row.get::<_, String>(2).unwrap_or_else(|_| "YES".into());
        Ok((name, sql_type, nullable))
    })?;

    let columns = rows
        .map(|r| {
            r.map(|(name, sql_type, nullable)| ColumnInfo {
                logical: classify_duckdb_type(&sql_type),
                sql_type,
                nullable: nullable.eq_ignore_ascii_case("YES"),
                name,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    DatasetSchema::new(table, columns)
        .with_filters(cfg.predicate_filter.clone(), cfg.projection_filter.clone())
}

fn classify_duckdb_type(sql_type: &str) -> LogicalType {
    // DuckDB type strings: TINYINT, SMALLINT, INTEGER, BIGINT, HUGEINT,
    // UTINYINT…, FLOAT, DOUBLE, DECIMAL(.., ..), VARCHAR, TEXT, BOOLEAN,
    // DATE, TIME, TIMESTAMP, TIMESTAMP_S, TIMESTAMP_NS, TIMESTAMPTZ, …
    let t = sql_type.to_ascii_uppercase();
    if t.starts_with("BOOL") {
        LogicalType::Bool
    } else if t == "FLOAT" || t == "DOUBLE" || t == "REAL" || t.starts_with("DECIMAL") {
        LogicalType::Float
    } else if t.ends_with("INT") || t.starts_with("UINT") || t == "HUGEINT" {
        LogicalType::Int
    } else if t == "VARCHAR"
        || t == "TEXT"
        || t == "STRING"
        || t == "CHAR"
        || t.starts_with("VARCHAR(")
    {
        LogicalType::Utf8
    } else if t.starts_with("TIMESTAMP") || t == "DATE" || t == "TIME" || t.starts_with("INTERVAL")
    {
        LogicalType::Temporal
    } else {
        LogicalType::Other
    }
}

fn count_rows(conn: &Connection, table: &str) -> Result<i64, AppError> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM {}", DatasetSchema::quote_ident(table)),
        [],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------------------
// Pool construction (unchanged behaviour)
// ---------------------------------------------------------------------------

fn init_pool(conn: Connection) -> Result<DbPoolRef, AppError> {
    let size = std::env::var("DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });

    let total_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let threads_per_conn = (total_cpus / size).max(1);
    conn.execute_batch(&format!("SET threads={threads_per_conn};"))?;
    log::info!(
        "Connection pool: {size} conns × {threads_per_conn} DuckDB threads (total CPUs: {total_cpus})"
    );

    let mut conns = Vec::with_capacity(size);
    for _ in 0..size {
        conns.push(conn.try_clone()?);
    }
    Ok(Arc::new(DbPool {
        conns: Mutex::new(conns),
        available: Condvar::new(),
    }))
}

// ---------------------------------------------------------------------------
// Backend trait impl — wires the registry into the generic core handlers.
// ---------------------------------------------------------------------------

#[async_trait]
impl Backend for Registry {
    fn names(&self) -> Vec<String> {
        Registry::names(self)
    }

    fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
        let row_counts = self.row_counts.read().unwrap();
        let cfgs = self.configs.read().unwrap();
        let recs = self.refresh_records.read().unwrap();
        let mut entries: Vec<_> = self
            .statuses
            .read()
            .unwrap()
            .iter()
            .map(|(name, (status, on_start))| {
                let (rows, lazy, columns) = if *status == DatasetStatus::Published {
                    let rows = row_counts.get(name).copied().unwrap_or(0).max(0) as usize;
                    let lazy = cfgs.get(name).map(|c| c.lazy).unwrap_or(false);
                    let columns = self
                        .datasets
                        .read()
                        .unwrap()
                        .get(name)
                        .map(|s| s.columns.len())
                        .unwrap_or(0);
                    (rows, lazy, columns)
                } else {
                    (0, false, 0)
                };
                let rec = recs.get(name).cloned().unwrap_or_default();
                let cfg = cfgs.get(name);
                let kind = cfg
                    .map(|c| c.source.kind.as_str().to_string())
                    .unwrap_or_else(|| "parquet".into());
                let depends_on = cfg.map(|c| c.source.depends_on.clone()).unwrap_or_default();
                let residency = if lazy { "lazy" } else { "memory" };
                DatasetStatusEntry {
                    name: name.clone(),
                    status: status.clone(),
                    on_start: on_start.clone(),
                    kind,
                    residency: residency.into(),
                    storage_bytes: None,
                    generation_id: rec.generation_id,
                    last_refresh_at: rec.last_refresh_at,
                    last_refresh_duration_ms: rec.last_refresh_duration_ms,
                    next_refresh_at: rec.next_refresh_at,
                    refresh_source: rec.refresh_source,
                    consecutive_failures: rec.consecutive_failures,
                    last_error: rec.last_error,
                    columns,
                    rows,
                    lazy,
                    depends_on,
                }
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn refresh_record(&self, name: &str) -> Option<datapress_core::backend::RefreshRecord> {
        self.refresh_records.read().unwrap().get(name).cloned()
    }

    fn record_refresh(&self, name: &str, record: datapress_core::backend::RefreshRecord) {
        let mut map = self.refresh_records.write().unwrap();
        let existing = map.entry(name.to_string()).or_default();
        existing.consecutive_failures = record.consecutive_failures;
        existing.last_error = record.last_error;
        if record.next_refresh_at.is_some() {
            existing.next_refresh_at = record.next_refresh_at;
        }
        if record.refresh_source.is_some() {
            existing.refresh_source = record.refresh_source;
        }
        if record.last_refresh_at.is_some() {
            existing.last_refresh_at = record.last_refresh_at;
        }
        if record.last_refresh_duration_ms.is_some() {
            existing.last_refresh_duration_ms = record.last_refresh_duration_ms;
        }
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        self.ensure_ready_sync(name)?;
        let schema = self.get(name)?;
        let rows = self
            .row_counts
            .read()
            .unwrap()
            .get(name)
            .copied()
            .unwrap_or(0);
        let lazy = self
            .configs
            .read()
            .unwrap()
            .get(name)
            .map(|c| c.lazy)
            .unwrap_or(false);
        Ok(DatasetSummary {
            name: schema.name.clone(),
            columns: schema.columns.len(),
            rows: rows.max(0) as usize,
            lazy,
        })
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        self.ensure_ready_sync(name)?;
        self.get(name)
    }

    async fn sample(&self, name: &str) -> Result<String, AppError> {
        self.ensure_ready_sync(name)?;
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let max_page_size = self.max_page_size;
        actix_web::web::block(move || -> Result<String, AppError> {
            let conn = DbPool::get(&pool);
            DatasetRepository::new(&conn, &schema, max_page_size).sample()
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?
    }

    async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError> {
        self.ensure_ready_sync(name)?;
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let req = req.clone();
        let max_page_size = self.max_page_size;
        actix_web::web::block(move || -> Result<String, AppError> {
            let conn = DbPool::get(&pool);
            DatasetRepository::new(&conn, &schema, max_page_size).query(&req)
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?
    }

    async fn query_arrow(&self, name: &str, req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let req = req.clone();
        let max_page_size = self.max_page_size;
        actix_web::web::block(move || -> Result<Vec<u8>, AppError> {
            let conn = DbPool::get(&pool);
            DatasetRepository::new(&conn, &schema, max_page_size).query_arrow_bytes(&req)
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?
    }

    async fn query_arrow_stream(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let req = req.clone();
        let max_page_size = self.max_page_size;
        let (mut writer, stream) = arrow_ipc_stream_channel(8);

        tokio::task::spawn_blocking(move || {
            let result = {
                let conn = DbPool::get(&pool);
                DatasetRepository::new(&conn, &schema, max_page_size)
                    .query_arrow_write(&req, &mut writer)
            };
            if let Err(err) = result {
                log::error!("duckdb arrow stream failed: {err}");
                writer.send_error(err);
            }
        });

        Ok(stream)
    }

    async fn query_arrow_stream_all(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let req = req.clone();
        let max_page_size = self.max_page_size;
        let (mut writer, stream) = arrow_ipc_stream_channel(8);

        tokio::task::spawn_blocking(move || {
            let result = {
                let conn = DbPool::get(&pool);
                DatasetRepository::new(&conn, &schema, max_page_size)
                    .query_arrow_write_all(&req, &mut writer)
            };
            if let Err(err) = result {
                log::error!("duckdb arrow full stream failed: {err}");
                writer.send_error(err);
            }
        });

        Ok(stream)
    }

    async fn count(&self, name: &str, req: &CountRequest) -> Result<i64, AppError> {
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let preds = req.predicates.clone();
        let max_page_size = self.max_page_size;
        actix_web::web::block(move || -> Result<i64, AppError> {
            let conn = DbPool::get(&pool);
            DatasetRepository::new(&conn, &schema, max_page_size).count(&preds)
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?
    }

    // DuckDB relies on engine MVCC for snapshot consistency (R4.5); the
    // `datasets` parameter is accepted for trait compliance but not used.
    async fn query_sql(
        &self,
        sql: &str,
        _datasets: &[String],
        max_rows: u64,
    ) -> Result<String, AppError> {
        let pool = self.pool.clone();
        let sql = sql.to_string();
        actix_web::web::block(move || -> Result<String, AppError> {
            let conn = DbPool::get(&pool);
            crate::repository::query_sql(&conn, &sql, max_rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))?
    }

    async fn query_sql_arrow_stream(
        &self,
        sql: &str,
        _datasets: &[String],
        max_rows: u64,
    ) -> Result<ArrowIpcStream, AppError> {
        let pool = self.pool.clone();
        let sql = sql.to_string();
        let (mut writer, stream) = arrow_ipc_stream_channel(8);

        tokio::task::spawn_blocking(move || {
            let result = {
                let conn = DbPool::get(&pool);
                crate::repository::query_sql_arrow_write(&conn, &sql, max_rows, &mut writer)
            };
            if let Err(err) = result {
                log::error!("duckdb sql arrow stream failed: {err}");
                writer.send_error(err);
            }
        });

        Ok(stream)
    }

    async fn parquet(&self, name: &str) -> Result<bytes::Bytes, AppError> {
        let schema = self.get(name)?;
        let pool = self.pool.clone();
        let max_page_size = self.max_page_size;
        let buf = actix_web::web::block(move || -> Result<Vec<u8>, AppError> {
            let conn = DbPool::get(&pool);
            DatasetRepository::new(&conn, &schema, max_page_size).parquet_bytes()
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))??;
        Ok(bytes::Bytes::from(buf))
    }

    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        Registry::reload(self, name).await
    }

    async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
        Registry::try_reload(self, name).await
    }

    async fn register(&self, cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        Registry::register(self, cfg).await
    }

    fn set_cascade_handle(&self, handle: CascadeHandle) {
        *self.cascade_handle.lock().unwrap() = Some(handle);
    }

    fn is_managed(&self, name: &str) -> bool {
        self.configs
            .read()
            .unwrap()
            .get(name)
            .map(|c| c.managed)
            .unwrap_or(false)
    }

    fn is_temp(&self, name: &str) -> bool {
        self.configs
            .read()
            .unwrap()
            .get(name)
            .map(|c| c.managed && c.temp)
            .unwrap_or(false)
    }

    async fn unregister(&self, name: &str) -> Result<(), AppError> {
        Registry::unregister(self, name).await
    }
}

#[cfg(test)]
mod tests {
    use datapress_core::config::{
        AddressingStyle, DatasetConfig, IndexConfig, S3Config, SourceConfig, SourceKind,
    };

    use super::{build_s3_secret_sql, duckdb_s3_url_style, s3_secret_scope};

    fn dataset(location: &str) -> DatasetConfig {
        DatasetConfig {
            name: "x".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: location.into(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
            managed: false,
            temp: false,
        }
    }

    #[test]
    fn s3_secret_scope_uses_bucket() {
        assert_eq!(
            s3_secret_scope(&dataset("s3://bucket/path/*.parquet")).unwrap(),
            "s3://bucket"
        );
        assert_eq!(
            s3_secret_scope(&dataset("s3://bucket/year=*/part-?.parquet")).unwrap(),
            "s3://bucket"
        );
        assert_eq!(
            s3_secret_scope(&dataset("s3://bucket/path/file.parquet")).unwrap(),
            "s3://bucket"
        );
    }

    #[test]
    fn duckdb_s3_url_style_uses_httpfs_values() {
        assert_eq!(duckdb_s3_url_style(AddressingStyle::Virtual), "vhost");
        assert_eq!(duckdb_s3_url_style(AddressingStyle::Path), "path");
    }

    #[test]
    fn explicit_s3_secret_matches_duckdb_scoped_format() {
        let mut dataset = dataset("s3://proxy-aws-bucket01/path/*.parquet");
        dataset.name = "myaws".into();
        dataset.s3 = Some(S3Config {
            region: Some("eu-west-3".into()),
            endpoint: Some("https://s3.eu-west-3.amazonaws.com".into()),
            addressing_style: AddressingStyle::Virtual,
            allow_http: false,
            access_key_id: Some("aws access key".into()),
            secret_access_key: Some("aws secret key id".into()),
            session_token: None,
            ..Default::default()
        });

        assert_eq!(
            build_s3_secret_sql(&dataset).unwrap(),
            "CREATE OR REPLACE SECRET ds_myaws (TYPE s3, PROVIDER config, ENDPOINT 's3.eu-west-3.amazonaws.com', KEY_ID 'aws access key', SECRET 'aws secret key id', REGION 'eu-west-3', URL_STYLE 'vhost', USE_SSL true, SCOPE 's3://proxy-aws-bucket01');"
        );
    }

    // -----------------------------------------------------------------------
    // R4.5 — DuckDB MVCC: CREATE OR REPLACE TABLE reads a consistent snapshot
    // so a concurrent upstream reload does not bleed into the result.
    // -----------------------------------------------------------------------
    #[test]
    fn duckdb_mvcc_create_or_replace_reads_snapshot() {
        // Open two connections sharing the same in-memory database.
        let conn1 = duckdb::Connection::open_in_memory().unwrap();
        let conn2 = conn1.try_clone().unwrap();

        // Create `base` with 3 rows.
        conn1
            .execute_batch(
                "CREATE TABLE base (id INT);\
                 INSERT INTO base VALUES (1), (2), (3);",
            )
            .unwrap();

        // Start a transaction on conn1 and materialize `derived` from `base`.
        conn1.execute_batch("BEGIN;").unwrap();
        conn1
            .execute_batch("CREATE OR REPLACE TABLE derived AS SELECT id FROM base;")
            .unwrap();

        // While conn1's transaction is open, conn2 inserts 3 more rows into
        // `base`.  Because DuckDB uses MVCC, conn1's snapshot of `base` is
        // from before this insert.
        conn2
            .execute_batch("INSERT INTO base VALUES (4), (5), (6);")
            .unwrap();

        // Commit the transaction — `derived` was built from the snapshot at
        // BEGIN, so it should contain exactly 3 rows.
        conn1.execute_batch("COMMIT;").unwrap();

        let derived_count: i64 = conn1
            .query_row("SELECT COUNT(*) FROM derived", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            derived_count, 3,
            "derived must reflect the pre-insert snapshot (MVCC isolation, R4.5)"
        );

        // After the transaction, `base` has 6 rows (both transactions visible).
        let base_count: i64 = conn1
            .query_row("SELECT COUNT(*) FROM base", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            base_count, 6,
            "base should have all 6 rows after both commits"
        );
    }
}
