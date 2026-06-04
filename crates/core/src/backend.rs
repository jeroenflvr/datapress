//! Backend-agnostic interface used by the shared HTTP handlers.
//!
//! Both `datapress-duckdb` and `datapress-datafusion` implement [`Backend`]
//! against their own dataset registry / store. The generic handlers in
//! [`crate::handlers`] and the [`crate::server::serve`] helper then drive
//! either backend through the same code path.

use std::io::{self, Write};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::errors::AppError;
use crate::models::{CountRequest, QueryRequest};
use crate::schema::DatasetSchema;

/// Stream of Arrow IPC response chunks emitted by a backend.
pub type ArrowIpcStream = BoxStream<'static, Result<Bytes, AppError>>;

/// Writer used by backend encoders to push Arrow IPC bytes into an HTTP
/// response stream without accumulating one full response buffer.
pub struct ArrowIpcChunkWriter {
    tx: mpsc::Sender<Result<Bytes, AppError>>,
}

impl ArrowIpcChunkWriter {
    pub fn send_error(&self, err: AppError) {
        let _ = self.tx.blocking_send(Err(err));
    }
}

impl Write for ArrowIpcChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx
            .blocking_send(Ok(Bytes::copy_from_slice(buf)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "response stream closed"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn arrow_ipc_stream_channel(capacity: usize) -> (ArrowIpcChunkWriter, ArrowIpcStream) {
    let (tx, rx) = mpsc::channel(capacity);
    let writer = ArrowIpcChunkWriter { tx };
    let stream = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
    .boxed();
    (writer, stream)
}

/// Outcome of a successful [`Backend::reload`].
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReloadStats {
    pub rows: usize,
    pub elapsed_ms: u128,
}

/// One entry in `GET /api/datasets`.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetSummary {
    pub name: String,
    pub columns: usize,
    pub rows: usize,
}

/// Read / reload interface every backend exposes to the HTTP layer.
///
/// All methods are async — synchronous backends (DuckDB) wrap their
/// blocking calls in `actix_web::web::block` inside the impl.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Sorted list of dataset names.
    fn names(&self) -> Vec<String>;

    /// Cheap summary for the dataset listing endpoint. `Err(NotFound)`
    /// on unknown name.
    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError>;

    /// Full schema for `name`. `Err(NotFound)` on unknown name.
    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError>;

    /// Names of columns the backend has built an equality index over,
    /// for inclusion in the `/schema` response. Default impl returns
    /// an empty vec — backends without per-column indexes (e.g.
    /// DuckDB, which relies on the embedded database engine) need
    /// not override.
    fn indexed_columns(&self, _name: &str) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    /// JSON for the first row of the dataset, or the literal string
    /// `"null"` if the dataset is empty.
    async fn sample(&self, name: &str) -> Result<String, AppError>;

    /// Execute `req` against `name`, returning the JSON-encoded `data`
    /// array (without the `{"data": …, "page": …}` envelope — that's
    /// added by the handler).
    async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError>;

    /// Execute `req` against `name`, returning the result as an Arrow IPC
    /// **stream** byte buffer (one schema message + zero or more
    /// `RecordBatch` messages + EOS). The handler ships this verbatim
    /// with `Content-Type: application/vnd.apache.arrow.stream`.
    ///
    /// Default impl errors with `InvalidValue` — backends that don't
    /// produce Arrow natively (e.g. DuckDB today) reject the format and
    /// the handler falls through to JSON. Override on backends where
    /// batches are already Arrow.
    async fn query_arrow(&self, _name: &str, _req: &QueryRequest) -> Result<Vec<u8>, AppError> {
        Err(AppError::InvalidValue(
            "Arrow IPC response format is not supported by this backend".into(),
        ))
    }

    /// Execute `req` and stream the Arrow IPC bytes. The default adapter
    /// preserves compatibility for backends that only implement
    /// [`Backend::query_arrow`], but high-throughput backends should
    /// override this to avoid building one full response buffer.
    async fn query_arrow_stream(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let bytes = self.query_arrow(name, req).await?;
        Ok(Box::pin(stream::once(
            async move { Ok(Bytes::from(bytes)) },
        )))
    }

    /// Execute `req` and stream all matching Arrow IPC batches in one HTTP
    /// response. Unlike [`Backend::query_arrow_stream`], this is not page
    /// scoped; `limit` may still cap the total rows returned.
    async fn query_arrow_stream_all(
        &self,
        name: &str,
        req: &QueryRequest,
    ) -> Result<ArrowIpcStream, AppError> {
        let bytes = self.query_arrow(name, req).await?;
        Ok(Box::pin(stream::once(
            async move { Ok(Bytes::from(bytes)) },
        )))
    }

    /// Count rows in `name` matching `req.predicates`.
    async fn count(&self, name: &str, req: &CountRequest) -> Result<i64, AppError>;

    /// Execute a pre-validated raw `SELECT` and return the JSON-encoded
    /// `data` array (same shape as [`Backend::query`] — the handler adds
    /// the `{"data": …}` envelope).
    ///
    /// `sql` has already passed [`crate::sql::validate`]: it is a single
    /// read-only query that references only registered datasets. The
    /// backend wraps it in an outer `LIMIT max_rows` before executing so
    /// the result size is bounded regardless of the user's own `LIMIT`.
    ///
    /// Default impl errors with `InvalidValue`; backends that support raw
    /// SQL (DuckDB, DataFusion) override it.
    async fn query_sql(&self, _sql: &str, _max_rows: u64) -> Result<String, AppError> {
        Err(AppError::InvalidValue(
            "raw SQL is not supported by this backend".into(),
        ))
    }

    /// Execute a pre-validated raw `SELECT` and stream the result as Arrow
    /// IPC bytes (one schema message + zero or more `RecordBatch` messages
    /// + EOS), the same wire format as [`Backend::query_arrow_stream`].
    ///
    /// `sql` has already passed [`crate::sql::validate`]; the backend wraps
    /// it in an outer `LIMIT max_rows` so the result is bounded regardless
    /// of the caller's own clauses. Powers the Arrow content-negotiated
    /// branch of `POST /api/v1/sql`.
    ///
    /// Default impl errors with `InvalidValue`; backends that support raw
    /// SQL (DuckDB, DataFusion) override it.
    async fn query_sql_arrow_stream(
        &self,
        _sql: &str,
        _max_rows: u64,
    ) -> Result<ArrowIpcStream, AppError> {
        Err(AppError::InvalidValue(
            "raw SQL is not supported by this backend".into(),
        ))
    }

    /// Encode the **entire** dataset as a single self-contained Parquet
    /// file, returned as in-memory bytes.
    ///
    /// Powers `GET /datasets/{name}/parquet`, which serves these bytes
    /// with HTTP range support so external tools (DuckDB `httpfs`, pandas,
    /// polars, …) can read the dataset straight over HTTP — e.g.
    /// `SELECT count(*) FROM 'http://host/api/v1/datasets/accidents/parquet'`.
    ///
    /// The handler caches the result per dataset (and invalidates on
    /// reload) so the repeated range requests a Parquet reader makes all
    /// see identical, stable bytes. Default impl errors with
    /// `InvalidValue`; every shipped backend overrides it.
    async fn parquet(&self, _name: &str) -> Result<Bytes, AppError> {
        Err(AppError::InvalidValue(
            "Parquet export is not supported by this backend".into(),
        ))
    }

    /// Rebuild `name` from its configured source and atomically swap it in.
    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError>;
}
