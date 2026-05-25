//! Runtime configuration loaded from `datasets.toml`.
//!
//! Each instance binds to a list of datasets. A dataset's `[dataset.source]`
//! block selects the format (`parquet` or `delta`) and the location (a
//! local path or an `s3://bucket/key` URL). When the location is on S3,
//! an optional `[dataset.s3]` block carries non-secret connection details
//! (region, endpoint, addressing style, …).
//!
//! Credentials are resolved at runtime via [`DatasetConfig::resolved_creds`]
//! in this precedence order:
//!
//! 1. Per-dataset env vars `${PREFIX}_AWS_ACCESS_KEY_ID`,
//!    `${PREFIX}_AWS_SECRET_ACCESS_KEY`, `${PREFIX}_AWS_SESSION_TOKEN`
//!    where `${PREFIX}` is the dataset name uppercased with non-alphanumeric
//!    characters replaced by `_` (e.g. `accidents` → `ACCIDENTS`,
//!    `sales.eu-1` → `SALES_EU_1`).
//! 2. Inline `access_key_id` / `secret_access_key` / `session_token` in the
//!    `[dataset.s3]` block.
//! 3. Plain `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
//!    `AWS_SESSION_TOKEN`.
//! 4. None — fall back to the engine's own provider chain
//!    (`~/.aws/credentials`, IMDS, …).

use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::errors::AppError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server:   ServerConfig,
    #[serde(rename = "dataset", default)]
    pub datasets: Vec<DatasetConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Which engine to run. Must match the binary's compile-time feature.
    pub backend: Backend,
    /// Listen address. Defaults to loopback (127.0.0.1) — explicitly opt in
    /// to 0.0.0.0 if you want to expose the port.
    pub listen:  IpAddr,
    /// TCP port.
    pub port:    u16,
    /// Number of actix worker threads. `None` (= unset) → one per CPU.
    pub workers: Option<usize>,
    /// Optional URL path prefix — useful when sitting behind a reverse
    /// proxy that rewrites e.g. `/datapress/...` → `/...`. When set, every
    /// route is mounted under this prefix (so the proxy can pass the URL
    /// through unchanged). Must start with `/` and not end with `/`; the
    /// empty string (default) means no prefix.
    pub prefix:  String,
    /// Negotiate response compression (gzip / brotli / zstd) via the
    /// `Accept-Encoding` request header. Enabled by default. Disable when
    /// running behind a proxy that already compresses, or when the extra
    /// CPU is not worth the bandwidth saving.
    pub compress: bool,
    /// Maximum accepted JSON request body size, in bytes. Larger bodies
    /// are rejected with `413 Payload Too Large` before any handler runs.
    /// Default `1 MiB`. Most query bodies are well under 10 KiB; this is
    /// a DoS guard, not a tuning knob.
    pub max_body_bytes: usize,
    /// Per-request handler timeout, in milliseconds. If a handler hasn't
    /// produced a response within this budget the request is aborted with
    /// `504 Gateway Timeout`. Default `30_000` (30 s). Set `0` to disable.
    pub request_timeout_ms: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            listen:  IpAddr::from([127, 0, 0, 1]),
            port:    8080,
            workers: None,
            prefix:  String::new(),
            compress: true,
            max_body_bytes:     1024 * 1024,
            request_timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Datafusion,
    Duckdb,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Datafusion => "datafusion",
            Backend::Duckdb     => "duckdb",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatasetConfig {
    pub name:   String,
    pub source: SourceConfig,
    #[serde(default)]
    pub s3:     Option<S3Config>,
    #[serde(default)]
    pub index:  IndexConfig,
    /// Optional column projection applied at load time. When non-empty,
    /// only the listed columns are read from the parquet/delta source —
    /// every other column is skipped entirely (no decode, no allocation,
    /// no resident memory). Empty (default) = read all columns. Names are
    /// matched case-insensitively against the source schema.
    #[serde(default)]
    pub columns: Vec<String>,
    /// When `true` (default), Utf8 columns that are dictionary-encoded in
    /// the source parquet are read as Arrow `Dictionary(Int32, Utf8)`
    /// instead of being expanded to plain Utf8. Massively cheaper in RAM
    /// for low-cardinality columns. Set to `false` to bypass the override
    /// — useful as a workaround if you observe null-handling oddities on
    /// a particular parquet file.
    #[serde(default = "default_true")]
    pub dict_encode: bool,
    /// When `true`, the backend should keep the dataset on disk and stream
    /// it at query time instead of materialising it into RAM at startup.
    /// Trades the in-memory hot paths (raw Arrow slice, equality index)
    /// for bounded memory use on large / multi-file sources. Currently
    /// honoured by the DataFusion backend for local parquet.
    #[serde(default)]
    pub lazy:   bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub kind:     SourceKind,
    /// Either a local filesystem path or an `s3://bucket/key` URL.
    pub location: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Parquet,
    Delta,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Parquet => "parquet",
            SourceKind::Delta   => "delta",
        }
    }
}

