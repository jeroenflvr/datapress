use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};

use fast_api::config::AppConfig;
use fast_api::duckdb_backend::{db, handlers};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path = std::env::var("DATASETS_CONFIG")
        .unwrap_or_else(|_| "datasets.toml".to_string());
    let cfg = AppConfig::load(&config_path).expect("failed to load datasets config");
    let registry = Arc::new(db::load_registry(&cfg).expect("failed to register datasets"));

    log::info!("Listening on http://0.0.0.0:8080 (DuckDB backend)");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(registry.clone()))
            .wrap(middleware::Logger::default())
            .service(handlers::health)
            .service(handlers::list_datasets)
            .service(handlers::get_schema)
            .service(handlers::query_dataset)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
