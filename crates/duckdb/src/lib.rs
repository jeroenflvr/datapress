//! `datapress-duckdb` — DuckDB backend for the DataPress HTTP server.

pub mod db;
pub mod repository;

use std::sync::Arc;

use datapress_core::backend::Backend;
use datapress_core::config::AppConfig;

/// Build the in-memory registry, start the actix server, and run until
/// the process receives SIGINT.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    datapress_core::banner::print();
    let registry: Arc<dyn Backend> =
        Arc::new(db::load_registry(&cfg).map_err(|e| std::io::Error::other(format!("{e}")))?);
    datapress_core::server::serve(cfg, registry, "DuckDB").await
}

/// Like [`serve`], but driven to a graceful stop by `shutdown` instead of
/// OS signals. Used when DataPress is embedded in another runtime (the
/// Python extension) so it doesn't install signal handlers that fight the
/// host's.
pub async fn serve_with_shutdown(
    cfg: AppConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    datapress_core::banner::print();
    let registry: Arc<dyn Backend> =
        Arc::new(db::load_registry(&cfg).map_err(|e| std::io::Error::other(format!("{e}")))?);
    datapress_core::server::serve_with_shutdown(cfg, registry, "DuckDB", shutdown).await
}
