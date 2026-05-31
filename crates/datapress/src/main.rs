use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use datapress_core::config::{AppConfig, Backend};

/// Embedded `datasets.toml` template, written verbatim by `datapress init`.
const CONFIG_TEMPLATE: &str = include_str!("../templates/datasets.toml.template");

/// File name used for both the generated template and the config lookup.
const CONFIG_FILE_NAME: &str = "datasets.toml";

/// Environment variable that, when set, overrides config discovery with an
/// explicit path.
const CONFIG_ENV_VAR: &str = "DATAPRESS_CONFIG_FILE";

/// Unified `datapress` server. Both backends are compiled in (by default)
/// and the active one is chosen at runtime from `server.backend` in the
/// config, so a single `cargo install datapress` ships a binary that can
/// serve either DuckDB or DataFusion without rebuilding.
#[derive(Debug, Parser)]
#[command(name = "datapress", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the config file. Overrides the `DATAPRESS_CONFIG_FILE` env
    /// var and the default discovery order (`./datasets.toml`, then
    /// `$HOME/datasets.toml`).
    #[arg(short, long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write a commented `datasets.toml.template` you can copy and edit.
    Init {
        /// Directory to write `datasets.toml.template` into. Defaults to
        /// your home directory when omitted. The directory is created if
        /// it does not exist.
        location: Option<PathBuf>,

        /// Overwrite an existing template instead of refusing.
        #[arg(short, long)]
        force: bool,
    },
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init { location, force }) => init_config(location, force),
        None => run_server(cli.config).await,
    }
}

/// Load the config (from `--config`, `$DATAPRESS_CONFIG_FILE`, or the
/// default discovery order) and start the server on the configured backend.
async fn run_server(config_override: Option<PathBuf>) -> std::io::Result<()> {
    let config_path = resolve_config_path(config_override)?;
    log::info!("Loading config from {}", config_path.display());

    let cfg = AppConfig::load(&config_path.to_string_lossy())
        .map_err(|e| std::io::Error::other(format!("failed to load config: {e}")))?;

    match cfg.server.backend {
        Backend::Duckdb => serve_duckdb(cfg).await,
        Backend::Datafusion => serve_datafusion(cfg).await,
    }
}

/// Resolve which config file to use.
///
/// Precedence (highest first):
///   1. an explicit `--config` flag,
///   2. the `DATAPRESS_CONFIG_FILE` environment variable,
///   3. `./datasets.toml` in the current working directory,
///   4. `$HOME/datasets.toml`.
fn resolve_config_path(config_override: Option<PathBuf>) -> std::io::Result<PathBuf> {
    if let Some(path) = config_override {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os(CONFIG_ENV_VAR) {
        return Ok(PathBuf::from(path));
    }

    let cwd_cfg = PathBuf::from(CONFIG_FILE_NAME);
    if cwd_cfg.is_file() {
        return Ok(cwd_cfg);
    }
    if let Some(home_cfg) = home_dir().map(|h| h.join(CONFIG_FILE_NAME))
        && home_cfg.is_file()
    {
        return Ok(home_cfg);
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "no config file found. Looked at ${CONFIG_ENV_VAR}, \
             ./{CONFIG_FILE_NAME}, and $HOME/{CONFIG_FILE_NAME}. \
             Run `datapress init` to generate a template, or pass --config <FILE>."
        ),
    ))
}

/// Write `datasets.toml.template` into `location` (or `$HOME` when omitted).
fn init_config(location: Option<PathBuf>, force: bool) -> std::io::Result<()> {
    let dir = match location {
        Some(dir) => dir,
        None => home_dir().ok_or_else(|| {
            std::io::Error::other(
                "could not determine home directory; pass an explicit location: \
                 `datapress init <DIR>`",
            )
        })?,
    };

    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{CONFIG_FILE_NAME}.template"));

    if target.exists() && !force {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; pass --force to overwrite",
                target.display()
            ),
        ));
    }

    std::fs::write(&target, CONFIG_TEMPLATE)?;
    println!("Wrote {}", target.display());
    println!(
        "Copy it to {CONFIG_FILE_NAME} and edit, then run `datapress` (or set \
         ${CONFIG_ENV_VAR})."
    );
    Ok(())
}

/// Best-effort home directory, without pulling in an extra crate.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p: &PathBuf| Path::new(p).is_absolute())
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
