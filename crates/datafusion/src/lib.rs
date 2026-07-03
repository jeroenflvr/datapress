//! `datapress-datafusion` — DataFusion backend for the DataPress HTTP server.

pub mod store;

#[cfg(feature = "pgwire")]
pub mod pgwire;

use std::sync::Arc;

use crate::store::Store;
use datapress_core::backend::Backend;
use datapress_core::config::AppConfig;

/// Build the dataset store, start the actix server, and run until the
/// process receives SIGINT.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    datapress_core::banner::print();
    let store = Arc::new(Store::load(&cfg).await.expect("failed to load datasets"));

    #[cfg(feature = "pgwire")]
    let _pgwire = if cfg.server.pgwire.enabled {
        let ctx = store.session_context().clone();
        Some(pgwire::spawn_pgwire(ctx, cfg.server.pgwire.clone())?)
    } else {
        None
    };
    #[cfg(not(feature = "pgwire"))]
    if cfg.server.pgwire.enabled {
        log::warn!(
            "server.pgwire.enabled = true but this binary was built without the \
             `pgwire` feature; the PostgreSQL wire protocol server will not start"
        );
    }

    let store: Arc<dyn Backend> = store;
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
    let store = Arc::new(Store::load(&cfg).await.expect("failed to load datasets"));

    #[cfg(feature = "pgwire")]
    let pgwire_server = if cfg.server.pgwire.enabled {
        let ctx = store.session_context().clone();
        Some(pgwire::spawn_pgwire(ctx, cfg.server.pgwire.clone())?)
    } else {
        None
    };
    #[cfg(not(feature = "pgwire"))]
    if cfg.server.pgwire.enabled {
        log::warn!(
            "server.pgwire.enabled = true but this binary was built without the \
             `pgwire` feature; the PostgreSQL wire protocol server will not start"
        );
    }

    let store: Arc<dyn Backend> = store;
    let result =
        datapress_core::server::serve_with_shutdown(cfg, store, "DataFusion", shutdown).await;

    // The core server has returned (the `shutdown` future fired), so tear the
    // pgwire listener down with it rather than leaving it orphaned. Dropping the
    // handle signals its runtime to stop and joins the thread.
    #[cfg(feature = "pgwire")]
    drop(pgwire_server);

    result
}
