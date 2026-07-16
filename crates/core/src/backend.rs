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

use crate::config::{DatasetConfig, OnStart};
use crate::errors::AppError;
use crate::models::{CountRequest, QueryRequest};
use crate::schema::DatasetSchema;

// ---------------------------------------------------------------------------
// Cascade notification handle (R4.3)
// ---------------------------------------------------------------------------

/// Opaque handle given to backends so they can notify the cascade engine after
/// a successful publish. Cheap to clone — Arc-backed sender.
///
/// Backends receive this via [`Backend::set_cascade_handle`] and call
/// [`CascadeHandle::notify_published`] at the end of every successful
/// `reload_inner` (after the ArcSwap / DuckDB status flip to Published).
#[derive(Clone)]
pub struct CascadeHandle {
    tx: Arc<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl CascadeHandle {
    /// Create a new handle wrapping `tx`. Only called from inside
    /// `crate::refresh` when the cascade engine is set up.
    pub(crate) fn new(tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        Self { tx: Arc::new(tx) }
    }

    /// Notify the cascade engine that `name` was successfully published.
    /// Silently discards the notification when the cascade engine has shut down.
    pub fn notify_published(&self, name: &str) {
        let _ = self.tx.send(name.to_string());
    }
}

/// Stream of Arrow IPC response chunks emitted by a backend.
pub type ArrowIpcStream = BoxStream<'static, Result<Bytes, AppError>>;

/// Target size for a single Arrow IPC response chunk. Arrow's
/// `StreamWriter` issues many tiny `write()` calls (length prefixes,
/// per-buffer padding, one call per column buffer); forwarding each as
/// its own HTTP chunk produces hundreds of micro-frames that ping-pong
/// across the `spawn_blocking` ↔ async boundary and dominate the wire
/// time. Coalescing them into ~64 KiB chunks keeps the channel and the
/// chunked transfer encoding efficient.
const ARROW_CHUNK_TARGET: usize = 64 * 1024;

/// Writer used by backend encoders to push Arrow IPC bytes into an HTTP
/// response stream without accumulating one full response buffer. Small
/// writes are buffered and flushed in ~[`ARROW_CHUNK_TARGET`]-byte chunks.
pub struct ArrowIpcChunkWriter {
    tx: mpsc::Sender<Result<Bytes, AppError>>,
    buf: Vec<u8>,
}

impl ArrowIpcChunkWriter {
    pub fn send_error(&mut self, err: AppError) {
        // Drop any partial chunk so it can't trail the error on the wire.
        self.buf.clear();
        let _ = self.tx.blocking_send(Err(err));
    }

    /// Ship whatever is currently buffered as one chunk. No-op when empty.
    fn send_buffered(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::take(&mut self.buf));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "response stream closed"))
    }
}

impl Write for ArrowIpcChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        if self.buf.len() >= ARROW_CHUNK_TARGET {
            self.send_buffered()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_buffered()
    }
}

impl Drop for ArrowIpcChunkWriter {
    fn drop(&mut self) {
        // Flush the tail the encoder didn't explicitly flush (e.g. after
        // `StreamWriter::finish`). Any send error here is unobservable —
        // the receiver has already gone away.
        let _ = self.send_buffered();
    }
}

pub fn arrow_ipc_stream_channel(capacity: usize) -> (ArrowIpcChunkWriter, ArrowIpcStream) {
    let (tx, rx) = mpsc::channel(capacity);
    let writer = ArrowIpcChunkWriter {
        tx,
        buf: Vec::with_capacity(ARROW_CHUNK_TARGET),
    };
    let stream = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
    .boxed();
    (writer, stream)
}

/// Outcome of a successful [`Backend::reload`].
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct ReloadStats {
    pub rows: usize,
    pub elapsed_ms: u128,
    /// True when a `residency = auto` build crossed the `force_lazy_above_mb`
    /// threshold and was demoted to the storage backend (R2B.2 / R2B.3).
    /// Used by the metrics layer to increment `datapress_materialize_spill_total`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub demoted_to_storage: bool,
    /// True when a `residency = memory` build crossed the `force_lazy_above_mb`
    /// threshold but was kept in RAM because the operator explicitly chose
    /// `memory` residency (R2B.1 WARN case).
    /// Used to increment `datapress_memory_override_exceeded_total`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub memory_override_exceeded: bool,
}

