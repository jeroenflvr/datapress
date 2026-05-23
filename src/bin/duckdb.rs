use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use fast_api::config::{AppConfig, Backend};
use fast_api::duckdb_backend::{db, handlers};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path = std::env::var("DATASETS_CONFIG")
        .unwrap_or_else(|_| "datasets.toml".to_string());
    let cfg = AppConfig::load(&config_path).expect("failed to load datasets config");

    if cfg.server.backend != Backend::Duckdb {
        log::warn!(
            "datasets.toml has server.backend = '{}', but this binary is the duckdb build \
             — running as duckdb anyway",
            cfg.server.backend.as_str(),
        );
    }

    let registry = Arc::new(db::load_registry(&cfg).expect("failed to register datasets"));
    let addr     = (cfg.server.listen, cfg.server.port);
    let workers  = cfg.server.workers;

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
