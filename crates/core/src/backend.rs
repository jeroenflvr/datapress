//! Backend-agnostic interface used by the shared HTTP handlers.
//!
//! Both `datapress-duckdb` and `datapress-datafusion` implement [`Backend`]
//! against their own dataset registry / store. The generic handlers in
//! [`crate::handlers`] and the [`crate::server::serve`] helper then drive
//! either backend through the same code path.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::errors::AppError;
use crate::models::{CountRequest, QueryRequest};
use crate::schema::DatasetSchema;

/// Outcome of a successful [`Backend::reload`].
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReloadStats {
    pub rows:       usize,
    pub elapsed_ms: u128,
}

/// One entry in `GET /api/datasets`.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetSummary {
    pub name:    String,
    pub columns: usize,
    pub rows:    usize,
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

    /// JSON for the first row of the dataset, or the literal string
    /// `"null"` if the dataset is empty.
    async fn sample(&self, name: &str) -> Result<String, AppError>;

    /// Execute `req` against `name`, returning the JSON-encoded `data`
    /// array (without the `{"data": …, "page": …}` envelope — that's
    /// added by the handler).
    async fn query(&self, name: &str, req: &QueryRequest) -> Result<String, AppError>;

    /// Count rows in `name` matching `req.predicates`.
    async fn count(&self, name: &str, req: &CountRequest) -> Result<i64, AppError>;

    /// Rebuild `name` from its configured source and atomically swap it in.
    async fn reload(&self, name: &str) -> Result<ReloadStats, AppError>;
}