/// What triggered the most recent successful publish of a dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshSource {
    Startup,
    Manual,
    Schedule,
    Cascade,
}

/// Per-dataset refresh / observability record.
///
/// Stored in each backend so the `/status` endpoint and the
/// `X-Dataset-Refreshed-At` response header can read it without touching
/// the scheduler.  Updated by the backend at every successful publish and
/// by the scheduler after each tick outcome.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RefreshRecord {
    /// RFC-3339 timestamp of the last successful publish. `None` until the
    /// first build completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_at: Option<String>,
    /// Build duration in milliseconds for the last successful publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_duration_ms: Option<u128>,
    /// What triggered the last successful publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_source: Option<RefreshSource>,
    /// Generation identifier from the storage manifest (storage-backed
    /// datasets only).  `None` for memory-resident datasets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
    /// Number of consecutive scheduler failures (timeout or build error).
    /// Reset to `0` on success or coalesce.  `0` when the scheduler is not
    /// configured for this dataset.
    pub consecutive_failures: u32,
    /// Error message from the last failed build/refresh, truncated to 500
    /// characters.  `None` when the last build succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// RFC-3339 of the next scheduled fire.  Set by the scheduler when it
    /// reschedules an entry; `None` for non-scheduled datasets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_refresh_at: Option<String>,
}

/// Per-dataset lifecycle state. Updated atomically as datasets move through
/// the startup and reload pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetStatus {
    /// Registered but not yet built (startup or first-touch pending).
    Pending,
    /// Currently being built in a background task.
    Building,
    /// Built and serving queries.
    Published,
    /// Last build attempt failed; previous generation (if any) still serves.
    Failed,
}

/// Full status entry returned by [`Backend::dataset_statuses`] and
/// `GET /api/v1/datasets/{name}/status`.
/// Includes all configured datasets, not only published ones.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetStatusEntry {
    pub name: String,
    #[serde(rename = "state")]
    pub status: DatasetStatus,
    /// How this dataset is built at startup.
    #[serde(skip)]
    pub on_start: OnStart,
    /// Source kind: `"parquet"`, `"delta"`, or `"query"`.
    pub kind: String,
    /// Effective residency of the current generation: `"memory"` or `"lazy"`.
    pub residency: String,
    /// Size of the storage-backed generation in bytes. `null` for memory-resident.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_bytes: Option<u64>,
    /// Storage generation identifier (ULID). `null` for memory-resident.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
    /// RFC-3339 timestamp of the last successful publish. `null` until first build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_at: Option<String>,
    /// Build duration in milliseconds for the last successful publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_duration_ms: Option<u128>,
    /// RFC-3339 of the next scheduled fire. `null` for non-scheduled datasets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_refresh_at: Option<String>,
    /// What triggered the last successful publish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_source: Option<RefreshSource>,
    /// Consecutive scheduler failures since last success. `0` when no
    /// scheduler is configured or the last tick succeeded.
    pub consecutive_failures: u32,
    /// Error message from the last failed build/refresh (truncated to 500
    /// characters). `null` when the last build succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Number of columns (`0` when not yet published).
    pub columns: usize,
    /// Number of rows (`0` when not yet published).
    pub rows: usize,
    /// Effective lazy flag (`false` when not yet published).
    pub lazy: bool,
    /// Upstream dataset names this dataset depends on (query kind only).
    pub depends_on: Vec<String>,
}

/// One entry in `GET /api/datasets`.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetSummary {
    pub name: String,
    pub columns: usize,
    pub rows: usize,
    /// Effective lazy state — reflects `force_lazy_above_mb` having promoted
    /// a dataset to lazy at startup, not just the declared `lazy` flag.
    pub lazy: bool,
}

