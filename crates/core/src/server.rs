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

    log_routes(&prefix, backend.as_ref());

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

/// Pretty-print the route table at startup. Two sections:
///   - general routes (health, dataset listing)
///   - per-dataset routes (schema / query / count / reload)
fn log_routes(prefix: &str, backend: &dyn Backend) {
    // Column widths chosen to fit the longest method + a comfortable
    // path column. Names are inlined into the per-dataset paths.
    const METHOD_W: usize = 6;

    let p = prefix; // already validated to start with '/' or be empty

    log::info!("Routes:");
    log::info!("  general:");
    for (method, path) in [
        ("GET",  format!("{p}/health")),
        ("GET",  format!("{p}/api/datasets")),
    ] {
        log::info!("    {:<width$} {}", method, path, width = METHOD_W);
    }

    let names = backend.names();
    if names.is_empty() {
        log::info!("  datasets: (none registered)");
        return;
    }

    log::info!("  datasets:");
    for name in &names {
        log::info!("    {}", name);
        for (method, suffix) in [
            ("GET",  "schema"),
            ("POST", "query"),
            ("POST", "count"),
            ("POST", "reload"),
        ] {
            log::info!(
                "      {:<width$} {p}/api/datasets/{name}/{suffix}",
                method,
                width = METHOD_W,
            );
        }
    }
}
