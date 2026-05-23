use duckdb::Connection;
use std::sync::{Arc, Condvar, Mutex};

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
                return PooledConn { pool: Arc::clone(pool), conn: Some(conn) };
            }
            guard = pool.available.wait(guard).unwrap();
        }
    }
}

pub type DbPoolRef = Arc<DbPool>;

/// Build the pool. Size is read from `DB_POOL_SIZE` env var, defaulting to
/// the number of logical CPUs.
pub fn init_pool(conn: Connection) -> duckdb::Result<DbPoolRef> {
    let size = std::env::var("DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });

    // Cap DuckDB's internal thread count so N concurrent queries don't all
    // try to use every CPU core simultaneously.
    let total_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let threads_per_conn = (total_cpus / size).max(1);
    conn.execute_batch(&format!("SET threads={threads_per_conn};"))?;
    log::info!(
        "Connection pool: {size} conns × {threads_per_conn} DuckDB threads \
         (total CPUs: {total_cpus})"
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

pub fn load_into_memory(db_path: &str) -> duckdb::Result<Connection> {
    log::info!("Loading {db_path} into memory…");
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(&format!(
        "ATTACH '{db_path}' AS src (READ_ONLY);
         CREATE TABLE accidents AS SELECT * FROM src.accidents;
         DETACH src;
         CREATE INDEX idx_state    ON accidents (State);
         CREATE INDEX idx_severity ON accidents (Severity);
         CREATE INDEX idx_city     ON accidents (City);"
    ))?;
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM accidents", [], |r| r.get(0))?;
    log::info!("Loaded {count} rows into memory.");
    Ok(conn)
}
