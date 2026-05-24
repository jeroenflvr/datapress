//! Shared actix-web bootstrap. Both backends call [`serve`] from their
//! own thin `serve(cfg)` entry point.

use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use crate::backend::Backend;
use crate::config::AppConfig;
use crate::handlers;

/// Bind the HTTP server, register the generic handler set against
/// `backend`, and run until the process receives SIGINT.
///
/// `label` is the human-readable backend name used in the startup log
/// line (e.g. `"DuckDB"`, `"DataFusion"`).
pub async fn serve(
    cfg:     AppConfig,
    backend: Arc<dyn Backend>,
    label:   &str,
) -> std::io::Result<()> {
    let addr    = (cfg.server.listen, cfg.server.port);
    let workers = cfg.server.workers;
    let prefix  = cfg.server.prefix.clone();

    log::info!(
        "Listening on http://{}:{}{} ({} backend, {} workers)",
        cfg.server.listen, cfg.server.port,
        if prefix.is_empty() { "".into() } else { format!("{prefix}/") },
        label,
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
    );

    let mut server = HttpServer::new(move || {
        let backend = backend.clone();
        let prefix  = prefix.clone();
        App::new()
            .app_data(web::Data::new(backend))
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
