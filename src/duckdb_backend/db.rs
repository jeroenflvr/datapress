use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};

use duckdb::Connection;

use crate::config::{AppConfig, DatasetConfig, SourceKind};
use crate::errors::AppError;
use crate::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------

pub struct DbPool {
    conns:     Mutex<Vec<Connection>>,
    available: Condvar,
}

/// RAII guard — returns the connection to the pool on drop.
pub struct PooledConn {
    pool: Arc<DbPool>,
    conn: Option<Connection>,
}

impl std::ops::Deref for PooledConn {
    type Target = Connection;
    fn deref(&self) -> &Connection { self.conn.as_ref().unwrap() }
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
                return PooledConn { pool: Arc::clone(pool), conn: Some(conn) };
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
    pub pool:     DbPoolRef,
    /// Original dataset configs, indexed by name. Reload reads the source
    /// path from here — clients can't redirect a reload at an arbitrary file.
    configs:      HashMap<String, DatasetConfig>,
    /// Hot-swappable schema map. `RwLock` is enough here: reads are very
    /// short (clone an `Arc`); writes happen only on reload.
    datasets:     RwLock<HashMap<String, Arc<DatasetSchema>>>,
    /// Per-name reload mutex. Serialises concurrent reloads of the same
    /// dataset; reloads of different datasets proceed in parallel.
    reload_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Outcome of a successful `reload`.
pub struct ReloadStats {
    pub rows:       usize,
    pub elapsed_ms: u128,
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

    /// Rebuild `name` from disk and atomically swap it in. DuckDB's
    /// `CREATE OR REPLACE TABLE` runs in a single transaction — in-flight
    /// SELECTs against the old table see snapshot-consistent data through
    /// MVCC, and the next query sees the new table.
    pub async fn reload(self: &Arc<Self>, name: &str) -> Result<ReloadStats, AppError> {
        let cfg = self
            .configs
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

        let started = std::time::Instant::now();
        let pool    = self.pool.clone();

        let (schema, rows) = actix_web::web::block(move || -> Result<(DatasetSchema, i64), AppError> {
            let conn = DbPool::get(&pool);
            replace_table(&conn, &cfg)?;
            let schema = introspect_schema(&conn, &cfg.name)?;
            let rows   = count_rows(&conn, &cfg.name)?;
            Ok((schema, rows))
        })
        .await
        .map_err(|e| AppError::Internal(format!("join error: {e}")))??;

        self.datasets
            .write()
            .unwrap()
            .insert(name.to_string(), Arc::new(schema));

        let elapsed_ms = started.elapsed().as_millis();
        log::info!("reloaded dataset '{name}': {rows} rows in {elapsed_ms} ms");
        Ok(ReloadStats { rows: rows as usize, elapsed_ms })
    }
}

// ---------------------------------------------------------------------------
// Startup: register every dataset as an in-memory table
// ---------------------------------------------------------------------------

pub fn load_registry(cfg: &AppConfig) -> Result<Registry, AppError> {
    let conn = Connection::open_in_memory()?;

    // Install the extensions we'll need across the dataset list. Each
    // INSTALL is a no-op when the extension is already cached on disk;
    // the first run downloads from the DuckDB extension repo.
    let needs_httpfs = cfg.datasets.iter().any(|d| d.source.is_s3());
    let needs_delta  = cfg.datasets.iter().any(|d| d.source.kind == SourceKind::Delta);
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

    let mut datasets = HashMap::new();
    let mut configs  = HashMap::new();

    for d in &cfg.datasets {
        log::info!(
            "Loading dataset '{}' ({} @ {})",
            d.name, d.source.kind.as_str(), d.source.location
        );
        let schema = register_dataset(&conn, d)?;
        log::info!(
            "  → {} columns ({} rows in-memory)",
            schema.columns.len(),
            count_rows(&conn, &d.name)?,
        );
        datasets.insert(d.name.clone(), Arc::new(schema));
        configs.insert(d.name.clone(), d.clone());
    }

    let pool = init_pool(conn)?;
    Ok(Registry {
        pool,
        configs,
        datasets:     RwLock::new(datasets),
        reload_locks: Mutex::new(HashMap::new()),
    })
}

/// Build the `SELECT` source clause for `read_parquet(…)` or
/// `delta_scan(…)` from a dataset config. For local parquet this expands
/// to an explicit list of files (so DuckDB doesn't have to re-glob on
/// every reload); for S3 / Delta we pass the URL string through unchanged.
fn build_scan_clause(cfg: &DatasetConfig) -> Result<String, AppError> {
    match (cfg.source.kind, cfg.source.is_s3()) {
        (SourceKind::Parquet, false) => {
            let files = cfg.resolve_local_parquet_files()?;
            let file_list = files.iter()
                .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("read_parquet([{file_list}])"))
        }
        (SourceKind::Parquet, true) => {
            // DuckDB accepts a single URL or a glob; pass the location through.
            let loc = cfg.source.location.replace('\'', "''");
            Ok(format!("read_parquet('{loc}')"))
        }
        (SourceKind::Delta, _) => {
            let loc = cfg.source.location.replace('\'', "''");
            Ok(format!("delta_scan('{loc}')"))
        }
    }
}

