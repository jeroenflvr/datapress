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
    // Create the store with all datasets Pending — no builds yet.
    let store = Arc::new(
        Store::new_nonblocking(&cfg)
            .await
            .expect("failed to initialise dataset store"),
    );

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

    // Spawn background startup builds (non-blocking — returns immediately).
    // The HTTP listener below binds and starts serving while these run.
    store
        .clone()
        .spawn_startup_builds(cfg.server.startup.max_concurrent, &cfg.server);

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
    let store = Arc::new(
        Store::new_nonblocking(&cfg)
            .await
            .expect("failed to initialise dataset store"),
    );

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

    store
        .clone()
        .spawn_startup_builds(cfg.server.startup.max_concurrent, &cfg.server);

    let store: Arc<dyn Backend> = store;
    let result =
        datapress_core::server::serve_with_shutdown(cfg, store, "DataFusion", shutdown).await;

    #[cfg(feature = "pgwire")]
    drop(pgwire_server);

    result
}
