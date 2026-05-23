use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use duckdb::Connection;

use crate::config::{AppConfig, DatasetConfig};
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
    pub datasets: HashMap<String, DatasetSchema>,
}

impl Registry {
    /// Resolve a dataset by name, returning a 400-style error on miss.
    pub fn get(&self, name: &str) -> Result<&DatasetSchema, AppError> {
        self.datasets
            .get(name)
            .ok_or_else(|| AppError::UnknownColumn(format!("dataset '{name}'")))
    }

    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.datasets.keys().map(String::as_str).collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------------------
// Startup: register every dataset as an in-memory table
// ---------------------------------------------------------------------------

pub fn load_registry(cfg: &AppConfig) -> Result<Registry, AppError> {
    let conn = Connection::open_in_memory()?;
    let mut datasets = HashMap::new();

    for d in &cfg.datasets {
        log::info!("Loading dataset '{}' from {}", d.name, d.source);
        let schema = register_dataset(&conn, d)?;
        log::info!(
            "  → {} columns ({} rows in-memory)",
            schema.columns.len(),
            count_rows(&conn, &d.name)?,
        );
        datasets.insert(d.name.clone(), schema);
    }

    let pool = init_pool(conn)?;
    Ok(Registry { pool, datasets })
}

/// Materialise the parquet source as an in-memory table named `cfg.name`
/// and introspect its schema via DuckDB's `DESCRIBE`.
fn register_dataset(conn: &Connection, cfg: &DatasetConfig) -> Result<DatasetSchema, AppError> {
    let files = cfg.resolve_files()?;
    let file_list = files.iter()
        .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let table = DatasetSchema::quote_ident(&cfg.name);
    conn.execute_batch(&format!(
        "CREATE TABLE {table} AS SELECT * FROM read_parquet([{file_list}]);"
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
