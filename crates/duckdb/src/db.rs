use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};

use async_trait::async_trait;
use duckdb::Connection;

use datapress_core::backend::{
    ArrowIpcStream, Backend, DatasetSummary, ReloadStats, arrow_ipc_stream_channel,
};
use datapress_core::config::{AddressingStyle, AppConfig, DatasetConfig, QuackConfig, SourceKind};
use datapress_core::errors::AppError;
use datapress_core::models::{CountRequest, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

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
    configs: HashMap<String, DatasetConfig>,
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
    pub async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
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
        let pool = self.pool.clone();

        let (schema, rows) =
            actix_web::web::block(move || -> Result<(DatasetSchema, i64), AppError> {
                let conn = DbPool::get(&pool);
                replace_table(&conn, &cfg)?;
                let schema = introspect_schema(&conn, &cfg.name)?;
                let rows = count_rows(&conn, &cfg.name)?;
                Ok((schema, rows))
            })
            .await
            .map_err(|e| AppError::Internal(format!("join error: {e}")))??;

        self.datasets
            .write()
            .unwrap()
            .insert(name.to_string(), Arc::new(schema));
        self.row_counts
            .write()
            .unwrap()
            .insert(name.to_string(), rows);

        let elapsed_ms = started.elapsed().as_millis();
        log::info!("reloaded dataset '{name}': {rows} rows in {elapsed_ms} ms");
        Ok(ReloadStats {
            rows: rows as usize,
            elapsed_ms,
        })
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

    let mut datasets = HashMap::new();
    let mut configs = HashMap::new();
    let mut row_counts = HashMap::new();

    for d in &cfg.datasets {
        log::info!(
            "Loading dataset '{}' ({} @ {})",
            d.name,
            d.source.kind.as_str(),
            d.source.location
        );
        let schema = register_dataset(&conn, d)?;
        let rows = count_rows(&conn, &d.name)?;
        log::info!(
            "  → {} columns ({} rows in-memory)",
            schema.columns.len(),
            rows,
        );
        datasets.insert(d.name.clone(), Arc::new(schema));
        configs.insert(d.name.clone(), d.clone());
        row_counts.insert(d.name.clone(), rows);
    }

    if cfg.server.quack.enabled {
        start_quack_server(&conn, &cfg.server.quack)?;
    }

    let pool = init_pool(conn)?;
    Ok(Registry {
        pool,
        max_page_size: cfg.server.max_page_size.max(1),
        configs,
        datasets: RwLock::new(datasets),
        row_counts: RwLock::new(row_counts),
        reload_locks: Mutex::new(HashMap::new()),
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
fn replace_table(conn: &Connection, cfg: &DatasetConfig) -> Result<(), AppError> {
    let scan = build_scan_clause(cfg)?;
    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TABLE {table} AS SELECT * FROM {scan};"
    ))?;
    Ok(())
}

/// Materialise the source as an in-memory table named `cfg.name` and
/// introspect its schema via DuckDB's `DESCRIBE`.
fn register_dataset(conn: &Connection, cfg: &DatasetConfig) -> Result<DatasetSchema, AppError> {
    let scan = build_scan_clause(cfg)?;
    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!("CREATE TABLE {table} AS SELECT * FROM {scan};"))?;
    introspect_schema(conn, &cfg.name)
}

fn introspect_schema(conn: &Connection, table: &str) -> Result<DatasetSchema, AppError> {
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

    Ok(DatasetSchema::new(table, columns))
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

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        let schema = self.get(name)?;
        let rows = self
            .row_counts
            .read()
            .unwrap()
            .get(name)
            .copied()
            .unwrap_or(0);
        Ok(DatasetSummary {
            name: schema.name.clone(),
            columns: schema.columns.len(),
            rows: rows.max(0) as usize,
        })
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        self.get(name)
    }

    async fn sample(&self, name: &str) -> Result<String, AppError> {
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

    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
        Registry::reload(self, name).await
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
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
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
        });

        assert_eq!(
            build_s3_secret_sql(&dataset).unwrap(),
            "CREATE OR REPLACE SECRET ds_myaws (TYPE s3, PROVIDER config, ENDPOINT 's3.eu-west-3.amazonaws.com', KEY_ID 'aws access key', SECRET 'aws secret key id', REGION 'eu-west-3', URL_STYLE 'vhost', USE_SSL true, SCOPE 's3://proxy-aws-bucket01');"
        );
    }
}
