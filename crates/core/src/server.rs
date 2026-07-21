//! Shared actix-web bootstrap. Both backends call [`serve`] from their
//! own thin `serve(cfg)` entry point.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpServer, middleware, web};

use crate::backend::Backend;
use crate::config::AppConfig;
use crate::handlers;
use crate::refresh::{CascadeDag, CascadeDep, DatasetSchedule, RefreshScheduler, TtlHandle};
use crate::timeout::Timeout;

/// How the running server is asked to begin a graceful shutdown.
enum Shutdown {
    /// Install `SIGINT`/`SIGTERM` (or `Ctrl+C`) handlers and stop when one
    /// arrives. Used by the standalone binaries, which own the process and
    /// its signal disposition.
    Signals,
    /// Stop when the given future resolves. Used when DataPress is embedded
    /// (e.g. the Python extension), where the *host* owns signal handling
    /// and drives shutdown by completing this future. No OS signal handlers
    /// are installed, so we never fight the host's handlers.
    External(Pin<Box<dyn Future<Output = ()> + Send>>),
}

/// Bind the HTTP server, register the generic handler set against
/// `backend`, and run until the process receives `SIGINT` or `SIGTERM`.
///
/// Shutdown is **graceful**: on signal the listening socket is closed,
/// existing connections get up to `cfg.server.shutdown_timeout_secs`
/// seconds to drain in-flight requests, then workers are stopped.
///
/// `label` is the human-readable backend name used in the startup log
/// line (e.g. `"DuckDB"`, `"DataFusion"`).
pub async fn serve(cfg: AppConfig, backend: Arc<dyn Backend>, label: &str) -> std::io::Result<()> {
    run_server(cfg, backend, label, Shutdown::Signals).await
}

/// Like [`serve`], but driven to a graceful stop by `shutdown` instead of
/// OS signals.
///
/// Intended for embedding DataPress inside another runtime (the Python
/// extension's `DataPress.run()`), where installing process-global signal
/// handlers would race the host's own. The caller resolves `shutdown` —
/// for example when its asyncio task is cancelled by `Ctrl+C` — and the
/// server then drains in-flight requests within
/// `cfg.server.shutdown_timeout_secs` and returns.
pub async fn serve_with_shutdown(
    cfg: AppConfig,
    backend: Arc<dyn Backend>,
    label: &str,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    run_server(cfg, backend, label, Shutdown::External(Box::pin(shutdown))).await
}

