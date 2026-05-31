use datapress_core::config::{AppConfig, Backend};

/// Unified `datapress` entry point. Both backends are compiled in (by
/// default) and the active one is chosen at runtime from
/// `server.backend` in the datasets config, so a single
/// `cargo install datap-rs` ships a binary that can serve either DuckDB
/// or DataFusion without rebuilding.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path =
        std::env::var("DATASETS_CONFIG").unwrap_or_else(|_| "datasets.toml".to_string());
    let cfg = AppConfig::load(&config_path).expect("failed to load datasets config");

    match cfg.server.backend {
        Backend::Duckdb => serve_duckdb(cfg).await,
        Backend::Datafusion => serve_datafusion(cfg).await,
    }
}

#[cfg(feature = "duckdb")]
async fn serve_duckdb(cfg: AppConfig) -> std::io::Result<()> {
    datapress_duckdb::serve(cfg).await
}

#[cfg(not(feature = "duckdb"))]
async fn serve_duckdb(_cfg: AppConfig) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "server.backend = 'duckdb' but this binary was built without the `duckdb` feature",
    ))
}

#[cfg(feature = "datafusion")]
async fn serve_datafusion(cfg: AppConfig) -> std::io::Result<()> {
    datapress_datafusion::serve(cfg).await
}

#[cfg(not(feature = "datafusion"))]
async fn serve_datafusion(_cfg: AppConfig) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "server.backend = 'datafusion' but this binary was built without the `datafusion` feature",
    ))
}