/// Read / reload interface every backend exposes to the HTTP layer.
///
/// All methods are async — synchronous backends (DuckDB) wrap their
/// blocking calls in `actix_web::web::block` inside the impl.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Sorted list of **published** dataset names (excludes pending/building/failed).
    fn names(&self) -> Vec<String>;

    /// Status entries for **all configured** datasets, including those in
    /// pending, building, or failed states. Default impl returns every
    /// published dataset as `Published + Eager`.
    fn dataset_statuses(&self) -> Vec<DatasetStatusEntry> {
        self.names()
            .into_iter()
            .map(|n| {
                let s = self.summary(&n).ok();
                let rec = self.refresh_record(&n).unwrap_or_default();
                DatasetStatusEntry {
                    name: n,
                    status: DatasetStatus::Published,
                    on_start: OnStart::Eager,
                    kind: "parquet".into(),
                    residency: "memory".into(),
                    storage_bytes: None,
                    generation_id: rec.generation_id,
                    last_refresh_at: rec.last_refresh_at,
                    last_refresh_duration_ms: rec.last_refresh_duration_ms,
                    next_refresh_at: rec.next_refresh_at,
                    refresh_source: rec.refresh_source,
                    consecutive_failures: rec.consecutive_failures,
                    last_error: rec.last_error,
                    columns: s.as_ref().map_or(0, |s| s.columns),
                    rows: s.as_ref().map_or(0, |s| s.rows),
                    lazy: s.is_some_and(|s| s.lazy),
                    depends_on: vec![],
                }
            })
            .collect()
    }

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
    /// read-only query that references only registered datasets. `datasets`
    /// names the distinct datasets the statement touches (lowercased); the
    /// DataFusion backend uses this to capture a consistent snapshot of
    /// each before planning (R4.5 / T1.2). DuckDB backends rely on engine
    /// MVCC and may ignore the parameter. The backend wraps `sql` in an
    /// outer `LIMIT max_rows` before executing so the result size is
    /// bounded regardless of the user's own `LIMIT`.
    ///
    /// Default impl errors with `InvalidValue`; backends that support raw
    /// SQL (DuckDB, DataFusion) override it.
    async fn query_sql(
        &self,
        _sql: &str,
        _datasets: &[String],
        _max_rows: u64,
    ) -> Result<String, AppError> {
        Err(AppError::InvalidValue(
            "raw SQL is not supported by this backend".into(),
        ))
    }

    /// Execute a pre-validated raw `SELECT` and stream the result as Arrow
    /// IPC bytes (one schema message + zero or more `RecordBatch` messages
    /// + EOS), the same wire format as [`Backend::query_arrow_stream`].
    ///
    /// `sql` has already passed [`crate::sql::validate`]; `datasets` names
    /// the distinct datasets the statement touches — see [`Self::query_sql`]
    /// for the snapshot-rule semantics. The backend wraps `sql` in an
    /// outer `LIMIT max_rows`. Powers the Arrow content-negotiated branch
    /// of `POST /api/v1/sql`.
    ///
    /// Default impl errors with `InvalidValue`; backends that support raw
    /// SQL (DuckDB, DataFusion) override it.
    async fn query_sql_arrow_stream(
        &self,
        _sql: &str,
        _datasets: &[String],
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

    /// Attempt to rebuild `name`, but skip if the per-dataset reload mutex is
    /// already held (i.e. another reload is in progress). Returns `Ok(None)`
    /// when skipped (coalesced), `Ok(Some(stats))` on success, and `Err` on
    /// a build failure.
    ///
    /// Called by the refresh scheduler (R3.2): the scheduler has already
    /// acquired the global concurrency semaphore permit before calling this;
    /// the `try_lock` ensures the per-dataset mutex is not double-acquired.
    /// Default impl falls through to a regular `reload` (i.e. no
    /// try_lock — backends that expose per-dataset try_lock must override).
    async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
        self.reload(name).await.map(Some)
    }

    /// Register a brand-new dataset from `cfg` at runtime, without a server
    /// restart. The backend opens the source, builds/registers the dataset
    /// under `cfg.name`, and makes it immediately queryable through the same
    /// registry the HTTP handlers read.
    ///
    /// Returns the fresh [`DatasetSummary`] on success. Errors with
    /// `AppError::InvalidValue` when a dataset of that name already exists,
    /// and with the backend's usual source errors (not found, access denied,
    /// empty) when the source can't be opened.
    ///
    /// Default impl errors with `InvalidValue`; the shipped backends
    /// (DuckDB, DataFusion) override it.
    async fn register(&self, _cfg: DatasetConfig) -> Result<DatasetSummary, AppError> {
        Err(AppError::InvalidValue(
            "live dataset registration is not supported by this backend".into(),
        ))
    }

    /// Install the cascade notification handle (R4.3). Called once by the
    /// server after building the cascade engine, before the first request.
    /// Default no-op — backends that sit upstream of datasets with
    /// `on_upstream_reload = true` should store the handle and call
    /// `handle.notify_published(name)` inside every successful `reload_inner`.
    fn set_cascade_handle(&self, _handle: CascadeHandle) {}

    /// Return whether `name` was created via the saved-queries API
    /// (`managed = true`). Config-file datasets always return `false`.
    ///
    /// Default impl returns `false` — shipped backends override this.
    fn is_managed(&self, _name: &str) -> bool {
        false
    }

    /// Return whether `name` is a `temp`-kind managed dataset (lost on
    /// restart). Always `false` for config-defined datasets.
    ///
    /// Default impl returns `false` — shipped backends override this.
    fn is_temp(&self, _name: &str) -> bool {
        false
    }

    /// Unregister (remove) a runtime-managed dataset from the backend's
    /// registry, deregistering it from the query engine and dropping all
    /// in-memory state. Storage generations are **not** deleted here —
    /// the caller is responsible for running GC after this returns.
    ///
    /// Returns `Err(AppError::NotFound)` when `name` is unknown, and
    /// `Err(AppError::Forbidden)` when `name` is not managed.
    ///
    /// Default impl errors with `InvalidValue`.
    async fn unregister(&self, _name: &str) -> Result<(), AppError> {
        Err(AppError::InvalidValue(
            "live dataset unregister is not supported by this backend".into(),
        ))
    }

    // ------------------------------------------------------------------
    // T5.1 / T5.2 — observability hooks
    // ------------------------------------------------------------------

    /// Return the per-dataset [`RefreshRecord`] for `name`, or `None` if the
    /// dataset is unknown.  Default impl returns `None`.
    fn refresh_record(&self, _name: &str) -> Option<RefreshRecord> {
        None
    }

    /// Persist an updated [`RefreshRecord`] for `name` into the backend's
    /// refresh-state map.  Called by:
    /// - The backend itself on every successful publish (to set
    ///   `last_refresh_at`, `last_refresh_duration_ms`, `refresh_source`, and
    ///   optionally `generation_id`).
    /// - The refresh scheduler after every tick outcome (to update
    ///   `consecutive_failures`, `last_error`, and `next_refresh_at`).
    ///
    /// Default no-op — backends implement storage.
    fn record_refresh(&self, _name: &str, _record: RefreshRecord) {}

    /// Record a failed manual or wave reload for `name`: increments
    /// `consecutive_failures` and sets `last_error` in the stored
    /// [`RefreshRecord`], preserving `last_refresh_at` and other fields from
    /// the last successful build.  Called by the `reload-all` wave task when
    /// `try_reload` returns `Err`.
    ///
    /// Default impl uses [`Self::refresh_record`] + [`Self::record_refresh`],
    /// so backends that implement those two methods get failure tracking for
    /// free.
    fn record_reload_failure(&self, name: &str, error: &str) {
        let mut rec = self.refresh_record(name).unwrap_or_default();
        rec.consecutive_failures = rec.consecutive_failures.saturating_add(1);
        rec.last_error = Some(error.chars().take(500).collect());
        self.record_refresh(name, rec);
    }

    /// RFC-3339 publish timestamp of the **current** generation of `name`.
    ///
    /// Used by the `X-Dataset-Refreshed-At` response header (T5.2).
    /// Returns `None` when the dataset has not yet been published, is not
    /// known, or when the backend does not track publish timestamps.
    ///
    /// Default impl delegates to [`Self::refresh_record`].
    fn refreshed_at(&self, name: &str) -> Option<String> {
        self.refresh_record(name).and_then(|r| r.last_refresh_at)
    }
}