/// Issue a `CREATE OR REPLACE SECRET` for one S3 dataset. The secret is
/// scoped to the dataset's `s3://bucket/prefix` so peers with different
/// credentials don't collide.
fn apply_s3_secret(conn: &Connection, cfg: &DatasetConfig) -> Result<(), AppError> {
    let creds = cfg.resolved_creds();
    // If we have no explicit creds, leave DuckDB to use its own provider
    // chain (env, IMDS, ~/.aws/credentials). We still want region/endpoint
    // applied though, so we always emit a secret if non-credential S3
    // settings are present.
    let s3 = cfg.s3.clone().unwrap_or_default();
    let region = cfg.resolved_region();

    let mut parts: Vec<String> = vec!["TYPE S3".to_string()];
    parts.push(format!("REGION '{}'", region.replace('\'', "''")));
    if let Some(ep) = s3.endpoint.as_deref().filter(|s| !s.is_empty()) {
        // DuckDB wants endpoint *without* the scheme.
        let bare = ep.trim_start_matches("http://").trim_start_matches("https://");
        parts.push(format!("ENDPOINT '{}'", bare.replace('\'', "''")));
    }
    parts.push(format!("URL_STYLE '{}'", s3.addressing_style.as_str()));
    if s3.allow_http {
        parts.push("USE_SSL false".to_string());
    }
    if let (Some(k), Some(s)) = (creds.access_key_id.as_deref(), creds.secret_access_key.as_deref()) {
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
        // No explicit keys — ask DuckDB to use its credential chain.
        parts.push("PROVIDER credential_chain".to_string());
    }
    parts.push(format!("SCOPE '{}'", cfg.source.location.replace('\'', "''")));

    // Secret name: dataset name normalised. DuckDB identifiers are
    // case-insensitive and accept alphanum + underscore.
    let secret_name = format!(
        "ds_{}",
        cfg.name.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    );
    let sql = format!(
        "CREATE OR REPLACE SECRET {secret_name} ({});",
        parts.join(", ")
    );
    conn.execute_batch(&sql)?;
    Ok(())
}

/// Atomically replace the dataset's table by re-reading its source.
/// `CREATE OR REPLACE TABLE ... AS SELECT ...` is a single DuckDB transaction:
/// if the source read fails, the existing table is preserved.
fn replace_table(conn: &Connection, cfg: &DatasetConfig) -> Result<(), AppError> {
    let scan  = build_scan_clause(cfg)?;
    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TABLE {table} AS SELECT * FROM {scan};"
    ))?;
    Ok(())
}

/// Materialise the source as an in-memory table named `cfg.name` and
/// introspect its schema via DuckDB's `DESCRIBE`.
fn register_dataset(conn: &Connection, cfg: &DatasetConfig) -> Result<DatasetSchema, AppError> {
    let scan  = build_scan_clause(cfg)?;
    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!(
        "CREATE TABLE {table} AS SELECT * FROM {scan};"
    ))?;
    introspect_schema(conn, &cfg.name)
}

fn introspect_schema(conn: &Connection, table: &str) -> Result<DatasetSchema, AppError> {
    let mut stmt = conn.prepare(&format!(
        "DESCRIBE {}",
        DatasetSchema::quote_ident(table)
    ))?;
    let rows = stmt.query_map([], |row| {
        // DESCRIBE columns: column_name, column_type, null, key, default, extra
        let name:     String = row.get(0)?;
        let sql_type: String = row.get(1)?;
        let nullable: String = row.get::<_, String>(2).unwrap_or_else(|_| "YES".into());
        Ok((name, sql_type, nullable))
    })?;

    let columns = rows
        .map(|r| r.map(|(name, sql_type, nullable)| ColumnInfo {
            logical:  classify_duckdb_type(&sql_type),
            sql_type,
            nullable: nullable.eq_ignore_ascii_case("YES"),
            name,
        }))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(DatasetSchema::new(table, columns))
}

fn classify_duckdb_type(sql_type: &str) -> LogicalType {
    // DuckDB type strings: TINYINT, SMALLINT, INTEGER, BIGINT, HUGEINT,
    // UTINYINT…, FLOAT, DOUBLE, DECIMAL(.., ..), VARCHAR, TEXT, BOOLEAN,
    // DATE, TIME, TIMESTAMP, TIMESTAMP_S, TIMESTAMP_NS, TIMESTAMPTZ, …
    let t = sql_type.to_ascii_uppercase();
    if t.starts_with("BOOL")                       { LogicalType::Bool }
    else if t == "FLOAT" || t == "DOUBLE"
         || t == "REAL"  || t.starts_with("DECIMAL")
                                                    { LogicalType::Float }
    else if t.ends_with("INT") || t.starts_with("UINT")
         || t == "HUGEINT"                          { LogicalType::Int }
    else if t == "VARCHAR" || t == "TEXT"
         || t == "STRING"  || t == "CHAR"
         || t.starts_with("VARCHAR(")               { LogicalType::Utf8 }
    else if t.starts_with("TIMESTAMP")
         || t == "DATE" || t == "TIME"
         || t.starts_with("INTERVAL")               { LogicalType::Temporal }
    else                                            { LogicalType::Other }
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
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4));

    let total_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
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
        conns:     Mutex::new(conns),
        available: Condvar::new(),
    }))
}
