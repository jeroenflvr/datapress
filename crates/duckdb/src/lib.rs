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
    let registry: Arc<dyn Backend> = Arc::new(
        db::load_registry(&cfg).expect("failed to register datasets"),
    );
    datapress_core::server::serve(cfg, registry, "DuckDB").await
}
