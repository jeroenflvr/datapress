//! Shared actix-web bootstrap. Both backends call [`serve`] from their
//! own thin `serve(cfg)` entry point.

use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpServer, middleware, web};

use crate::backend::Backend;
use crate::config::AppConfig;
use crate::handlers;
use crate::timeout::Timeout;

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
    let compress = cfg.server.compress;
    let max_body = cfg.server.max_body_bytes;
    let timeout_ms = cfg.server.request_timeout_ms;

    log::info!(
        "Listening on http://{}:{}{} ({} backend, {} workers, compression {}, max-body {} bytes, timeout {})",
        cfg.server.listen, cfg.server.port,
        if prefix.is_empty() { "".into() } else { format!("{prefix}/") },
        label,
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
        if compress { "on" } else { "off" },
        max_body,
        if timeout_ms == 0 { "off".into() } else { format!("{timeout_ms} ms") },
    );

    log_routes(&prefix, backend.as_ref());

    let mut server = HttpServer::new(move || {
        let backend  = backend.clone();
        let prefix   = prefix.clone();
        let json_cfg = web::JsonConfig::default().limit(max_body);
        let pay_cfg  = web::PayloadConfig::default().limit(max_body);
        let timeout  = Timeout::new(Duration::from_millis(timeout_ms.max(1)));
        App::new()
            .app_data(web::Data::new(backend))
            .app_data(json_cfg)
            .app_data(pay_cfg)
            .wrap(middleware::Condition::new(timeout_ms > 0, timeout))
            .wrap(middleware::Condition::new(compress, middleware::Compress::default()))
            .wrap(middleware::Logger::new("%a \"%r\" %s %b bytes %Dms"))
            .service(handlers::healthz)
            .service(handlers::readyz)
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
        ("GET",  "/healthz".to_string()),
        ("GET",  "/readyz".to_string()),
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