/// Non-secret S3 connection settings. Credentials are pulled from env / the
/// AWS credential chain — see [`DatasetConfig::resolved_creds`].
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct S3Config {
    pub region:           Option<String>,
    /// Custom endpoint (MinIO, R2, Wasabi, LocalStack, …). Omit for AWS.
    pub endpoint:         Option<String>,
    /// `virtual` (default — `bucket.host`) or `path` (`host/bucket/`).
    /// MinIO and most non-AWS providers require `path`.
    pub addressing_style: AddressingStyle,
    /// Allow plain-HTTP endpoints. Required for local MinIO over `http://…`.
    pub allow_http:       bool,
    /// Inline credentials. Strongly discouraged in production — prefer env
    /// vars (see module docs).
    pub access_key_id:     Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token:     Option<String>,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            region:            None,
            endpoint:          None,
            addressing_style:  AddressingStyle::Virtual,
            allow_http:        false,
            access_key_id:     None,
            secret_access_key: None,
            session_token:     None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressingStyle {
    #[default]
    Virtual,
    Path,
}

impl AddressingStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            AddressingStyle::Virtual => "virtual",
            AddressingStyle::Path    => "path",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

/// Resolved S3 credentials. `None` fields mean "let the engine's default
/// provider chain figure it out".
#[derive(Debug, Clone, Default)]
pub struct ResolvedCreds {
    pub access_key_id:     Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token:     Option<String>,
}

impl ResolvedCreds {
    pub fn has_keypair(&self) -> bool {
        self.access_key_id.is_some() && self.secret_access_key.is_some()
    }
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
        // Server prefix: empty, or must start with '/' and not end with '/'.
        let p = &self.server.prefix;
        if !p.is_empty() {
            if !p.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "server.prefix must start with '/' (got '{p}')"
                )));
            }
            if p.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "server.prefix must not end with '/' (got '{p}')"
                )));
            }
        }

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

            // Location-specific checks.
            if d.source.is_s3() {
                d.source.s3_bucket()?;
                if d.s3.as_ref().and_then(|s| s.region.as_deref()).is_none()
                    && d.s3.as_ref().and_then(|s| s.endpoint.as_deref()).is_none()
                    && std::env::var("AWS_REGION").is_err()
                    && std::env::var("AWS_DEFAULT_REGION").is_err()
                {
                    log::warn!(
                        "dataset '{}': S3 source without explicit region — \
                         relying on AWS_REGION env var",
                        d.name
                    );
                }
            } else {
                // Local path. For parquet we can fully resolve to a file
                // list up front; for delta we only check that the directory
                // exists (delta has its own layout — _delta_log/, …).
                match d.source.kind {
                    SourceKind::Parquet => { d.resolve_local_parquet_files()?; }
                    SourceKind::Delta   => {
                        let p = Path::new(&d.source.location);
                        if !p.exists() {
                            return Err(AppError::Internal(format!(
                                "dataset '{}': delta location does not exist: {}",
                                d.name, d.source.location
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl SourceConfig {
    pub fn is_s3(&self) -> bool {
        self.location.starts_with("s3://")
    }

    /// Returns `(bucket, key_prefix_or_empty)` for an `s3://…` location.
    pub fn s3_bucket(&self) -> Result<(&str, &str), AppError> {
        let rest = self.location.strip_prefix("s3://")
            .ok_or_else(|| AppError::Internal(format!(
                "not an s3:// URL: {}", self.location
            )))?;
        let (bucket, key) = match rest.split_once('/') {
            Some((b, k)) => (b, k),
            None         => (rest, ""),
        };
        if bucket.is_empty() {
            return Err(AppError::Internal(format!(
                "s3 URL missing bucket: {}", self.location
            )));
        }
        Ok((bucket, key))
    }
}

impl DatasetConfig {
    /// Expand `source.location` to a concrete list of local `.parquet`
    /// files. Only valid for `kind = parquet` on local paths — S3 and
    /// Delta sources are resolved by the backend itself.
    ///
    /// Accepts three location shapes:
    ///   * a single `*.parquet` file
    ///   * a directory (lists every `*.parquet` directly inside, non-recursive)
    ///   * a glob pattern containing `*`, `?` or `[…]` (e.g.
    ///     `data/year=2024/*.parquet`, `data/**/*.parquet`)
    pub fn resolve_local_parquet_files(&self) -> Result<Vec<PathBuf>, AppError> {
        if self.source.is_s3() {
            return Err(AppError::Internal(format!(
                "dataset '{}': resolve_local_parquet_files called on s3 source",
                self.name
            )));
        }
        let loc = &self.source.location;

        // Glob pattern? Expand and require at least one match.
        if loc.contains('*') || loc.contains('?') || loc.contains('[') {
            let mut files: Vec<PathBuf> = glob::glob(loc)
                .map_err(|e| AppError::Internal(format!(
                    "dataset '{}': bad glob pattern '{loc}': {e}", self.name
                )))?
                .filter_map(|r| r.ok())
                .filter(|p| p.is_file()
                    && p.extension().and_then(|e| e.to_str()) == Some("parquet"))
                .collect();
            files.sort();
            if files.is_empty() {
                return Err(AppError::Internal(format!(
                    "dataset '{}': glob '{loc}' matched no .parquet files",
                    self.name
                )));
            }
            return Ok(files);
        }

        let path = Path::new(loc);
        if !path.exists() {
            return Err(AppError::Internal(format!(
                "dataset '{}': source path does not exist: {loc}", self.name
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

        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| AppError::Internal(format!("read {loc}: {e}")))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(AppError::Internal(format!(
                "dataset '{}': no *.parquet files found in {loc}",
                self.name
            )));
        }
        Ok(files)
    }

    /// Env-var prefix derived from the dataset name: uppercase with
    /// non-alphanumeric chars replaced by `_`. E.g. `sales.eu-1` →
    /// `SALES_EU_1`.
    pub fn env_prefix(&self) -> String {
        self.name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect()
    }

    /// Resolve S3 credentials following the precedence chain documented at
    /// the top of this module. Returns an empty struct when nothing was
    /// found — the caller should then leave credential resolution to the
    /// engine's default provider chain.
    pub fn resolved_creds(&self) -> ResolvedCreds {
        let prefix = self.env_prefix();
        let from_env = |suffix: &str| {
            std::env::var(format!("{prefix}_{suffix}")).ok()
                .filter(|s| !s.is_empty())
        };
        let inline = self.s3.as_ref();
        let plain_env = |k: &str| {
            std::env::var(k).ok().filter(|s| !s.is_empty())
        };

        ResolvedCreds {
            access_key_id: from_env("AWS_ACCESS_KEY_ID")
                .or_else(|| inline.and_then(|s| s.access_key_id.clone()))
                .or_else(|| plain_env("AWS_ACCESS_KEY_ID")),
            secret_access_key: from_env("AWS_SECRET_ACCESS_KEY")
                .or_else(|| inline.and_then(|s| s.secret_access_key.clone()))
                .or_else(|| plain_env("AWS_SECRET_ACCESS_KEY")),
            session_token: from_env("AWS_SESSION_TOKEN")
                .or_else(|| inline.and_then(|s| s.session_token.clone()))
                .or_else(|| plain_env("AWS_SESSION_TOKEN")),
        }
    }

    /// Resolved S3 region: per-dataset env (`${PREFIX}_AWS_REGION`)
    /// → inline → `AWS_REGION` → `AWS_DEFAULT_REGION` → `us-east-1`.
    pub fn resolved_region(&self) -> String {
        let prefix = self.env_prefix();
        std::env::var(format!("{prefix}_AWS_REGION")).ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3.as_ref().and_then(|s| s.region.clone()))
            .or_else(|| std::env::var("AWS_REGION").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "us-east-1".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_defaults() {
        let s = ServerConfig::default();
        assert_eq!(s.backend, Backend::Datafusion);
        assert_eq!(s.port, 8080);
        assert!(s.compress);
        assert_eq!(s.max_body_bytes, 1024 * 1024);
        assert_eq!(s.request_timeout_ms, 30_000);
        assert_eq!(s.prefix, "");
        assert!(s.listen.is_loopback());
    }

    #[test]
    fn server_overrides_from_toml() {
        let toml = r#"
            [server]
            backend = "duckdb"
            port = 9000
            prefix = "/datapress"
            compress = false
            max_body_bytes = 4096
            request_timeout_ms = 0
            [[dataset]]
            name = "x"
            source.kind = "parquet"
            source.location = "/tmp/missing.parquet"
        "#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.server.backend, Backend::Duckdb);
        assert_eq!(cfg.server.port, 9000);
        assert_eq!(cfg.server.prefix, "/datapress");
        assert!(!cfg.server.compress);
        assert_eq!(cfg.server.max_body_bytes, 4096);
        assert_eq!(cfg.server.request_timeout_ms, 0);
        assert_eq!(cfg.datasets.len(), 1);
        assert_eq!(cfg.datasets[0].name, "x");
        assert!(cfg.datasets[0].dict_encode); // default
    }

    #[test]
    fn validate_rejects_bad_prefix() {
        let bad = ["no-leading-slash", "/trailing/"];
        for p in bad {
            let cfg = AppConfig {
                server: ServerConfig { prefix: p.to_string(), ..Default::default() },
                datasets: vec![],
            };
            assert!(cfg.validate().is_err(), "prefix {p:?} should fail");
        }
    }

    #[test]
    fn validate_rejects_no_datasets() {
        let cfg = AppConfig {
            server: ServerConfig::default(),
            datasets: vec![],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("[[dataset]]")));
    }

    #[test]
    fn validate_rejects_bad_dataset_name() {
        let cfg: AppConfig = toml::from_str(r#"
            [[dataset]]
            name = "bad name!"
            source.kind = "parquet"
            source.location = "/tmp/whatever"
        "#).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("alphanumeric")));
    }

    #[test]
    fn validate_rejects_duplicate_names() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "dp-dup-test-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.parquet");
        std::fs::File::create(&f).unwrap().write_all(b"x").unwrap();
        let path = f.to_str().unwrap();

        let cfg: AppConfig = toml::from_str(&format!(r#"
            [[dataset]]
            name = "a"
            source.kind = "parquet"
            source.location = "{path}"
            [[dataset]]
            name = "a"
            source.kind = "parquet"
            source.location = "{path}"
        "#)).unwrap();
        let err = cfg.validate().expect_err("expected error");
        assert!(matches!(err, AppError::Internal(m) if m.contains("duplicate")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn s3_bucket_parsing() {
        let mk = |loc: &str| SourceConfig { kind: SourceKind::Parquet, location: loc.into() };
        let s1 = mk("s3://bucket/path/key");
        assert_eq!(s1.s3_bucket().unwrap(), ("bucket", "path/key"));
        let s2 = mk("s3://only-bucket");
        assert_eq!(s2.s3_bucket().unwrap(), ("only-bucket", ""));
        assert!(mk("s3:///nokey").s3_bucket().is_err());
        assert!(mk("/local/path").s3_bucket().is_err());
    }

    #[test]
    fn env_prefix_sanitises_name() {
        let mk = |name: &str| DatasetConfig {
            name: name.into(),
            source: SourceConfig { kind: SourceKind::Parquet, location: "x".into() },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
        };
        assert_eq!(mk("accidents").env_prefix(),  "ACCIDENTS");
        assert_eq!(mk("sales.eu-1").env_prefix(), "SALES_EU_1");
        assert_eq!(mk("a_b.c-d").env_prefix(),    "A_B_C_D");
    }

    #[test]
    fn resolve_local_parquet_single_file_and_dir() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "dp-cfg-test-{}", std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.parquet");
        let mut fh = std::fs::File::create(&f).unwrap();
        fh.write_all(b"not really parquet").unwrap();

        let mk = |loc: &str| DatasetConfig {
            name: "ds".into(),
            source: SourceConfig { kind: SourceKind::Parquet, location: loc.into() },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
        };

        // Direct file.
        let files = mk(f.to_str().unwrap()).resolve_local_parquet_files().unwrap();
        assert_eq!(files, vec![f.clone()]);

        // Directory.
        let files = mk(dir.to_str().unwrap()).resolve_local_parquet_files().unwrap();
        assert_eq!(files, vec![f.clone()]);

        // Missing path.
        assert!(mk("/no/such/place.parquet").resolve_local_parquet_files().is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