async fn run_server(
    cfg: AppConfig,
    backend: Arc<dyn Backend>,
    label: &str,
    shutdown: Shutdown,
) -> std::io::Result<()> {
    let addr = (cfg.server.listen, cfg.server.port);
    let workers = cfg.server.workers;
    let prefix = cfg.server.prefix.clone();
    let compress = cfg.server.compress;
    let max_body = cfg.server.max_body_bytes;
    let max_page_size = cfg.server.max_page_size;
    let timeout_ms = cfg.server.request_timeout_ms;
    let shutdown_secs = cfg.server.shutdown_timeout_secs;
    let sql_settings = handlers::SqlSettings {
        enabled: cfg.sql.enabled,
        max_rows: cfg.sql.max_rows.max(1),
    };
    let readiness_settings = handlers::ReadinessSettings {
        readiness_mode: cfg.server.startup.readiness.clone(),
    };

    // Compute the saved-queries dir from the config path + optional override.
    let saved_queries_dir: Option<std::path::PathBuf> = crate::config::source_config_path()
        .map(|p| {
            crate::config::resolve_saved_queries_dir(&p, cfg.server.saved_queries_dir.as_deref())
        })
        .transpose()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // Routes are enabled when the admin token is set OR auth is configured.
    let queries_api_enabled = crate::admin::require_admin_configured() || cfg.auth.enabled;
    let docs_cfg = cfg.docs.clone();
    let swagger_cfg = cfg.swagger.clone();
    let metrics_cfg = cfg.metrics.clone();
    let explorer_cfg = cfg.explorer.clone();

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
    #[cfg(not(feature = "metrics"))]
    if metrics_cfg.enabled {
        log::warn!(
            "[metrics] enabled = true in config, but this binary was built \
             without --features metrics; skipping Prometheus endpoint"
        );
    }
    #[cfg(not(feature = "explorer"))]
    if explorer_cfg.enabled {
        log::warn!(
            "[explorer] enabled = true in config, but this binary was built \
             without --features explorer; skipping explorer UI"
        );
    }

    // Boot the JWKS cache (and validate config) before binding the
    // listener. With `start_degraded = true` this only warns on an
    // unreachable IdP; with `false` it propagates the error and the
    // process exits non-zero.
    #[cfg(feature = "auth")]
    let auth_state = if cfg.auth.enabled {
        let jwks = crate::auth::JwksCache::boot(&cfg.auth)
            .await
            .map_err(|e| std::io::Error::other(format!("auth bootstrap failed: {e}")))?;
        log::info!(
            "[auth] OIDC enforcement enabled (issuer = {}, audience = {}, read_scopes = {:?}, reload_scopes = {:?})",
            cfg.auth.issuer,
            if cfg.auth.audience.is_empty() {
                "<none>"
            } else {
                cfg.auth.audience.as_str()
            },
            cfg.auth.read_scopes,
            cfg.auth.reload_scopes,
        );
        Some(crate::auth::AuthState {
            cfg: Arc::new(cfg.auth.clone()),
            jwks,
        })
    } else {
        None
    };

    log::info!(
        "Listening on http://{}:{}{} ({} backend, {} workers, compression {}, max-body {} bytes, max-page-size {}, timeout {}, shutdown grace {}s)",
        cfg.server.listen,
        cfg.server.port,
        if prefix.is_empty() {
            "".into()
        } else {
            format!("{prefix}/")
        },
        label,
        workers
            .map(|w| w.to_string())
            .unwrap_or_else(|| "auto".into()),
        if compress { "on" } else { "off" },
        max_body,
        max_page_size,
        if timeout_ms == 0 {
            "off".into()
        } else {
            format!("{timeout_ms} ms")
        },
        shutdown_secs,
    );

    log_routes(&prefix, backend.as_ref());

    #[cfg(feature = "docs")]
    if docs_cfg.enabled {
        log::info!("  {}{} (mkdocs site):", prefix, docs_cfg.path);
        log::info!("    GET    {}{}/", prefix, docs_cfg.path);
        log::info!("    GET    {}{}/{{path}}", prefix, docs_cfg.path);
    }

    #[cfg(feature = "swagger")]
    if swagger_cfg.enabled {
        log::info!("  {}{} (swagger UI):", prefix, swagger_cfg.path);
        log::info!("    GET    {}{}/", prefix, swagger_cfg.path);
        log::info!("    GET    {}{}/openapi.json", prefix, swagger_cfg.path);
    }

    #[cfg(feature = "explorer")]
    if explorer_cfg.enabled {
        log::info!("  {}{} (explorer UI):", prefix, explorer_cfg.path);
        log::info!("    GET    {}{}/", prefix, explorer_cfg.path);
        log::info!(
            "    GET    {}{}/datasets/{{name}}",
            prefix,
            explorer_cfg.path
        );
    }

    // Resolve the Swagger UI's OIDC login endpoints once, before binding.
    // We emit an explicit `oauth2` authorizationCode flow in the spec (see
    // `swagger::ResolvedOAuth2`); discovering the authorize/token URLs here
    // keeps the operator-facing config to just an `issuer`. On failure we
    // log and serve the docs *without* a login button rather than shipping
    // an empty Authorize dialog.
    #[cfg(feature = "swagger")]
    let swagger_oauth2 = if swagger_cfg.enabled {
        match swagger_cfg.oauth2.as_ref() {
            Some(o) => match crate::swagger::resolve_oauth2(o).await {
                Ok(resolved) => Some(resolved),
                Err(e) => {
                    log::warn!(
                        "[swagger.oauth2] OIDC discovery for issuer {} failed ({e}); \
                         serving docs without the Authorize button",
                        o.issuer
                    );
                    None
                }
            },
            None => None,
        }
    } else {
        None
    };

    // Build the Prometheus middleware once, outside the worker closure, so
    // every worker shares a single registry (counts aggregate correctly).
    // Constructed whenever the feature is compiled; the runtime `enabled`
    // flag gates whether it is actually wrapped (and the endpoint served).
    //
    // The endpoint path is `{prefix}{metrics.path}` so it lands under the
    // configured prefix like every other route.
    #[cfg(feature = "metrics")]
    let metrics_mount = format!("{prefix}{}", metrics_cfg.path);
    #[cfg(feature = "metrics")]
    let (prometheus, datapress_metrics) = {
        use actix_web_prom::PrometheusMetricsBuilder;
        use std::sync::Arc;
        // Create a shared prometheus::Registry so our custom metrics and the
        // actix-web-prom HTTP metrics all land in the same scrape target.
        let reg = prometheus::Registry::new();
        // Register our custom refresh/dataset metrics (T5.3) on this registry.
        let dm = crate::metrics::DatapressMetrics::register(&reg)
            .map_err(|e| std::io::Error::other(format!("metrics register failed: {e}")))?;
        let prom = PrometheusMetricsBuilder::new("datapress")
            .endpoint(metrics_mount.as_str())
            .registry(reg)
            .build()
            .map_err(|e| std::io::Error::other(format!("metrics init failed: {e}")))?;
        (prom, Arc::new(dm))
    };
    #[cfg(feature = "metrics")]
    let metrics_enabled = metrics_cfg.enabled;

    #[cfg(feature = "metrics")]
    if metrics_cfg.enabled {
        log::info!("  {}{} (prometheus metrics):", prefix, metrics_cfg.path);
        log::info!("    GET    {}{}", prefix, metrics_cfg.path);
    }

    // Compute the prefixed mount strings for docs / swagger / explorer once,
    // before the HttpServer closure, so workers can clone strings rather than
    // reformat them on every request.
    #[cfg(feature = "docs")]
    let docs_mount = format!("{prefix}{}", docs_cfg.path);
    #[cfg(feature = "swagger")]
    let swagger_mount = format!("{prefix}{}", swagger_cfg.path);
    #[cfg(feature = "explorer")]
    let explorer_mount = format!("{prefix}{}", explorer_cfg.path);

    let build_info = web::Data::new(
        handlers::BuildInfo::new(
            // `&'static str` so it fits BuildInfo's compile-time fields.
            // The match keeps this generic enough for future backends.
            match label {
                "DuckDB" => "DuckDB",
                "DataFusion" => "DataFusion",
                _ => "unknown",
            },
        )
        .with_storage_backend(cfg.server.storage.as_ref().map(|s| {
            use crate::config::StorageBackendKind;
            match s.backend {
                StorageBackendKind::Local => "local".to_string(),
                StorageBackendKind::S3 => "s3".to_string(),
            }
        })),
    );

    // One Parquet export cache shared across all workers (it wraps an Arc),
    // so a dataset is encoded at most once and every worker serves the same
    // bytes for the ranged requests a Parquet reader makes.
    let parquet_cache = web::Data::new(handlers::ParquetCache::default());

    // One shared explorer state across all workers (it wraps an Arc backend).
    // Built once here; each worker clones the `web::Data` handle.
    #[cfg(feature = "explorer")]
    let explorer_state = if explorer_cfg.enabled {
        // "Docs" link target: the locally-mounted MkDocs site when it is
        // both compiled in and enabled, otherwise the public docs site.
        let docs_url = {
            #[cfg(feature = "docs")]
            {
                if docs_cfg.enabled {
                    docs_mount.clone()
                } else {
                    "https://docs.datap-rs.org".to_string()
                }
            }
            #[cfg(not(feature = "docs"))]
            {
                "https://docs.datap-rs.org".to_string()
            }
        };
        // "API" link target: the locally-mounted Swagger UI when it is both
        // compiled in and enabled; `None` hides the link.
        let swagger_url = {
            #[cfg(feature = "swagger")]
            {
                if swagger_cfg.enabled {
                    Some(format!("{swagger_mount}/"))
                } else {
                    None
                }
            }
            #[cfg(not(feature = "swagger"))]
            {
                None::<String>
            }
        };
        // Resolve the explorer's OIDC login endpoints once, before binding —
        // same discovery path the Swagger UI uses. Drives the API Query tab's
        // Authorization Code + PKCE login. On failure we log and serve the
        // explorer without a Login button rather than a broken dialog.
        let explorer_oauth2 = match explorer_cfg.oauth2.as_ref() {
            Some(o) => match crate::oauth2::resolve_oauth2(o).await {
                Ok(resolved) => Some(resolved),
                Err(e) => {
                    log::warn!(
                        "[explorer.oauth2] OIDC discovery for issuer {} failed ({e}); \
                         serving the explorer without the Login button",
                        o.issuer
                    );
                    None
                }
            },
            None => None,
        };
        Some(web::Data::new(crate::explorer::ExplorerState {
            backend: backend.clone(),
            datasets: std::sync::RwLock::new(cfg.datasets.clone()),
            explorer_base: explorer_mount.clone(),
            api_base: format!("{prefix}/api/v1"),
            backend_label: label.to_string(),
            sql_enabled: cfg.sql.enabled,
            docs_url,
            swagger_url,
            oauth2: explorer_oauth2,
            environment: cfg.server.environment.clone(),
            environment_color: cfg.server.environment_color.clone(),
            queries_enabled: queries_api_enabled,
            storage_backend: cfg.server.storage.as_ref().map(|s| {
                use crate::config::StorageBackendKind;
                match s.backend {
                    StorageBackendKind::Local => "local".to_string(),
                    StorageBackendKind::S3 => "s3".to_string(),
                }
            }),
            saved_queries_dir: saved_queries_dir.clone(),
        }))
    } else {
        None
    };

    // Create the TTL channel before the HttpServer closure so `ttl_handle`
    // can be cloned into app data. The receiver is consumed by the scheduler
    // loop below.
    let (ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, String)>();
    let ttl_handle = TtlHandle::new(ttl_tx);

    #[cfg(not(feature = "mcp"))]
    if cfg.mcp.enabled {
        log::warn!(
            "[mcp] enabled = true in config, but this binary was built \
             without --features mcp; skipping MCP endpoint"
        );
    }

    // Build the MCP settings and compute its prefixed mount string.
    #[cfg(feature = "mcp")]
    let mcp_cfg = cfg.mcp.clone();
    #[cfg(feature = "mcp")]
    let mcp_mount = format!("{prefix}{}", mcp_cfg.path);
    #[cfg(feature = "mcp")]
    let mcp_settings = web::Data::new(crate::mcp::http::McpSettings {
        enabled: mcp_cfg.enabled,
        mcp: mcp_cfg.clone(),
        sql: cfg.sql.clone(),
        max_page_size,
        own_host: format!("{}:{}", cfg.server.listen, cfg.server.port),
    });

    #[cfg(feature = "mcp")]
    if mcp_cfg.enabled {
        log::info!("  {mcp_mount} (MCP endpoint):");
        log::info!("    POST   {mcp_mount}");
        log::info!("    DELETE {mcp_mount}");
    }

    // Clone backend for the scheduler BEFORE it is moved into HttpServer::new.
    let scheduler_backend = backend.clone();
    // Clone again to install the cascade handle after spawn (both need the
    // concrete type to call set_cascade_handle via the Backend trait).
    let scheduler_backend_for_cascade = backend.clone();
    // Clone DatapressMetrics for the scheduler before the HttpServer closure
    // moves the original.
    #[cfg(feature = "metrics")]
    let scheduler_metrics = datapress_metrics.clone();

    let mut server = HttpServer::new(move || {
        let backend = backend.clone();
        let prefix = prefix.clone();
        let json_cfg = web::JsonConfig::default().limit(max_body);
        let pay_cfg = web::PayloadConfig::default().limit(max_body);
        let query_limits = handlers::QueryLimits { max_page_size };
        let timeout = Timeout::new(Duration::from_millis(timeout_ms.max(1)));
        #[cfg(feature = "docs")]
        let docs_mount = docs_mount.clone();
        #[cfg(feature = "docs")]
        let docs_cfg = docs_cfg.clone();
        #[cfg(feature = "explorer")]
        let explorer_state = explorer_state.clone();
        #[cfg(feature = "swagger")]
        let swagger_mount = swagger_mount.clone();
        #[cfg(feature = "swagger")]
        let swagger_cfg = swagger_cfg.clone();
        #[cfg(feature = "swagger")]
        let swagger_oauth2 = swagger_oauth2.clone();
        #[cfg(feature = "auth")]
        let auth_state = auth_state.clone();
        #[cfg(feature = "metrics")]
        let prometheus = prometheus.clone();
        #[cfg(feature = "metrics")]
        let datapress_metrics = datapress_metrics.clone();
        #[cfg(feature = "mcp")]
        let mcp_mount = mcp_mount.clone();
        #[cfg(feature = "mcp")]
        let mcp_settings = mcp_settings.clone();
        let app = App::new()
            .app_data(web::Data::new(backend))
            .app_data(build_info.clone())
            .app_data(web::Data::new(query_limits))
            .app_data(web::Data::new(sql_settings))
            .app_data(web::Data::new(readiness_settings.clone()))
            .app_data(parquet_cache.clone())
            .app_data(web::Data::new(ttl_handle.clone()))
            .app_data(web::Data::new(handlers::SavedQueriesSettings {
                dir: saved_queries_dir.clone(),
                enabled: queries_api_enabled,
            }))
            .app_data(json_cfg)
            .app_data(pay_cfg);
        #[cfg(feature = "metrics")]
        let app = app.app_data(web::Data::from(datapress_metrics));
        let app = app
            .wrap(middleware::Condition::new(timeout_ms > 0, timeout))
            .wrap(middleware::Condition::new(
                compress,
                middleware::Compress::default(),
            ))
            .wrap(middleware::Logger::new("%a \"%r\" %s %b bytes %Dms"));
        // Auth middleware wraps everything below, including the prefix
        // scope. Probes live under the prefix scope but remain
        // unauthenticated because their handlers require no scope — not
        // because of any path exemption in the auth middleware. When auth
        // is disabled the middleware is a pass-through.
        #[cfg(feature = "auth")]
        let app = match auth_state.clone() {
            Some(state) => app
                .app_data(web::Data::new(state.cfg.clone()))
                .wrap(crate::auth::Auth::new(state)),
            None => app.wrap(crate::auth::Auth::disabled()),
        };
        // Prometheus middleware sits OUTERMOST (added last → runs first) so
        // it observes every request — including those auth rejects — and so
        // the `/metrics` scrape it serves bypasses the auth layer entirely.
        // `Condition` makes it a pass-through (and suppresses the endpoint)
        // when `[metrics].enabled = false`.
        #[cfg(feature = "metrics")]
        let app = app.wrap(middleware::Condition::new(metrics_enabled, prometheus));
        // Docs + swagger + explorer are registered BEFORE the
        // `web::scope(prefix)` catch-all below. An empty `prefix` (the
        // default) becomes `web::scope("")` which matches every path and
        // 404s any miss *inside* the scope — so services registered after
        // it become unreachable. Keeping these at the top of the dispatch
        // chain sidesteps that. They are served at their prefixed mount
        // strings (computed once before this closure from `{prefix}{path}`).
        #[cfg(feature = "docs")]
        let app = if docs_cfg.enabled {
            app.configure(|c| crate::docs::configure(&docs_mount, c))
        } else {
            app
        };
        #[cfg(feature = "swagger")]
        let app = if swagger_cfg.enabled {
            app.configure(|c| {
                crate::swagger::configure(&swagger_mount, swagger_oauth2.as_ref(), &prefix, c)
            })
        } else {
            app
        };
        // Explorer UI — registered (like docs/swagger) BEFORE the
        // `web::scope(prefix)` catch-all so an empty prefix can't shadow it.
        #[cfg(feature = "explorer")]
        let app = match explorer_state {
            Some(state) => app.configure(|c| crate::explorer::configure(state, c)),
            None => app,
        };
        // MCP endpoint — registered BEFORE the prefix scope catch-all for
        // the same reason as docs/swagger/explorer.
        #[cfg(feature = "mcp")]
        let app = {
            let mcp_settings_clone = mcp_settings.clone();
            app.configure(|c| {
                crate::mcp::http::configure(&mcp_mount, mcp_settings_clone, c)
            })
        };
        // MCP OAuth2 protected-resource metadata — only when both auth and
        // mcp features are on and mcp is runtime-enabled. Served at the
        // bare root (no prefix) as required by RFC 9728.
        #[cfg(all(feature = "mcp", feature = "auth"))]
        let app = {
            use crate::mcp::http::OAuthProtectedResourceSettings;
            let mcp_s = mcp_settings.get_ref();
            let auth_cfg_opt = auth_state.as_ref().map(|s| s.cfg.clone());
            if mcp_s.enabled {
                if let Some(auth_cfg) = auth_cfg_opt.as_ref().filter(|a| a.enabled) {
                    let own_host = mcp_s.own_host.clone();
                    let resource_settings = web::Data::new(OAuthProtectedResourceSettings {
                        resource: format!("http://{own_host}/"),
                        issuer: auth_cfg.issuer.clone(),
                        scopes_supported: auth_cfg.read_scopes.clone(),
                    });
                    app.route(
                        "/.well-known/oauth-protected-resource",
                        web::get().to(crate::mcp::http::handle_oauth_protected_resource),
                    ).app_data(resource_settings)
                } else {
                    app
                }
            } else {
                app
            }
        };
        app.service(
            web::scope(prefix.as_str())
                .service(handlers::healthz)
                .service(handlers::readyz)
                .service(handlers::version)
                .service(handlers::health)
                // Canonical, versioned API — the only API mount.
                .service(web::scope("/api/v1").configure(handlers::v1::configure)),
        )
    });
    if let Some(w) = workers {
        server = server.workers(w);
    }

    // Build the refresh scheduler (R3.1) from datasets with interval schedules.
    // Only `kind = "query"` datasets with a `refresh.interval` are scheduled
    // (validated at config load time: non-query datasets can't have [refresh]).
    let refresh_schedules: Vec<DatasetSchedule> = cfg
        .datasets
        .iter()
        .filter_map(|d| {
            let rc = d.refresh.as_ref()?;
            let interval = rc.interval?;
            Some(DatasetSchedule {
                name: d.name.clone(),
                interval,
                timeout: rc.timeout,
                jitter: rc.jitter,
            })
        })
        .collect();
    let refresh_max_concurrent = cfg.server.refresh.max_concurrent;

    // Build the cascade DAG (R4.3) from datasets with on_upstream_reload = true.
    let mut cascade_dag: CascadeDag = CascadeDag::new();
    for d in &cfg.datasets {
        let rc = match d.refresh.as_ref() {
            Some(rc) if rc.on_upstream_reload => rc,
            _ => continue,
        };
        let timeout = rc.timeout;
        let debounce = rc.debounce;
        for upstream in &d.source.depends_on {
            cascade_dag
                .entry(upstream.clone())
                .or_default()
                .push(CascadeDep {
                    name: d.name.clone(),
                    debounce,
                    timeout,
                });
        }
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

    // Shutdown token shared between the OS-signal listener and the scheduler.
    let scheduler_token = tokio_util::sync::CancellationToken::new();

    // Always spawn the refresh scheduler — even when there are no periodic
    // schedules or cascade edges, the scheduler loop handles TTL deletions
    // for `kind = "temp"` datasets from the queries API (R8.1).
    let has_schedules_or_cascade = !refresh_schedules.is_empty() || !cascade_dag.is_empty();
    if has_schedules_or_cascade {
        log::info!(
            "[refresh] starting scheduler: {} scheduled dataset(s), {} cascade upstream(s), \
             max_concurrent={}",
            refresh_schedules.len(),
            cascade_dag.len(),
            refresh_max_concurrent,
        );
    }
    let sched = RefreshScheduler::new(refresh_schedules, refresh_max_concurrent);
    #[cfg(feature = "metrics")]
    let sched = sched.with_metrics(scheduler_metrics);
    let result = sched.spawn(
        scheduler_backend,
        scheduler_token.clone(),
        cascade_dag,
        Some(ttl_rx),
    );
    // Install cascade handle on the backend so publishes trigger cascades.
    if let Some(handle) = result.cascade_handle {
        scheduler_backend_for_cascade.set_cascade_handle(handle);
    }
    let scheduler_handles = result.handles;

    tokio::spawn(shutdown_listener(
        handle,
        shutdown_secs,
        shutdown,
        scheduler_token,
        scheduler_handles,
    ));

    running.await
}

/// Wait for the configured shutdown trigger (OS signal or an external
/// future), log it, then ask the actix server handle to stop gracefully
/// and drain the refresh scheduler.
///
/// # Second-signal force-quit
///
/// When the shutdown trigger is `Shutdown::Signals`, a **second** SIGINT or
/// SIGTERM received *during* the graceful drain logs a single WARN line and
/// calls [`std::process::exit(130)`] immediately — the same exit code that
/// a SIGINT-killed process would have in a POSIX shell.  This matches the
/// widely-expected Ctrl-C behaviour: one press → drain, two presses → quit
/// now.  The external-future path (`Shutdown::External`) is intentionally
/// not affected: the host process owns signal semantics there.
async fn shutdown_listener(
    handle: actix_web::dev::ServerHandle,
    grace_secs: u64,
    shutdown: Shutdown,
    scheduler_token: tokio_util::sync::CancellationToken,
    scheduler_handles: Vec<tokio::task::JoinHandle<()>>,
) {
    match shutdown {
        Shutdown::Signals => {
            let which = wait_for_signal().await;
            log::info!(
                "Received {which}, shutting down gracefully \
                 (up to {grace_secs}s for in-flight requests)..."
            );

            // Spawn a background task that watches for a *second* signal
            // during the drain window.  If one arrives, exit immediately.
            tokio::spawn(async move {
                let which2 = wait_for_signal().await;
                log::warn!(
                    "Received {which2} a second time — forcing immediate shutdown (exit 130)"
                );
                std::process::exit(130);
            });
        }
        Shutdown::External(fut) => {
            fut.await;
            log::info!(
                "Shutdown requested by host, draining in-flight requests (up to {grace_secs}s)..."
            );
        }
    }

    // Signal the scheduler (and cascade engine) to stop (R3.6).
    scheduler_token.cancel();

    // Stop the HTTP server (graceful drain).
    handle.stop(true).await;

    // Wait for all scheduler / cascade engine tasks within the grace period.
    if !scheduler_handles.is_empty() {
        let deadline = Duration::from_secs(grace_secs);
        for jh in scheduler_handles {
            match tokio::time::timeout(deadline, jh).await {
                Ok(_) => {}
                Err(_) => {
                    log::warn!(
                        "[refresh] scheduler/cascade task did not finish within \
                         {}s shutdown deadline; abandoning",
                        grace_secs
                    );
                }
            }
        }
    }

    log::info!("Shutdown complete.");
}

#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    // `expect` is OK here: failing to install a signal handler at startup
    // is a misconfigured runtime, not a recoverable condition.
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
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
///   - general routes (probes, health) — all mounted under the configured
///     `server.prefix` (empty string when no prefix is set).
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
        ("GET", format!("{p}/healthz")),
        ("GET", format!("{p}/readyz")),
        ("GET", format!("{p}/version")),
        ("GET", format!("{p}/health")),
    ] {
        log::info!("    {:<width$} {}", method, path, width = METHOD_W);
    }

    // Only the canonical versioned API scope.
    let mounts: &[(&str, &[(&str, &str)])] = &[("/api/v1", handlers::v1::ROUTES)];

    let names = backend.names();
    for (mount, routes) in mounts {
        log::info!("  {p}{mount}:");
        // Top-level (non-dataset-scoped) routes for this version.
        for (method, suffix) in *routes {
            if !suffix.contains("{name}") {
                log::info!(
                    "    {:<width$} {p}{mount}{suffix}",
                    method,
                    width = METHOD_W,
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
                        method,
                        width = METHOD_W,
                    );
                }
            }
        }
    }
}
