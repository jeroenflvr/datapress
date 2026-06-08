//! Synchronous, blocking wrapper around the async [`crate::Client`].
//!
//! Backed by a private current-thread Tokio runtime. Enable with the
//! `blocking` feature. Convenient for scripts, tests, and the Python
//! bindings, which want a plain call-and-wait API.

use crate::error::Result;
use crate::models::{Predicate, QueryRequest, QueryResponse, SqlResponse};
use serde_json::Value as JsonValue;

/// Blocking DataPress client.
///
/// Each instance owns a single-threaded Tokio runtime used to drive the
/// async client to completion. Not `Clone` (the runtime is not shared);
/// create one per thread.
pub struct Client {
    inner: crate::Client,
    rt: tokio::runtime::Runtime,
}

impl Client {
    /// Construct a blocking client with defaults for `base_url`.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::from_async(crate::Client::new(base_url)?)
    }

    /// Wrap an already-built async [`crate::Client`].
    pub fn from_async(inner: crate::Client) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| crate::ClientError::Decode(format!("runtime build failed: {e}")))?;
        Ok(Self { inner, rt })
    }

    /// Start a [`crate::ClientBuilder`]; pass the result to
    /// [`Client::from_async`] after `.build()`.
    pub fn builder(base_url: impl Into<String>) -> crate::ClientBuilder {
        crate::ClientBuilder::new(base_url)
    }

    /// Liveness probe.
    pub fn healthz(&self) -> Result<JsonValue> {
        self.rt.block_on(self.inner.healthz())
    }

    /// Readiness probe.
    pub fn readyz(&self) -> Result<JsonValue> {
        self.rt.block_on(self.inner.readyz())
    }

    /// List registered dataset names.
    pub fn datasets(&self) -> Result<Vec<String>> {
        self.rt.block_on(self.inner.datasets())
    }

    /// Fetch the schema description for `dataset`.
    pub fn schema(&self, dataset: &str) -> Result<JsonValue> {
        self.rt.block_on(self.inner.schema(dataset))
    }

    /// Count matching rows.
    pub fn count(&self, dataset: &str, predicates: &[Predicate]) -> Result<u64> {
        self.rt.block_on(self.inner.count(dataset, predicates))
    }

    /// Run a structured query, returning the JSON envelope.
    pub fn query_json(&self, dataset: &str, request: &QueryRequest) -> Result<QueryResponse> {
        self.rt.block_on(self.inner.query_json(dataset, request))
    }

    /// Run a raw read-only SQL statement.
    pub fn sql(&self, sql: impl Into<String>, max_rows: Option<u64>) -> Result<SqlResponse> {
        self.rt.block_on(self.inner.sql(sql, max_rows))
    }

    /// Trigger an in-place reload of `dataset`.
    pub fn reload(&self, dataset: &str) -> Result<JsonValue> {
        self.rt.block_on(self.inner.reload(dataset))
    }

    /// Run a structured query asking for Arrow IPC, returning the raw
    /// stream bytes.
    pub fn query_arrow_bytes(
        &self,
        dataset: &str,
        request: &QueryRequest,
    ) -> Result<bytes::Bytes> {
        self.rt
            .block_on(self.inner.query_arrow_bytes(dataset, request))
    }

    /// Run a structured query and decode the Arrow IPC response into
    /// record batches.
    #[cfg(feature = "arrow")]
    pub fn query_arrow(
        &self,
        dataset: &str,
        request: &QueryRequest,
    ) -> Result<Vec<arrow::record_batch::RecordBatch>> {
        self.rt.block_on(self.inner.query_arrow(dataset, request))
    }

    /// Access the underlying async client (shares the same HTTP pool).
    pub fn inner(&self) -> &crate::Client {
        &self.inner
    }
}
