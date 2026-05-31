//! `datapress-datafusion` — DataFusion backend for the DataPress HTTP server.

pub mod store;

use std::sync::Arc;

use crate::store::Store;
use datapress_core::backend::Backend;
use datapress_core::config::AppConfig;

/// Build the dataset store, start the actix server, and run until the
/// process receives SIGINT.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    datapress_core::banner::print();
    let store: Arc<dyn Backend> =
        Arc::new(Store::load(&cfg).await.expect("failed to load datasets"));
    datapress_core::server::serve(cfg, store, "DataFusion").await
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
    let store: Arc<dyn Backend> =
        Arc::new(Store::load(&cfg).await.expect("failed to load datasets"));
    datapress_core::server::serve_with_shutdown(cfg, store, "DataFusion", shutdown).await
}
