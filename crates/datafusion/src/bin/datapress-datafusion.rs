use datapress_core::config::{AppConfig, Backend};

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

    datapress_datafusion::serve(cfg).await
}
