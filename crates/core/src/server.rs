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
    let docs_cfg = cfg.docs.clone();
    let swagger_cfg = cfg.swagger.clone();

    // Warn (but don't fail) when the operator asked for docs in TOML but
    // this binary was built without the cargo feature that embeds them.
    #[cfg(not(feature = "docs"))]
    if docs_cfg.enabled {
        log::warn!(
            "[docs] enabled = true in config, but this binary was built \
             without --features docs; skipping docs site"
        );
    }
    #[cfg(not(feature = "swagger"))]
    if swagger_cfg.enabled {
        log::warn!(
            "[swagger] enabled = true in config, but this binary was built \
             without --features swagger; skipping Swagger UI"
        );
    }
    #[cfg(not(feature = "auth"))]
    if cfg.auth.enabled {
        log::warn!(
            "[auth] enabled = true in config, but this binary was built \
             without --features auth; skipping OIDC enforcement"
        );
    }

    // Boot the JWKS cache (and validate config) before binding the
    // listener. With `start_degraded = true` this only warns on an
    // unreachable IdP; with `false` it propagates the error and the
    // process exits non-zero.
    #[cfg(feature = "auth")]
    let auth_state = if cfg.auth.enabled {
        let jwks = crate::auth::JwksCache::boot(&cfg.auth).await
            .map_err(|e| std::io::Error::other(format!("auth bootstrap failed: {e}")))?;
        log::info!(
            "[auth] OIDC enforcement enabled (issuer = {}, audience = {}, read_scopes = {:?}, reload_scopes = {:?})",
            cfg.auth.issuer,
            if cfg.auth.audience.is_empty() { "<none>" } else { cfg.auth.audience.as_str() },
            cfg.auth.read_scopes,
            cfg.auth.reload_scopes,
        );
        Some(crate::auth::AuthState {
            cfg:  Arc::new(cfg.auth.clone()),
            jwks,
        })
    } else {
        None
    };

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

    #[cfg(feature = "docs")]
    if docs_cfg.enabled {
        log::info!("  {} (mkdocs site):", docs_cfg.path);
        log::info!("    GET    {}/", docs_cfg.path);
        log::info!("    GET    {}/{{path}}", docs_cfg.path);
    }

    #[cfg(feature = "swagger")]
    if swagger_cfg.enabled {
        log::info!("  {} (swagger UI):", swagger_cfg.path);
        log::info!("    GET    {}/", swagger_cfg.path);
        log::info!("    GET    {}/openapi.json", swagger_cfg.path);
    }

    let build_info = web::Data::new(handlers::BuildInfo::new(
        // `&'static str` so it fits BuildInfo's compile-time fields.
        // The match keeps this generic enough for future backends.
        match label {
            "DuckDB"     => "DuckDB",
            "DataFusion" => "DataFusion",
            _            => "unknown",
        },
    ));

    let mut server = HttpServer::new(move || {
        let backend  = backend.clone();
        let prefix   = prefix.clone();
        let json_cfg = web::JsonConfig::default().limit(max_body);
        let pay_cfg  = web::PayloadConfig::default().limit(max_body);
        let timeout  = Timeout::new(Duration::from_millis(timeout_ms.max(1)));
        #[cfg(feature = "docs")]
        let docs_cfg = docs_cfg.clone();
        #[cfg(feature = "swagger")]
        let swagger_cfg = swagger_cfg.clone();
        #[cfg(feature = "auth")]
        let auth_state = auth_state.clone();
        let app = App::new()
            .app_data(web::Data::new(backend))
            .app_data(build_info.clone())
            .app_data(json_cfg)
            .app_data(pay_cfg)
            .wrap(middleware::Condition::new(timeout_ms > 0, timeout))
            .wrap(middleware::Condition::new(compress, middleware::Compress::default()))
            .wrap(middleware::Logger::new("%a \"%r\" %s %b bytes %Dms"));
        // Auth middleware wraps everything below — including the docs +
        // swagger services and the prefix scope. Health/version probes
        // are registered above and remain unauthenticated by design so
        // load balancers can keep checking liveness. When auth is
        // disabled the middleware is a pass-through.
        #[cfg(feature = "auth")]
        let app = match auth_state.clone() {
            Some(state) => app
                .app_data(web::Data::new(state.cfg.clone()))
                .wrap(crate::auth::Auth::new(state)),
            None => app.wrap(crate::auth::Auth::disabled()),
        };
        let app = app
            .service(handlers::healthz)
            .service(handlers::readyz)
            .service(handlers::version);
        // Docs + swagger are registered BEFORE the `web::scope(prefix)`
        // catch-all below. An empty `prefix` (the default) becomes
        // `web::scope("")` which matches every path and 404s any miss
        // *inside* the scope — so services registered after it become
        // unreachable. Keeping these at the top of the dispatch chain
        // sidesteps that.
        #[cfg(feature = "docs")]
        let app = if docs_cfg.enabled {
            app.configure(|c| crate::docs::configure(&docs_cfg.path, c))
        } else {
            app
        };
        #[cfg(feature = "swagger")]
        let app = if swagger_cfg.enabled {
            app.configure(|c| crate::swagger::configure(&swagger_cfg.path, swagger_cfg.oauth2.as_ref(), c))
        } else {
            app
        };
        app.service(
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
        ("GET",  "/version".to_string()),
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
