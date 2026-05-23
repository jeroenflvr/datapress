use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use fast_api::config::{AppConfig, Backend};
use fast_api::datafusion_backend::{handlers, store::Store};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path = std::env::var("DATASETS_CONFIG")
        .unwrap_or_else(|_| "datasets.toml".to_string());
    let cfg = AppConfig::load(&config_path).expect("failed to load datasets config");

    if cfg.server.backend != Backend::Datafusion {
        log::warn!(
            "datasets.toml has server.backend = '{}', but this binary is the datafusion build \
             — running as datafusion anyway",
            cfg.server.backend.as_str(),
        );
    }

    let store   = Arc::new(Store::load(&cfg).await.expect("failed to load datasets"));
    let addr    = (cfg.server.listen, cfg.server.port);
    let workers = cfg.server.workers;

    log::info!(
        "Listening on http://{}:{} (DataFusion backend, {} workers)",
        cfg.server.listen, cfg.server.port,
        workers.map(|w| w.to_string()).unwrap_or_else(|| "auto".into()),
    );

    let mut server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(store.clone()))
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
