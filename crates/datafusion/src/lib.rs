//! `datapress-datafusion` — DataFusion backend for the DataPress HTTP server.

pub mod store;

use std::sync::Arc;

use datapress_core::backend::Backend;
use datapress_core::config::AppConfig;
use crate::store::Store;

/// Build the dataset store, start the actix server, and run until the
/// process receives SIGINT.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    datapress_core::banner::print();
    let store: Arc<dyn Backend> = Arc::new(
        Store::load(&cfg).await.expect("failed to load datasets"),
    );
    datapress_core::server::serve(cfg, store, "DataFusion").await
}
