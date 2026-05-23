//! `datapress-duckdb` — DuckDB backend for the DataPress HTTP server.

pub mod db;
pub mod handlers;
pub mod repository;

use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use datapress_core::config::AppConfig;

/// Build the in-memory registry, start the actix server, and run until
/// the process receives SIGINT (or `HttpServer::run`'s graceful shutdown
/// fires for any other reason).
///
/// Returns when the server has cleanly stopped.
pub async fn serve(cfg: AppConfig) -> std::io::Result<()> {
    let registry = Arc::new(
        db::load_registry(&cfg).expect("failed to register datasets"),
    );
    let addr    = (cfg.server.listen, cfg.server.port);
    let workers = cfg.server.workers;

    log::info!(
        "Listening on http://{}:{} (DuckDB backend, {} workers)",
        cfg.server.listen, cfg.server.port,
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
    );

    let mut server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(registry.clone()))
            .wrap(middleware::Logger::default())
            .service(handlers::health)
            .service(handlers::list_datasets)
            .service(handlers::get_schema)
            .service(handlers::query_dataset)
            .service(handlers::reload_dataset)
    });
    if let Some(w) = workers {
        server = server.workers(w);
    }
    server.bind(addr)?.run().await
}
