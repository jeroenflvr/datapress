//! `datapress-datafusion` — DataFusion backend for the DataPress HTTP server.

pub mod handlers;
pub mod store;

use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use datapress_core::config::AppConfig;
use crate::store::Store;

/// Build the dataset store, start the actix server, and run until the
/// process receives SIGINT.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    datapress_core::banner::print();
    let store = Arc::new(
        Store::load(&cfg).await.expect("failed to load datasets"),
    );
    let addr    = (cfg.server.listen, cfg.server.port);
    let workers = cfg.server.workers;
    let prefix  = cfg.server.prefix.clone();

    log::info!(
        "Listening on http://{}:{}{} (DataFusion backend, {} workers)",
        cfg.server.listen, cfg.server.port,
        if prefix.is_empty() { "".into() } else { format!("{prefix}/") },
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
    );

    let mut server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(store.clone()))
            .wrap(middleware::Logger::default())
            .service(
                web::scope(prefix.as_str())
                    .service(handlers::health)
                    .service(handlers::list_datasets)
                    .service(handlers::get_schema)
                    .service(handlers::query_dataset)
                    .service(handlers::count_dataset)
                    .service(handlers::reload_dataset),
            )
    });
    if let Some(w) = workers {
        server = server.workers(w);
    }
    server.bind(addr)?.run().await
}
