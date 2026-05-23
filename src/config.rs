//! Runtime configuration loaded from `datasets.toml`.
//!
//! Each instance binds to a list of datasets. Backend-specific tuning (the
//! equality-index policy used by the DataFusion backend) lives in a per-
//! dataset `[dataset.index]` block.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::errors::AppError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    #[serde(rename = "dataset", default)]
    pub datasets: Vec<DatasetConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DatasetConfig {
    pub name:   String,
    pub source: String,
    #[serde(default)]
    pub index:  IndexConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    pub mode:            IndexMode,
    pub columns:         Vec<String>,
    pub max_cardinality: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            mode:            IndexMode::Auto,
            columns:         Vec::new(),
            max_cardinality: 100_000,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexMode {
    #[default]
    Auto,
    None,
    List,
}

// ---------------------------------------------------------------------------
// Loading + validation
// ---------------------------------------------------------------------------

impl AppConfig {
    /// Read and validate a TOML config file.
    pub fn load(path: &str) -> Result<Self, AppError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| AppError::Internal(format!("failed to read {path}: {e}")))?;
        let cfg: AppConfig = toml::from_str(&raw)
            .map_err(|e| AppError::Internal(format!("invalid {path}: {e}")))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.datasets.is_empty() {
            return Err(AppError::Internal(
                "datasets.toml has no [[dataset]] entries".into(),
            ));
        }

        let mut seen = HashSet::new();
        for d in &self.datasets {
            if !seen.insert(d.name.as_str()) {
                return Err(AppError::Internal(format!(
                    "duplicate dataset name: {}",
                    d.name
                )));
            }
            if d.name.is_empty() {
                return Err(AppError::Internal(
                    "dataset name must not be empty".into(),
                ));
            }
            // URL-safe: alphanum + _ - .
            if !d.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
                return Err(AppError::Internal(format!(
                    "dataset name '{}' must be alphanumeric (plus _ - .)",
                    d.name
                )));
            }

            if d.index.mode == IndexMode::List && d.index.columns.is_empty() {
                return Err(AppError::Internal(format!(
                    "dataset '{}': index.mode = 'list' requires non-empty index.columns",
                    d.name
                )));
            }

            // Source must resolve to at least one parquet file (file or dir).
            d.resolve_files()?;
        }
        Ok(())
    }
}

impl DatasetConfig {
    /// Expand `source` to a concrete list of `.parquet` files.
    /// `source` is either a single `.parquet` file or a directory containing
    /// one or more `*.parquet` files.
    pub fn resolve_files(&self) -> Result<Vec<PathBuf>, AppError> {
        let path = Path::new(&self.source);
        if !path.exists() {
            return Err(AppError::Internal(format!(
                "dataset '{}': source path does not exist: {}",
                self.name, self.source
            )));
        }

        if path.is_file() {
            if path.extension().and_then(|e| e.to_str()) != Some("parquet") {
                return Err(AppError::Internal(format!(
                    "dataset '{}': source must be a .parquet file",
                    self.name
                )));
            }
            return Ok(vec![path.to_path_buf()]);
        }

        // Directory: collect all *.parquet entries, sorted.
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| AppError::Internal(format!("read {}: {e}", self.source)))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(AppError::Internal(format!(
                "dataset '{}': no *.parquet files found in {}",
                self.name, self.source
            )));
        }
        Ok(files)
    }
}
