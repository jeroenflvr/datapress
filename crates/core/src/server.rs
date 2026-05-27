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
/// `backend`, and run until the process receives `SIGINT` or `SIGTERM`.
///
/// Shutdown is **graceful**: on signal the listening socket is closed,
/// existing connections get up to `cfg.server.shutdown_timeout_secs`
/// seconds to drain in-flight requests, then workers are stopped.
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
    let shutdown_secs = cfg.server.shutdown_timeout_secs;

    log::info!(
        "Listening on http://{}:{}{} ({} backend, {} workers, compression {}, max-body {} bytes, timeout {}, shutdown grace {}s)",
        cfg.server.listen, cfg.server.port,
        if prefix.is_empty() { "".into() } else { format!("{prefix}/") },
        label,
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
        if compress { "on" } else { "off" },
        max_body,
        if timeout_ms == 0 { "off".into() } else { format!("{timeout_ms} ms") },
        shutdown_secs,
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
                    // Canonical, versioned API.
                    .service(web::scope("/api/v1").configure(handlers::v1::configure))
                    // Legacy un-versioned alias. Kept around so older
                    // clients (and the historical `/api/datasets/...`
                    // URLs in docs / scripts) keep working. New code
                    // should prefer `/api/v1/...`.
                    .service(web::scope("/api").configure(handlers::v1::configure)),
            )
    });
    if let Some(w) = workers {
        server = server.workers(w);
    }

    // Disable actix's built-in signal handling so we can log which signal
    // triggered shutdown, then drive the same `ServerHandle::stop(true)`
    // path it would have used internally.
    let running = server
        .bind(addr)?
        .shutdown_timeout(shutdown_secs)
        .disable_signals()
        .run();
    let handle = running.handle();
    tokio::spawn(shutdown_listener(handle, shutdown_secs));

    running.await
}

/// Wait for `SIGINT` / `SIGTERM` (or `Ctrl+C` on non-Unix), log which one
/// arrived, then ask the actix server handle to stop gracefully.
async fn shutdown_listener(handle: actix_web::dev::ServerHandle, grace_secs: u64) {
    let which = wait_for_signal().await;
    log::info!(
        "Received {which}, shutting down gracefully (up to {grace_secs}s for in-flight requests)..."
    );
    handle.stop(true).await;
    log::info!("Shutdown complete.");
}

#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    // `expect` is OK here: failing to install a signal handler at startup
    // is a misconfigured runtime, not a recoverable condition.
    let mut sigterm = signal(SignalKind::terminate())
        .expect("install SIGTERM handler");
    let mut sigint  = signal(SignalKind::interrupt())
        .expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv()  => "SIGINT",
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() -> &'static str {
    // Windows / other: only Ctrl+C is portably available through tokio.
    let _ = tokio::signal::ctrl_c().await;
    "Ctrl+C"
}

/// Pretty-print the route table at startup. Two sections:
///   - general routes (health, probes)
///   - per-dataset routes for every mounted API version (canonical
///     `/api/v1/...` + the legacy un-versioned `/api/...` alias).
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
    ] {
        log::info!("    {:<width$} {}", method, path, width = METHOD_W);
    }

    // Each API version is mounted under its own scope; we currently
    // also expose v1 under the un-versioned `/api` for back-compat.
    let mounts: &[(&str, &[(&str, &str)])] = &[
        ("/api/v1", handlers::v1::ROUTES),
        ("/api",    handlers::v1::ROUTES), // legacy alias
    ];

    let names = backend.names();
    for (mount, routes) in mounts {
        log::info!("  {p}{mount}:");
        // Top-level (non-dataset-scoped) routes for this version.
        for (method, suffix) in *routes {
            if !suffix.contains("{name}") {
                log::info!(
                    "    {:<width$} {p}{mount}{suffix}",
                    method, width = METHOD_W,
                );
            }
        }
        if names.is_empty() {
            log::info!("    (no datasets registered)");
            continue;
        }
        for name in &names {
            for (method, suffix) in *routes {
                if let Some(rest) = suffix.strip_prefix("/datasets/{name}") {
                    log::info!(
                        "    {:<width$} {p}{mount}/datasets/{name}{rest}",
                        method, width = METHOD_W,
                    );
                }
            }
        }
    }
}
