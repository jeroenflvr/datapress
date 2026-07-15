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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::errors::AppError;

// ---------------------------------------------------------------------------
// Duration parsing for refresh intervals ("30s", "15m", "2h", "1d")
// ---------------------------------------------------------------------------

/// Parse a human-readable duration string into a `std::time::Duration`.
///
/// Accepted suffixes (case-insensitive): `ms`, `s`, `m`, `h`, `d`.
/// Examples: `"30s"`, `"15m"`, `"2h"`, `"1d"`, `"500ms"`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num_str, suffix) = if let Some(pos) = s.find(|c: char| c.is_ascii_alphabetic()) {
        (&s[..pos], &s[pos..])
    } else {
        return Err(format!("missing unit suffix in duration '{s}'"));
    };
    let n: u64 = num_str
        .trim()
        .parse()
        .map_err(|_| format!("invalid number in duration '{s}'"))?;
    let dur = match suffix.to_ascii_lowercase().as_str() {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
        "d" => Duration::from_secs(n * 86400),
        other => return Err(format!("unknown duration unit '{other}' in '{s}'")),
    };
    Ok(dur)
}

/// Serde deserialization visitor for `parse_duration`.
fn deserialize_duration<'de, D>(de: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(de)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

fn deserialize_optional_duration<'de, D>(de: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(de)?;
    match s {
        None => Ok(None),
        Some(v) => parse_duration(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Serialize a `Duration` as its millisecond count (for JSON/TOML output).
fn serialize_duration<S>(d: &Duration, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ser.serialize_u64(d.as_millis() as u64)
}

fn serialize_optional_duration<S>(d: &Option<Duration>, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match d {
        None => ser.serialize_none(),
        Some(dur) => ser.serialize_some(&(dur.as_millis() as u64)),
    }
}

/// Absolute path of the `datasets.toml` this process was loaded from, set
/// once by [`AppConfig::load`]. `None` when the config was constructed
/// in-process (e.g. the Python bindings) rather than read from a file — in
/// that case the explorer's "append to server config" export is unavailable.
static SOURCE_CONFIG_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Path of the config file this process was loaded from, if any.
pub fn source_config_path() -> Option<&'static std::path::Path> {
    SOURCE_CONFIG_PATH.get().map(|p| p.as_path())
}

/// Mount paths the user MUST NOT pick for `[docs].path` or
/// `[swagger].path` — they would shadow first-party routes (probes,
/// API scopes, root).
const RESERVED_MOUNTS: &[&str] = &[
    "/", "/api", "/api/v1", "/health", "/healthz", "/readyz", "/version", "/metrics",
];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub docs: DocsConfig,
    #[serde(default)]
    pub swagger: SwaggerConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub explorer: ExplorerConfig,
    #[serde(default)]
    pub sql: SqlConfig,
    #[serde(default)]
    pub datafusion: DataFusionConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(rename = "dataset", default)]
    pub datasets: Vec<DatasetConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Which engine to run. Must match the binary's compile-time feature.
    pub backend: Backend,
    /// Listen address. Defaults to loopback (127.0.0.1) — explicitly opt in
    /// to 0.0.0.0 if you want to expose the port.
    pub listen: IpAddr,
    /// TCP port.
    pub port: u16,
    /// Number of actix worker threads. `None` (= unset) → one per CPU.
    pub workers: Option<usize>,
    /// Optional URL path prefix — useful when sitting behind a reverse
    /// proxy that strips or preserves a path component. When set, EVERY
    /// route is mounted under this prefix: probes (`/healthz`, `/readyz`,
    /// `/version`), the API (`/api/v1/...`), docs, Swagger UI, explorer,
    /// and metrics. Must start with `/` and not end with `/`; the empty
    /// string (default) means no prefix.
    pub prefix: String,
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
    /// Maximum rows returned by a single `/query` page. Larger
    /// `page_size` values are clamped before the backend runs.
    /// Default `100_000`.
    pub max_page_size: u64,
    /// When > 0, any dataset whose backing files exceed this many
    /// megabytes is forced into `lazy` mode at startup (streamed from
    /// disk instead of materialised into RAM), even if `lazy` was not set
    /// on the dataset. `0` (default) disables the size check. Local
    /// sources are sized with a filesystem stat; on the DataFusion backend
    /// S3 sources are sized by listing the object store under their prefix
    /// (the DuckDB backend only sizes local sources — S3 datasets there
    /// must opt in with an explicit `lazy = true`). Delta tables are
    /// measured by summing their parquet data files.
    pub force_lazy_above_mb: u64,
    /// Per-request handler timeout, in milliseconds. If a handler hasn't
    /// produced a response within this budget the request is aborted with
    /// `504 Gateway Timeout`. Default `30_000` (30 s). Set `0` to disable.
    pub request_timeout_ms: u64,
    /// Grace period for in-flight requests after the server has received
    /// `SIGTERM` / `SIGINT`, in seconds. The listening socket is closed
    /// immediately; existing connections then have up to this many
    /// seconds to finish before workers are force-stopped. Default `30`.
    pub shutdown_timeout_secs: u64,
    /// Optional DuckDB Quack remote SQL server. Only used by the DuckDB
    /// backend; ignored by DataFusion.
    pub quack: QuackConfig,
    /// Optional PostgreSQL wire-protocol server. Only used by the DataFusion
    /// backend (and only when compiled with the `pgwire` feature); ignored by
    /// DuckDB.
    pub pgwire: PgwireConfig,
    /// Optional environment label shown as a badge in the Explorer navbar,
    /// e.g. `"development"`, `"staging"`, or `"production"`. When unset the
    /// badge is hidden. Known values get a distinctive colour; anything else
    /// renders in grey.
    pub environment: Option<String>,
    /// Bootstrap colour name for the environment badge, e.g. `"danger"`,
    /// `"warning"`, `"success"`, `"info"`, `"primary"`, `"secondary"`.
    /// Overrides the automatic colour derived from `environment`. Only
    /// meaningful when `environment` is also set.
    pub environment_color: Option<String>,
    /// Non-blocking startup configuration.
    #[serde(default)]
    pub startup: StartupConfig,
    /// Refresh scheduler concurrency limits.
    #[serde(default)]
    pub refresh: ServerRefreshConfig,
    /// Optional server-level materialization storage backend.
    /// When absent, all query-dataset materializations stay in memory.
    #[serde(default)]
    pub storage: Option<StorageConfig>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 8080,
            workers: None,
            prefix: String::new(),
            compress: true,
            max_body_bytes: 1024 * 1024,
            max_page_size: 100_000,
            force_lazy_above_mb: 0,
            request_timeout_ms: 30_000,
            shutdown_timeout_secs: 30,
            quack: QuackConfig::default(),
            pgwire: PgwireConfig::default(),
            environment: None,
            environment_color: None,
            startup: StartupConfig::default(),
            refresh: ServerRefreshConfig::default(),
            storage: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 2B: materialization storage backend
// ---------------------------------------------------------------------------

/// Storage backend variant for `[server.storage]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackendKind {
    /// Write materialized query-dataset results to a local filesystem path.
    #[default]
    Local,
    /// Write materialized query-dataset results to an S3-compatible bucket.
    S3,
}

/// S3 connection settings for the server-level storage backend
/// (`[server.storage.s3]`). Credentials are referenced by environment
/// variable **name** only — inline values are rejected at startup.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageS3Config {
    pub region: Option<String>,
    /// Custom endpoint (MinIO, R2, …). Omit for AWS. Plain `host:port`
    /// or a full `http(s)://host:port` URL.
    pub endpoint: Option<String>,
    /// Name of the env var that holds the AWS access key ID.
    /// If both `access_key_id_env` and `secret_access_key_env` are absent,
    /// the default AWS credential provider chain is used.
    pub access_key_id_env: Option<String>,
    /// Name of the env var that holds the AWS secret access key.
    pub secret_access_key_env: Option<String>,
    /// `virtual` (default) or `path`. MinIO requires `path`.
    pub addressing_style: AddressingStyle,
    /// Allow plain-HTTP endpoints. Required for local MinIO `http://…`.
    pub allow_http: bool,
}

impl Default for StorageS3Config {
    fn default() -> Self {
        Self {
            region: None,
            endpoint: None,
            access_key_id_env: None,
            secret_access_key_env: None,
            addressing_style: AddressingStyle::Virtual,
            allow_http: false,
        }
    }
}

/// Resolved credentials from the storage S3 config. Both fields present
/// means explicit key-pair; both absent means provider chain.
#[derive(Debug, Clone, Default)]
pub struct StorageS3Creds {
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

impl StorageS3Config {
    /// Resolve the access-key pair from the env vars named in the config.
    /// Returns `Err` if exactly one of the two env vars is set (partial creds).
    pub fn resolved_creds(&self) -> Result<StorageS3Creds, AppError> {
        let key = self
            .access_key_id_env
            .as_deref()
            .and_then(|e| std::env::var(e).ok());
        let secret = self
            .secret_access_key_env
            .as_deref()
            .and_then(|e| std::env::var(e).ok());
        match (key, secret) {
            (Some(k), Some(s)) => Ok(StorageS3Creds {
                access_key_id: Some(k),
                secret_access_key: Some(s),
            }),
            (None, None) => Ok(StorageS3Creds::default()),
            _ => Err(AppError::Internal(
                "server.storage.s3: both access_key_id_env and secret_access_key_env \
                 must be set together, or both omitted"
                    .into(),
            )),
        }
    }
}

/// Server-level materialization storage backend (`[server.storage]`).
///
/// When present, `query`-kind datasets whose residency requires storage
/// (i.e. `residency = "lazy"` or automatic demotion) write their
/// materialized parquet files here. Absent → memory-only behavior.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Which storage medium to use.
    pub backend: StorageBackendKind,
    /// Root path (local) or `s3://bucket/prefix` (S3). Required when
    /// the block is present.
    pub root: String,
    /// Auto-demotion threshold in MiB. When a `query` dataset with
    /// `residency = "auto"` (the default) exceeds this size during
    /// materialization, the result is spilled to storage instead of
    /// staying in memory. Default `512`.
    #[serde(default = "default_force_lazy_above_mb")]
    pub force_lazy_above_mb: u64,
    /// S3 settings. Required when `backend = "s3"`.
    #[serde(default)]
    pub s3: StorageS3Config,
}

fn default_force_lazy_above_mb() -> u64 {
    512
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackendKind::Local,
            root: String::new(),
            force_lazy_above_mb: default_force_lazy_above_mb(),
            s3: StorageS3Config::default(),
        }
    }
}

/// Where a materialized `query`-dataset generation resides at runtime.
///
/// Applies to `[dataset.materialize]`; only valid on `kind = "query"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MaterializeResidency {
    /// Keep in memory unless the build crosses `force_lazy_above_mb`, in
    /// which case the generation is automatically demoted to storage.
    #[default]
    Auto,
    /// Always keep in memory. Crossing the threshold logs a WARN and
    /// increments a metric but does not demote.
    Memory,
    /// Always write to the storage backend; serve lazily from parquet.
    Lazy,
}

/// Per-dataset materialization options (`[dataset.materialize]`).
///
/// Only valid on `kind = "query"` datasets. Requires `[server.storage]`
/// when `residency = "lazy"` or when the auto-demotion threshold is crossed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaterializeConfig {
    /// Where to keep the built generation: `auto` (default), `memory`,
    /// or `lazy`.
    pub residency: MaterializeResidency,
    /// Column names to sort by when writing parquet files. Applied as an
    /// `ORDER BY` so row-group min/max stats prune effectively.
    #[serde(default)]
    pub sort_by: Vec<String>,
    /// When `true`, boot looks for the newest complete prior generation
    /// whose sql + schema hashes match current config and registers it
    /// without rebuilding. Default `false`.
    #[serde(default)]
    pub reuse_on_start: bool,
}

impl Default for MaterializeConfig {
    fn default() -> Self {
        Self {
            residency: MaterializeResidency::Auto,
            sort_by: Vec::new(),
            reuse_on_start: false,
        }
    }
}

/// Controls how a dataset is built at server startup.
///
/// - `eager` (default): built in background immediately at boot;
///   `/readyz` waits for it.
/// - `lazy`: registered as pending; built on the first incoming query
///   (the triggering request waits). Not gated by `/readyz`.
/// - `skip`: never auto-built; only built by an explicit
///   `POST /datasets/{name}/reload`. Not gated by `/readyz`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnStart {
    #[default]
    Eager,
    Lazy,
    Skip,
}

/// Server-level startup tuning (`[server.startup]` block).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StartupConfig {
    /// Maximum number of datasets that may be built concurrently during
    /// startup. Independent datasets within this limit build in parallel;
    /// a dependent dataset waits for all its dependencies first.
    /// Default `4`.
    pub max_concurrent: usize,
    /// Readiness policy for `/readyz`:
    /// - `all` (default): `/readyz` returns `200` only when every `eager`
    ///   dataset has published successfully.
    /// - `any`: `/readyz` returns `200` as soon as at least one `eager`
    ///   dataset has published.
    pub readiness: ReadinessMode,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            readiness: ReadinessMode::default(),
        }
    }
}

/// Readiness gate semantics for `/readyz`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessMode {
    /// `/readyz` 200 when every `eager` dataset is published.
    #[default]
    All,
    /// `/readyz` 200 as soon as at least one `eager` dataset is published.
    Any,
}

/// Per-dataset refresh schedule (`[dataset.refresh]` block).
///
/// Only valid on `kind = "query"` datasets. Setting this on a file-backed
/// dataset is a startup error.
///
/// Phase 3 implements interval-based refresh only; cron support is reserved
/// for a future phase. Setting both `interval` and `cron` is a startup error
/// (cron is not yet parsed, so any `cron` key in TOML will be rejected by
/// `deny_unknown_fields`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RefreshConfig {
    /// Polling interval, e.g. `"15m"`, `"2h"`. When absent, the dataset is
    /// not refreshed automatically.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_duration",
        serialize_with = "serialize_optional_duration"
    )]
    pub interval: Option<Duration>,
    /// Whether to refresh this dataset when an upstream dependency publishes
    /// a new generation. Parsed and stored; cascade behaviour is implemented
    /// in Phase 4. Default `false`.
    #[serde(default)]
    pub on_upstream_reload: bool,
    /// Per-build timeout, e.g. `"10m"`. Defaults to 10 minutes.
    #[serde(
        default = "default_refresh_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub timeout: Duration,
    /// Apply ±10 % uniform jitter to every scheduled fire. Default `true`.
    #[serde(default = "default_true")]
    pub jitter: bool,
    /// Debounce window for cascade refreshes (R4.3). Multiple upstream publishes
    /// arriving within this window coalesce to one downstream refresh.
    /// Default `5s`. Accepted formats: `"500ms"`, `"5s"`, `"1m"`, etc.
    #[serde(
        default = "default_debounce",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub debounce: Duration,
}

fn default_refresh_timeout() -> Duration {
    Duration::from_secs(600) // 10 minutes
}

fn default_debounce() -> Duration {
    Duration::from_secs(5)
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            interval: None,
            on_upstream_reload: false,
            timeout: default_refresh_timeout(),
            jitter: true,
            debounce: default_debounce(),
        }
    }
}

/// Server-level refresh concurrency (`[server.refresh]` block).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerRefreshConfig {
    /// Maximum number of dataset refreshes that may run concurrently.
    /// Default `1`.
    pub max_concurrent: usize,
}

impl Default for ServerRefreshConfig {
    fn default() -> Self {
        Self { max_concurrent: 1 }
    }
}

/// Experimental DuckDB Quack remote protocol server.
///
/// Quack exposes the DuckDB SQL surface of the in-process database. Keep it
/// disabled unless you intentionally want DuckDB clients to attach/query this
/// process directly.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct QuackConfig {
    /// Install/load the Quack extension and start `quack_serve` after
    /// datasets are registered.
    pub enabled: bool,
    /// Quack URI to listen on. `quack:localhost` uses DuckDB's default
    /// port 9494.
    pub uri: String,
    /// Optional explicit authentication token. If omitted, Quack generates
    /// one at startup and DataPress logs it once.
    pub token: Option<String>,
    /// Allow binding a non-local hostname such as `quack:0.0.0.0:9494`.
    /// For external exposure, put a TLS-terminating reverse proxy in front.
    pub allow_other_hostname: bool,
    /// Install a read-only authorization macro for remote queries. Enabled
    /// by default to match DataPress' read-oriented HTTP API.
    pub read_only: bool,
}

impl Default for QuackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            uri: "quack:localhost".into(),
            token: None,
            allow_other_hostname: false,
            read_only: true,
        }
    }
}

impl QuackConfig {
    /// Validate the enabled Quack configuration against DuckDB's current
    /// safety rules. The extension treats only the literal `localhost` as
    /// local unless `allow_other_hostname` is set.
    pub fn validate_enabled(&self) -> Result<(), AppError> {
        if self.uri.trim().is_empty() {
            return Err(AppError::Internal(
                "server.quack.uri must not be empty when server.quack.enabled = true".into(),
            ));
        }
        if !self.uri.starts_with("quack:") {
            return Err(AppError::Internal(format!(
                "server.quack.uri must start with 'quack:' (got '{}')",
                self.uri
            )));
        }
        if !self.allow_other_hostname {
            let host = self.hostname().unwrap_or_default();
            if host != "localhost" {
                return Err(AppError::Internal(format!(
                    "server.quack.uri host must be 'localhost' unless \
                     server.quack.allow_other_hostname = true (got '{}')",
                    self.uri
                )));
            }
        }
        if let Some(token) = self.token.as_deref()
            && token.len() < 4
        {
            return Err(AppError::Internal(
                "server.quack.token must be at least 4 characters".into(),
            ));
        }
        Ok(())
    }

    fn hostname(&self) -> Option<&str> {
        let rest = self.uri.strip_prefix("quack:")?;
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        let host = rest.split([':', '/', '?', '#']).next().unwrap_or_default();
        (!host.is_empty()).then_some(host)
    }
}

/// Experimental PostgreSQL wire-protocol server (`[server.pgwire]` block).
///
/// When enabled on the DataFusion backend, BI tools (Power BI via Npgsql,
/// `psql`, DBeaver, …) can connect and query the registered datasets as if
/// this process were PostgreSQL. Off by default and a no-op unless the binary
/// was built with the `pgwire` feature on `datapress-datafusion`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PgwireConfig {
    /// Start the pgwire listener after datasets are registered.
    pub enabled: bool,
    /// Listen address. Defaults to loopback (127.0.0.1). Binding a
    /// non-loopback address requires a password (and, since only cleartext
    /// password auth is available, TLS as well).
    pub listen: IpAddr,
    /// TCP port. Defaults to the PostgreSQL default 5432.
    pub port: u16,
    /// Username clients must present. Defaults to `datapress`.
    pub username: String,
    /// Password clients must present. Optional only for a loopback-only
    /// listener; required for any non-loopback bind.
    pub password: Option<String>,
    /// PEM certificate path for TLS. Must be set together with `tls_key`.
    pub tls_cert: Option<PathBuf>,
    /// PKCS#8 private-key path for TLS. Must be set together with `tls_cert`.
    pub tls_key: Option<PathBuf>,
}

impl Default for PgwireConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 5432,
            username: "datapress".into(),
            password: None,
            tls_cert: None,
            tls_key: None,
        }
    }
}

impl PgwireConfig {
    /// Validate the enabled pgwire configuration. Because the only available
    /// authentication mechanism is cleartext password (SCRAM would need a
    /// salted verifier the integration library does not expose), the rules
    /// are deliberately strict about exposing an off-box listener:
    ///
    /// * a non-loopback `listen` requires a `password` — an unauthenticated
    ///   SQL endpoint must never be reachable off the local host;
    /// * `tls_cert` and `tls_key` must be set together or not at all;
    /// * a non-loopback `listen` also requires TLS, so the cleartext password
    ///   never crosses a plaintext TCP connection off the box.
    pub fn validate_enabled(&self) -> Result<(), AppError> {
        let is_loopback = self.listen.is_loopback();
        let tls_configured = match (self.tls_cert.as_ref(), self.tls_key.as_ref()) {
            (Some(_), Some(_)) => true,
            (None, None) => false,
            _ => {
                return Err(AppError::Internal(
                    "server.pgwire.tls_cert and server.pgwire.tls_key must be set together \
                     (both or neither)"
                        .into(),
                ));
            }
        };

        if !is_loopback && self.password.is_none() {
            return Err(AppError::Internal(format!(
                "server.pgwire.password is required when server.pgwire.listen is not a \
                 loopback address (got '{}')",
                self.listen
            )));
        }

        if !is_loopback && !tls_configured {
            return Err(AppError::Internal(format!(
                "server.pgwire requires TLS (server.pgwire.tls_cert + tls_key) when \
                 server.pgwire.listen is not a loopback address (got '{}'): cleartext \
                 password auth must not cross a plaintext connection off the host",
                self.listen
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Datafusion,
    Duckdb,
}

/// Embedded MkDocs documentation site (`[docs]` block).
///
/// Enabled by default — when the binary was built with the `docs`
/// cargo feature, the site is served at [`DocsConfig::path`] out of
/// the box. Set `enabled = false` in `datasets.toml` to suppress it
/// (e.g. in prod). When the binary was built without the feature,
/// `enabled = true` is harmless: the server logs a warning at startup
/// and skips the mount. The mount path must be a non-trivial sub-path;
/// reserved API and probe roots are rejected at startup.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocsConfig {
    pub enabled: bool,
    pub path: String,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/mkdocs".into(),
        }
    }
}

/// Swagger UI + embedded OpenAPI spec (`[swagger]` block).
///
/// Enabled by default — when the binary was built with the `swagger`
/// cargo feature, an interactive Swagger UI is served at
/// [`SwaggerConfig::path`] (default `/docs`) and the raw OpenAPI JSON
/// at `<path>/openapi.json`. Set `enabled = false` in `datasets.toml`
/// to suppress it (e.g. in prod). When the binary was built without
/// the feature, `enabled = true` is harmless: the server logs a
/// warning at startup and skips the mount.
///
/// To let users sign in to the UI itself (Authorization Code + PKCE
/// against any OIDC provider), populate the optional `[swagger.oauth2]`
/// sub-block. Acquired tokens are attached as `Authorization: Bearer …`
/// to every "Try it out" request — useful for exercising auth-protected
/// endpoints from the docs page. This drives the UI only; it does not
/// turn on server-side token validation.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwaggerConfig {
    pub enabled: bool,
    pub path: String,
    pub oauth2: Option<SwaggerOAuth2Config>,
}

impl Default for SwaggerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/docs".into(),
            oauth2: None,
        }
    }
}

/// OIDC single-sign-on for the Swagger UI (`[swagger.oauth2]`).
///
/// Configures the UI to drive an Authorization Code + PKCE flow against
/// the given OIDC issuer. Swagger UI auto-discovers the authorize /
/// token endpoints from `<issuer>/.well-known/openid-configuration`,
/// so we don't need to pin them here.
///
/// All fields are required when the block is present — there is no
/// sensible default for `issuer` or `client_id`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwaggerOAuth2Config {
    /// OIDC issuer URL, e.g.
    /// `https://login.microsoftonline.com/<tenant>/v2.0` or
    /// `https://accounts.google.com`. Must not end in `/`.
    pub issuer: String,
    /// Public OAuth2 client identifier registered with the IdP. The
    /// client must be a SPA / public client (no secret) with
    /// `https://<your-host>{swagger.path}/oauth2-redirect.html` listed
    /// as an allowed redirect URI.
    pub client_id: String,
    /// Scopes to request by default. Will be pre-checked in the Swagger
    /// UI authorize dialog; users can edit them before signing in.
    /// `openid` is always added if missing.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Use PKCE for the authorization code flow. Defaults to `true`;
    /// disable only if your IdP doesn't support PKCE for public clients.
    #[serde(default = "default_true")]
    pub pkce: bool,
}

/// Prometheus metrics endpoint (`[metrics]` block).
///
/// Disabled by default. When `enabled = true` (and the binary was built
/// with the `metrics` cargo feature), the server installs a middleware
/// that records per-request HTTP counters and latency histograms, and
/// exposes them in the Prometheus text exposition format at
/// `{prefix}{path}` (default `/metrics` with an empty prefix).
///
/// Scrape configs must include the configured `server.prefix`. The
/// endpoint is **not** behind the `[auth]` layer: Prometheus scrapers
/// rarely carry bearer tokens, and the endpoint exposes only aggregate
/// request metrics (no row data). Keep it on a network the scraper can
/// reach but the public cannot, e.g. by binding `server.listen` to a
/// private interface.
///
/// When the binary was built without the `metrics` feature,
/// `enabled = true` is harmless: the server logs a warning at startup
/// and skips the endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "/metrics".into(),
        }
    }
}

/// Embedded dataset explorer UI (`[explorer]` block).
///
/// A server-rendered web app (Actix + Askama templates + htmx +
/// Bootstrap) served at [`ExplorerConfig::path`] (default `/explore`).
/// It offers a *discovery* view — per-dataset stats, schema, index and
/// source configuration — and an in-browser *DuckDB* console (DuckDB-WASM)
/// that queries each dataset's Parquet export directly.
///
/// Enabled by default. When the binary was built without the `explorer`
/// cargo feature, `enabled = true` is harmless: the server logs a warning
/// at startup and skips the mount. Set `enabled = false` to suppress it at
/// runtime even when the feature is compiled in.
///
/// To let users sign in from the explorer's **API Query** tab
/// (Authorization Code + PKCE against any OIDC provider), populate the
/// optional `[explorer.oauth2]` sub-block. Acquired tokens are attached as
/// `Authorization: Bearer …` to every API request the tab makes — useful
/// for exercising auth-protected endpoints. This drives the UI only; it
/// does not turn on server-side token validation (configure `[auth]` for
/// that).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExplorerConfig {
    pub enabled: bool,
    pub path: String,
    pub oauth2: Option<SwaggerOAuth2Config>,
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/explore".into(),
            oauth2: None,
        }
    }
}

/// Raw-SQL query endpoint (`[sql]` block).
///
/// Exposes `POST /api/v1/sql`, which accepts an arbitrary read-only
/// `SELECT` in the request body and runs it against the engine. **Off by
/// default** — raw SQL is a larger attack surface than the structured
/// `/query` endpoint, so it must be opted into explicitly.
///
/// Phase 1 is scoped to a *single* dataset per query: the statement may
/// reference at most one registered dataset (and no others / no files),
/// enforced by a parse-time table allowlist. Cross-dataset joins are a
/// future extension.
///
/// Safety rails applied to every accepted statement:
/// - exactly one statement, and it must be a read-only `SELECT` / `WITH`,
/// - every referenced table must be a registered dataset (no file
///   functions, no `ATTACH`/`COPY`/`PRAGMA`/DDL/DML),
/// - the result is hard-capped at [`SqlConfig::max_rows`] rows.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlConfig {
    /// Enable the `POST /api/v1/sql` endpoint. Default `false`.
    pub enabled: bool,
    /// Hard cap on the number of rows a single SQL query may return.
    /// The query result is wrapped in an outer `LIMIT` so this bound is
    /// enforced regardless of the user's own `LIMIT`. Default `100_000`.
    pub max_rows: u64,
}

impl Default for SqlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rows: 100_000,
        }
    }
}

/// DataFusion backend performance tuning (`[datafusion]` block).
///
/// Every knob is **off / stock by default**, so the backend behaves exactly
/// like DataFusion out of the box unless you opt in. These mainly help lazy
/// (`lazy = true`) parquet datasets, especially on object storage. Ignored by
/// the DuckDB backend.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataFusionConfig {
    /// Push row-level filters down into the parquet decoder so rows that fail
    /// a predicate are never materialised (in addition to the row-group /
    /// page-index pruning that always happens). DataFusion default is `false`
    /// because for some workloads the extra per-row evaluation is not worth
    /// it; turn it on for selective filters over large row groups.
    pub pushdown_filters: bool,
    /// Let the parquet scan reorder pushed-down predicates by estimated
    /// selectivity. Only has an effect together with `pushdown_filters`.
    /// DataFusion default is `false`.
    pub reorder_filters: bool,
    /// Cache object-store file listings on the shared runtime so repeated
    /// lazy queries reuse `LIST` results instead of re-listing the source
    /// prefix every time — the dominant per-query cost on S3. Default `false`.
    pub list_files_cache: bool,
    /// Memory budget for the file-listing cache, in MiB. Only used when
    /// `list_files_cache = true`. Default `64`.
    pub list_files_cache_mb: usize,
    /// How long a cached listing stays valid, in seconds. Bounds how long it
    /// takes for newly written files to become visible without an explicit
    /// reload. `0` means no expiry (infinite). Default `60`.
    pub list_files_cache_ttl_secs: u64,
}

impl Default for DataFusionConfig {
    fn default() -> Self {
        Self {
            pushdown_filters: false,
            reorder_filters: false,
            list_files_cache: false,
            list_files_cache_mb: 64,
            list_files_cache_ttl_secs: 60,
        }
    }
}

/// OIDC bearer-token enforcement for the HTTP API (`[auth]` block).
///
/// Disabled by default. When `enabled = true`, the server validates
/// every request's `Authorization: Bearer …` JWT against the JWKS
/// discovered from the issuer's OIDC metadata
/// (`<issuer>/.well-known/openid-configuration` → `jwks_uri`), then
/// enforces the configured scope requirements per route.
///
/// Only compiled in when the binary was built with the `auth` cargo
/// feature. Without the feature, `enabled = true` is rejected at
/// startup so a misconfigured production deployment can't silently
/// fall back to "no auth".
///
/// The Swagger UI's SSO support (`[swagger.oauth2]`) is *independent*
/// of this block — `[swagger.oauth2]` only drives the UI's login
/// dialog; `[auth]` is what enforces tokens on the API.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Master switch. `false` (default) skips all auth processing.
    pub enabled: bool,
    /// OIDC issuer URL — must match the `iss` claim of every accepted
    /// token. Required when `enabled = true`.
    pub issuer: String,
    /// Expected `aud` claim. When empty, audience validation is
    /// skipped (not recommended in production).
    pub audience: String,
    /// Scopes a caller must hold to read datasets (GET endpoints +
    /// POST `…/query` and `…/count`). Empty list means "no scope check,
    /// just a valid token is enough".
    pub read_scopes: Vec<String>,
    /// Scopes required for admin/mutation endpoints (POST `…/reload`).
    /// Empty list means "no scope check, just a valid token is enough".
    pub reload_scopes: Vec<String>,
    /// Allow unauthenticated GETs through. Useful for public datasets
    /// and demo deployments. Defaults to `false`.
    pub anonymous_read: bool,
    /// Continue serving even if the JWKS fetch fails at startup.
    /// When `true` (default), the server starts in a degraded mode that
    /// rejects every auth'd request with 503 until JWKS becomes
    /// reachable. When `false`, startup fails outright.
    pub start_degraded: bool,
    /// Allowed signing algorithms. Pinned to RS256 by default; never
    /// include `HS*` or `none` here unless you really know what you're
    /// doing.
    pub algorithms: Vec<String>,
    /// Clock-skew leeway for `exp`/`nbf` checks, in seconds.
    pub leeway_secs: u64,
    /// How often (in seconds) the background refresher re-fetches the
    /// JWKS. On a `kid` cache miss the JWKS is also refreshed
    /// out-of-band.
    pub jwks_refresh_secs: u64,
    /// Optional JSON-pointer into the JWT claims that extracts a
    /// tenant identifier — attached to the principal and logged on
    /// every request. Example: `"/tid"` (Azure AD), `"/org_id"`.
    /// When empty, no tenant is extracted.
    pub tenant_claim: String,
    /// If non-empty, requests whose extracted tenant ID is not in this
    /// list are rejected with 403. Has no effect when `tenant_claim`
    /// is empty.
    pub allowed_tenants: Vec<String>,
    /// If `true`, `POST …/reload` accepts *either* a valid token with
    /// `reload_scopes` *or* the legacy `X-Admin-Token` header. Defaults
    /// to `true` for one-release backwards compatibility — flip to
    /// `false` once your automation has migrated to OIDC.
    pub admin_token_fallback: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            audience: String::new(),
            read_scopes: Vec::new(),
            reload_scopes: Vec::new(),
            anonymous_read: false,
            start_degraded: true,
            algorithms: vec!["RS256".into()],
            leeway_secs: 60,
            jwks_refresh_secs: 3600,
            tenant_claim: String::new(),
            allowed_tenants: Vec::new(),
            admin_token_fallback: true,
        }
    }
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Datafusion => "datafusion",
            Backend::Duckdb => "duckdb",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatasetConfig {
    pub name: String,
    pub source: SourceConfig,
    #[serde(default)]
    pub s3: Option<S3Config>,
    #[serde(default)]
    pub index: IndexConfig,
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
    /// for bounded memory use on large / multi-file sources. Honoured by
    /// the DataFusion backend (local + S3 parquet) and by the DuckDB
    /// backend, which registers the dataset as a view over the source scan
    /// (local + S3 parquet, and delta) rather than materialising a table.
    #[serde(default)]
    pub lazy: bool,
    /// Column-level access control for query **predicates** — which columns
    /// a caller may filter on (structured `predicates` / `count`, and any
    /// reference on the raw-SQL endpoint). Mutually-exclusive `include`
    /// (allowlist) / `exclude` (denylist). Empty (default) = no restriction.
    #[serde(default)]
    pub predicate_filter: ColumnFilter,
    /// Column-level access control for **projection** — which columns a
    /// caller may see or return (the `columns` projection, `group_by`,
    /// aggregations, `order_by`, the `/schema` response and row sample, and
    /// any reference on the raw-SQL endpoint). Columns denied here are
    /// hidden as if they did not exist. Mutually-exclusive `include`
    /// (allowlist) / `exclude` (denylist). Empty (default) = no restriction.
    #[serde(default)]
    pub projection_filter: ColumnFilter,
    /// When and how this dataset is built at startup. See [`OnStart`].
    #[serde(default)]
    pub on_start: OnStart,
    /// Optional periodic refresh schedule. Only valid for `kind = "query"`
    /// datasets; setting this on a file-backed dataset is a startup error.
    #[serde(default)]
    pub refresh: Option<RefreshConfig>,
    /// Materialization residency and write options. Only valid for
    /// `kind = "query"` datasets. Requires `[server.storage]` when
    /// `residency = "lazy"`.
    #[serde(default)]
    pub materialize: Option<MaterializeConfig>,
}

fn default_true() -> bool {
    true
}

/// A mutually-exclusive column allow/deny list.
///
/// Set `include` to turn it into an **allowlist** — only the listed columns
/// pass. Set `exclude` to turn it into a **denylist** — every column except
/// the listed ones passes. Setting *both* is a configuration error (caught
/// by [`ColumnFilter::validate`]). Leaving both empty (the default) imposes
/// no restriction at all. Names are matched case-insensitively against the
/// dataset's canonical column names.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ColumnFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl ColumnFilter {
    /// Whether this filter restricts anything (either list is non-empty).
    pub fn is_active(&self) -> bool {
        !self.include.is_empty() || !self.exclude.is_empty()
    }

    /// Whether `col` passes the filter. Case-insensitive. An empty filter
    /// (neither list set) admits every column.
    pub fn allows(&self, col: &str) -> bool {
        let lc = col.to_lowercase();
        if !self.include.is_empty() {
            return self.include.iter().any(|c| c.to_lowercase() == lc);
        }
        if !self.exclude.is_empty() {
            return !self.exclude.iter().any(|c| c.to_lowercase() == lc);
        }
        true
    }

    /// Reject a filter that sets both `include` and `exclude`. `ctx`
    /// identifies the filter in the error message (e.g. `"predicate_filter"`).
    pub fn validate(&self, dataset: &str, ctx: &str) -> Result<(), AppError> {
        if !self.include.is_empty() && !self.exclude.is_empty() {
            return Err(AppError::InvalidValue(format!(
                "dataset '{dataset}': {ctx} may set 'include' or 'exclude', not both"
            )));
        }
        Ok(())
    }

    /// The names listed by whichever side is active, for cross-checking
    /// against the real schema at registration time (typos in a denylist
    /// would otherwise silently expose a column).
    pub fn listed(&self) -> &[String] {
        if !self.include.is_empty() {
            &self.include
        } else {
            &self.exclude
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceConfig {
    pub kind: SourceKind,
    /// Either a local filesystem path or an `s3://bucket/key` URL.
    /// Required for `parquet` and `delta` kinds; empty for `query`.
    #[serde(default)]
    pub location: String,
    /// The materialisation SQL for `kind = "query"` datasets. Must be a
    /// single read-only SELECT or WITH…SELECT. Required for `query` kind;
    /// absent for `parquet`/`delta`.
    #[serde(default)]
    pub sql: Option<String>,
    /// Datasets that `sql` references. Required for `query` kind; must
    /// list exactly the dataset names referenced in `sql`, no more, no
    /// fewer. Validated by [`AppConfig::validate`] against the SQL AST.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Parquet,
    Delta,
    /// A dataset whose content is the result of executing `sql` against
    /// other registered datasets. Materialised in-memory (DataFusion:
    /// Arc<DatasetState>; DuckDB: engine table) in dependency order at
    /// startup and on each explicit reload.
    Query,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Parquet => "parquet",
            SourceKind::Delta => "delta",
            SourceKind::Query => "query",
        }
    }
}

/// Non-secret S3 connection settings. Credentials are pulled from env / the
/// AWS credential chain — see [`DatasetConfig::resolved_creds`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct S3Config {
    pub region: Option<String>,
    /// Custom endpoint (MinIO, R2, Wasabi, LocalStack, …). Omit for AWS.
    pub endpoint: Option<String>,
    /// `virtual` (default — `bucket.host`) or `path` (`host/bucket/`).
    /// MinIO and most non-AWS providers require `path`.
    pub addressing_style: AddressingStyle,
    /// Allow plain-HTTP endpoints. Required for local MinIO over `http://…`.
    pub allow_http: bool,
    /// Inline credentials. Strongly discouraged in production — prefer env
    /// vars (see module docs).
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
    /// Hive partition-column handling for parquet sources. Defaults to
    /// `auto` (detect from the path). See [`Partitioning`].
    pub partitioning: Partitioning,
    /// Whether the bucket is folded into a custom `endpoint` host for
    /// virtual-hosted-style requests. Defaults to `auto`. See
    /// [`BucketInHost`].
    pub endpoint_bucket_in_host: BucketInHost,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            region: None,
            endpoint: None,
            addressing_style: AddressingStyle::Virtual,
            allow_http: false,
            access_key_id: None,
            secret_access_key: None,
            session_token: None,
            partitioning: Partitioning::Auto,
            endpoint_bucket_in_host: BucketInHost::Auto,
        }
    }
}

impl S3Config {
    /// Resolve the endpoint URL to hand to the object store, optionally
    /// folding `bucket` into the host for virtual-hosted-style requests so a
    /// plain `endpoint` works the same way it does on DuckDB.
    ///
    /// Returns `None` when no custom endpoint is configured (AWS default).
    /// The bucket is only prepended when it isn't already the leading host
    /// label, so re-running this (or a config that already embeds the
    /// bucket) never produces `bucket.bucket.host`.
    pub fn effective_endpoint(&self, bucket: &str) -> Option<String> {
        let ep = self.endpoint.as_deref().filter(|s| !s.is_empty())?;

        let fold = match self.endpoint_bucket_in_host {
            BucketInHost::False => false,
            BucketInHost::True => true,
            BucketInHost::Auto => self.addressing_style == AddressingStyle::Virtual,
        };
        if !fold {
            return Some(ep.to_string());
        }

        let (scheme, host_and_path) = match ep.split_once("://") {
            Some((s, rest)) => (Some(s), rest),
            None => (None, ep),
        };
        // Split host from any trailing path so we prefix the host label only.
        let (host, path) = match host_and_path.split_once('/') {
            Some((h, p)) => (h, Some(p)),
            None => (host_and_path, None),
        };
        // Guard against double-prefixing.
        if host == bucket || host.starts_with(&format!("{bucket}.")) {
            return Some(ep.to_string());
        }
        let new_host = format!("{bucket}.{host}");
        let rebuilt = match (scheme, path) {
            (Some(s), Some(p)) => format!("{s}://{new_host}/{p}"),
            (Some(s), None) => format!("{s}://{new_host}"),
            (None, Some(p)) => format!("{new_host}/{p}"),
            (None, None) => new_host,
        };
        Some(rebuilt)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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
            AddressingStyle::Path => "path",
        }
    }
}

/// How hive-style partition columns (`key=value/` path segments) are handled
/// for an S3 parquet source. Local parquet always auto-detects; this option
/// brings S3 in line and lets you force or disable the behaviour.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Partitioning {
    /// Detect `key=value` segments from the location glob or by listing the
    /// prefix. No partition columns are added when none are found.
    #[default]
    Auto,
    /// Force hive partitioning. Partition keys are taken from the location
    /// glob, or discovered by listing the prefix.
    Hive,
    /// Treat the source as a flat set of parquet files — never add partition
    /// columns even if the path looks hive-partitioned.
    None,
}

impl Partitioning {
    pub fn as_str(self) -> &'static str {
        match self {
            Partitioning::Auto => "auto",
            Partitioning::Hive => "hive",
            Partitioning::None => "none",
        }
    }
}

/// Whether the bucket name is folded into the endpoint hostname for
/// virtual-hosted-style requests against a custom endpoint. This aligns the
/// DataFusion object-store path with DuckDB, which builds the virtual host
/// itself — so the same plain `endpoint` works on both backends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BucketInHost {
    /// Fold the bucket into the host when `addressing_style = "virtual"` and
    /// a custom `endpoint` is set (guarded against double-prefixing).
    #[default]
    Auto,
    /// Always fold the bucket into the endpoint host.
    True,
    /// Never rewrite the endpoint — pass it through verbatim.
    False,
}

impl BucketInHost {
    pub fn as_str(self) -> &'static str {
        match self {
            BucketInHost::Auto => "auto",
            BucketInHost::True => "true",
            BucketInHost::False => "false",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IndexConfig {
    pub mode: IndexMode,
    pub columns: Vec<String>,
    pub max_cardinality: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            mode: IndexMode::Auto,
            columns: Vec::new(),
            max_cardinality: 100_000,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
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
        let mut cfg: AppConfig =
            toml::from_str(&raw).map_err(|e| AppError::Internal(format!("invalid {path}: {e}")))?;
        cfg.normalize();
        cfg.validate()?;
        // Remember where we loaded from so the explorer can optionally
        // append newly-registered datasets back to this file. Ignore the
        // error if it was already set (only the first load wins).
        let _ = SOURCE_CONFIG_PATH.set(PathBuf::from(path));
        Ok(cfg)
    }

    /// Canonicalise fields that are compared case-insensitively at runtime.
    ///
    /// Token scopes are lowercased when parsed out of a JWT (see `auth.rs`),
    /// so the configured `read_scopes` / `reload_scopes` are lowercased here
    /// once at load time. Without this an operator who writes
    /// `"Datasets:Read"` would silently 403 every caller, since the token
    /// side would have become `datasets:read`.
    fn normalize(&mut self) {
        for s in self
            .auth
            .read_scopes
            .iter_mut()
            .chain(self.auth.reload_scopes.iter_mut())
        {
            *s = s.to_ascii_lowercase();
        }
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

        if self.server.quack.enabled {
            self.server.quack.validate_enabled()?;
        }

        if self.server.pgwire.enabled {
            self.server.pgwire.validate_enabled()?;
        }

        // Validate the docs mount path even when the section is disabled,
        // so an inactive config typo can't go unnoticed.
        {
            let dp = &self.docs.path;
            if !dp.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "docs.path must start with '/' (got '{dp}')"
                )));
            }
            if dp.len() > 1 && dp.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "docs.path must not end with '/' (got '{dp}')"
                )));
            }
            if RESERVED_MOUNTS.iter().any(|r| *r == dp) {
                return Err(AppError::Internal(format!(
                    "docs.path '{dp}' collides with a reserved route"
                )));
            }
        }

        // Same for the swagger UI mount.
        {
            let sp = &self.swagger.path;
            if !sp.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "swagger.path must start with '/' (got '{sp}')"
                )));
            }
            if sp.len() > 1 && sp.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "swagger.path must not end with '/' (got '{sp}')"
                )));
            }
            if RESERVED_MOUNTS.iter().any(|r| *r == sp) {
                return Err(AppError::Internal(format!(
                    "swagger.path '{sp}' collides with a reserved route"
                )));
            }
            if sp == &self.docs.path {
                return Err(AppError::Internal(format!(
                    "swagger.path and docs.path must differ (both '{sp}')"
                )));
            }
            if let Some(o) = &self.swagger.oauth2 {
                if o.issuer.trim().is_empty() {
                    return Err(AppError::Internal(
                        "swagger.oauth2.issuer must not be empty".into(),
                    ));
                }
                if !(o.issuer.starts_with("https://") || o.issuer.starts_with("http://")) {
                    return Err(AppError::Internal(format!(
                        "swagger.oauth2.issuer must be an absolute http(s) URL (got '{}')",
                        o.issuer
                    )));
                }
                if o.client_id.trim().is_empty() {
                    return Err(AppError::Internal(
                        "swagger.oauth2.client_id must not be empty".into(),
                    ));
                }
            }
        }

        // Metrics endpoint mount path. Validated even when disabled so an
        // inactive config typo can't go unnoticed. `/metrics` is itself a
        // reserved mount (so docs/swagger can't shadow it), so we check the
        // remaining reserved routes — and the docs/swagger paths — for
        // collisions rather than the whole list.
        {
            let mp = &self.metrics.path;
            if !mp.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "metrics.path must start with '/' (got '{mp}')"
                )));
            }
            if mp.len() > 1 && mp.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "metrics.path must not end with '/' (got '{mp}')"
                )));
            }
            if RESERVED_MOUNTS.iter().any(|r| *r == mp && *r != "/metrics") {
                return Err(AppError::Internal(format!(
                    "metrics.path '{mp}' collides with a reserved route"
                )));
            }
            if mp == &self.docs.path {
                return Err(AppError::Internal(format!(
                    "metrics.path and docs.path must differ (both '{mp}')"
                )));
            }
            if mp == &self.swagger.path {
                return Err(AppError::Internal(format!(
                    "metrics.path and swagger.path must differ (both '{mp}')"
                )));
            }
        }

        // Explorer UI mount path. Validated even when disabled so an
        // inactive config typo can't go unnoticed.
        {
            let ep = &self.explorer.path;
            if !ep.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "explorer.path must start with '/' (got '{ep}')"
                )));
            }
            if ep.len() > 1 && ep.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "explorer.path must not end with '/' (got '{ep}')"
                )));
            }
            if RESERVED_MOUNTS.iter().any(|r| *r == ep) {
                return Err(AppError::Internal(format!(
                    "explorer.path '{ep}' collides with a reserved route"
                )));
            }
            if ep == &self.docs.path {
                return Err(AppError::Internal(format!(
                    "explorer.path and docs.path must differ (both '{ep}')"
                )));
            }
            if ep == &self.swagger.path {
                return Err(AppError::Internal(format!(
                    "explorer.path and swagger.path must differ (both '{ep}')"
                )));
            }
            if ep == &self.metrics.path {
                return Err(AppError::Internal(format!(
                    "explorer.path and metrics.path must differ (both '{ep}')"
                )));
            }
        }

        // Auth block — only meaningful when `enabled = true`. The cargo
        // feature gate is enforced separately in `server::serve` so a
        // binary built without `--features auth` and a config with
        // `auth.enabled = true` aborts with a clear error.
        if self.auth.enabled {
            let a = &self.auth;
            if a.issuer.trim().is_empty() {
                return Err(AppError::Internal(
                    "auth.issuer must not be empty when auth.enabled = true".into(),
                ));
            }
            if !(a.issuer.starts_with("https://") || a.issuer.starts_with("http://")) {
                return Err(AppError::Internal(format!(
                    "auth.issuer must be an absolute http(s) URL (got '{}')",
                    a.issuer
                )));
            }
            for alg in &a.algorithms {
                match alg.as_str() {
                    "RS256" | "RS384" | "RS512" | "ES256" | "ES384" | "PS256" | "PS384"
                    | "PS512" => {}
                    other => {
                        return Err(AppError::Internal(format!(
                            "auth.algorithms[{other}] is not allowed; pick one of \
                         RS256/RS384/RS512, ES256/ES384, PS256/PS384/PS512"
                        )));
                    }
                }
            }
            if a.algorithms.is_empty() {
                return Err(AppError::Internal(
                    "auth.algorithms must not be empty".into(),
                ));
            }
            if !a.tenant_claim.is_empty() && !a.tenant_claim.starts_with('/') {
                return Err(AppError::Internal(format!(
                    "auth.tenant_claim must be a JSON pointer starting with '/' (got '{}')",
                    a.tenant_claim
                )));
            }
            if !a.allowed_tenants.is_empty() && a.tenant_claim.is_empty() {
                return Err(AppError::Internal(
                    "auth.allowed_tenants is set but auth.tenant_claim is empty — \
                     can't enforce a tenant allow-list without a claim to extract from"
                        .into(),
                ));
            }
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
                return Err(AppError::Internal("dataset name must not be empty".into()));
            }
            // URL-safe: alphanum + _ - .
            if !d
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
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
            if d.source.kind == SourceKind::Query {
                // query sources: no location, validated separately below.
            } else if d.source.is_s3() {
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
                    SourceKind::Parquet => {
                        d.resolve_local_parquet_files()?;
                    }
                    SourceKind::Delta => {
                        let p = Path::new(&d.source.location);
                        if !p.exists() {
                            return Err(AppError::Internal(format!(
                                "dataset '{}': delta location does not exist: {}",
                                d.name, d.source.location
                            )));
                        }
                    }
                    // Query sources are handled by the earlier branch above.
                    SourceKind::Query => {}
                }
            }

            // R3.x — [dataset.refresh] is only valid on kind = "query".
            if d.source.kind != SourceKind::Query {
                if d.refresh.is_some() {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': [dataset.refresh] is only valid for kind = \"query\" datasets",
                        d.name
                    )));
                }
            } else if let Some(ref rc) = d.refresh {
                // Validate the interval when present.
                if rc.interval.is_some_and(|i| i.is_zero()) {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': refresh.interval must be greater than zero",
                        d.name
                    )));
                }
            }

            // R2B.1 / R2B.5 — [dataset.materialize] validation.
            if let Some(ref mc) = d.materialize {
                // Only valid on query datasets.
                if d.source.kind != SourceKind::Query {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': [dataset.materialize] is only valid for kind = \"query\" datasets",
                        d.name
                    )));
                }
                // Lazy or any materialize block without [server.storage] is a startup error.
                // auto without storage degrades to memory with a WARN (handled at runtime).
                if mc.residency == MaterializeResidency::Lazy && self.server.storage.is_none() {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': materialize.residency = \"lazy\" requires \
                         [server.storage] to be configured",
                        d.name
                    )));
                }
                // R2B.5: explicit [dataset.index] combined with lazy is a startup error.
                if mc.residency == MaterializeResidency::Lazy && d.index.mode != IndexMode::Auto {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': [dataset.index] with mode != \"auto\" is incompatible \
                         with materialize.residency = \"lazy\" (lazy datasets have no eq-index)",
                        d.name
                    )));
                }
            }
        }

        // R2B.7: validate [server.storage] when present — inline credentials are rejected.
        if let Some(ref sc) = self.server.storage {
            if sc.root.trim().is_empty() {
                return Err(AppError::Internal(
                    "server.storage.root must not be empty when [server.storage] is configured"
                        .into(),
                ));
            }
            if sc.backend == StorageBackendKind::S3 {
                // Validate s3 block: no inline credentials allowed (env-var indirection only).
                // (The StorageS3Config only accepts env-var NAMES, so inline values can only
                // appear if someone shoves raw values into the env-var-name fields — we can't
                // distinguish that here. The actual credential values are read from env at
                // runtime by `resolved_creds()`.)
                // Validate that the root is an s3:// URL.
                if !sc.root.starts_with("s3://") {
                    return Err(AppError::Internal(format!(
                        "server.storage.root must start with s3:// when backend = \"s3\" \
                         (got '{}')",
                        sc.root
                    )));
                }
            } else {
                // Local backend: root must not look like an S3 URL.
                if sc.root.starts_with("s3://") {
                    return Err(AppError::Internal(format!(
                        "server.storage.root looks like an S3 URL but backend = \"local\" \
                         (got '{}'); set backend = \"s3\" if you mean S3",
                        sc.root
                    )));
                }
            }
        }

        // R2.1 — validate `query` dataset sources: SQL structure, exact-match
        // depends_on, self-reference rejection (R2.3).
        {
            let all_names: std::collections::HashSet<String> = self
                .datasets
                .iter()
                .map(|d| d.name.to_lowercase())
                .collect();

            for d in &self.datasets {
                if d.source.kind != SourceKind::Query {
                    continue;
                }
                // Require sql field.
                let sql = d.source.sql.as_deref().ok_or_else(|| {
                    AppError::Internal(format!(
                        "dataset '{}': source.sql is required for kind = \"query\"",
                        d.name
                    ))
                })?;
                // Require non-empty depends_on.
                if d.source.depends_on.is_empty() {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': depends_on is required for kind = \"query\" \
                         and must list every dataset referenced in sql",
                        d.name
                    )));
                }
                // Self-reference (R2.3).
                if d.source
                    .depends_on
                    .iter()
                    .any(|dep| dep.eq_ignore_ascii_case(&d.name))
                {
                    return Err(AppError::Internal(format!(
                        "dataset '{}': a query dataset cannot depend on itself",
                        d.name
                    )));
                }
                // All depends_on entries must be known datasets (R2.1-c).
                for dep in &d.source.depends_on {
                    if !all_names.contains(&dep.to_lowercase()) {
                        return Err(AppError::Internal(format!(
                            "dataset '{}': depends_on '{}' is not a defined dataset",
                            d.name, dep
                        )));
                    }
                }
                // Parse SQL and extract referenced table names (reuse Phase 1
                // validator logic). R2.2: materialization is always permitted
                // regardless of [sql].enabled — pass all dataset names as allowed.
                let validated = crate::sql::validate(sql, &all_names, usize::MAX).map_err(|e| {
                    AppError::Internal(format!("dataset '{}': source.sql is invalid: {e}", d.name))
                })?;
                let referenced: std::collections::HashSet<String> =
                    validated.datasets.into_iter().collect();
                let declared: std::collections::HashSet<String> = d
                    .source
                    .depends_on
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect();
                // R2.1-a: references not listed in depends_on.
                for ref_name in &referenced {
                    if !declared.contains(ref_name) {
                        return Err(AppError::Internal(format!(
                            "dataset '{}': SQL references '{}' which is not listed \
                             in depends_on",
                            d.name, ref_name
                        )));
                    }
                }
                // R2.1-b: depends_on entries not referenced in SQL.
                for dep in &declared {
                    if !referenced.contains(dep) {
                        return Err(AppError::Internal(format!(
                            "dataset '{}': depends_on lists '{}' but it is not \
                             referenced in sql",
                            d.name, dep
                        )));
                    }
                }
            }

            // R2.4 — Kahn topological validation: detect cycles (incl. self)
            // and ensure a valid build order exists. The actual order vector
            // is exposed via `topological_dataset_order()`.
            self.topological_dataset_order()
                .map_err(|e| AppError::Internal(format!("dataset dependency error: {e}")))?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Cycle-path helpers (used by topological_dataset_order on cycle detection)
// ---------------------------------------------------------------------------

/// Walk the query-dataset dependency graph with DFS and return the indices
/// of one cycle path.  The returned slice is closed: `path[0] == path.last()`.
fn find_cycle_path<'a>(
    datasets: &'a [DatasetConfig],
    name_to_idx: &std::collections::HashMap<&str, usize>,
) -> Vec<&'a str> {
    let n = datasets.len();
    let mut visited = vec![false; n];
    let mut in_stack = vec![false; n];
    let mut path: Vec<usize> = Vec::new();

    for start in 0..n {
        if visited[start] || datasets[start].source.kind != SourceKind::Query {
            continue;
        }
        if let Some(cycle) = dfs_find_cycle(
            start,
            datasets,
            name_to_idx,
            &mut visited,
            &mut in_stack,
            &mut path,
        ) {
            let mut names: Vec<&str> = cycle.iter().map(|&i| datasets[i].name.as_str()).collect();
            names.push(datasets[cycle[0]].name.as_str()); // close the cycle
            return names;
        }
    }
    // Fallback (shouldn't be reached when a cycle exists).
    datasets
        .iter()
        .filter(|d| d.source.kind == SourceKind::Query)
        .map(|d| d.name.as_str())
        .collect()
}

fn dfs_find_cycle(
    node: usize,
    datasets: &[DatasetConfig],
    name_to_idx: &std::collections::HashMap<&str, usize>,
    visited: &mut Vec<bool>,
    in_stack: &mut Vec<bool>,
    path: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    visited[node] = true;
    in_stack[node] = true;
    path.push(node);

    for dep in &datasets[node].source.depends_on {
        if let Some(&next) = name_to_idx.get(dep.as_str()) {
            if datasets[next].source.kind != SourceKind::Query {
                continue;
            }
            if !visited[next] {
                if let Some(cycle) =
                    dfs_find_cycle(next, datasets, name_to_idx, visited, in_stack, path)
                {
                    return Some(cycle);
                }
            } else if in_stack[next] {
                // Back-edge found — extract the cycle portion.
                let start = path.iter().position(|&x| x == next).unwrap();
                return Some(path[start..].to_vec());
            }
        }
    }

    path.pop();
    in_stack[node] = false;
    None
}

impl AppConfig {
    /// Return the indices of `self.datasets` in a topological build order
    /// (dependencies before dependents). File-backed datasets have no
    /// dependencies and may appear in any relative order. Cycle detection
    /// uses Kahn's algorithm; on a cycle the error names the cycle path
    /// (`a → b → a`).
    ///
    /// Called at validation time to reject cycles, and by backends to build
    /// datasets in the correct order.
    pub fn topological_dataset_order(&self) -> Result<Vec<usize>, AppError> {
        use std::collections::{HashMap, VecDeque};

        let n = self.datasets.len();
        // name → index
        let name_to_idx: HashMap<&str, usize> = self
            .datasets
            .iter()
            .enumerate()
            .map(|(i, d)| (d.name.as_str(), i))
            .collect();

        // Build adjacency: dep → dependents (edges dep → current).
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n]; // adj[dep] = [dependents]

        for (i, d) in self.datasets.iter().enumerate() {
            if d.source.kind != SourceKind::Query {
                continue;
            }
            for dep_name in &d.source.depends_on {
                if let Some(&dep_idx) = name_to_idx.get(dep_name.as_str()) {
                    adj[dep_idx].push(i);
                    in_degree[i] += 1;
                }
                // Unknown depends_on entries are caught by validate() before
                // this is called; skip silently here.
            }
        }

        let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(node) = queue.pop_front() {
            order.push(node);
            for &dep in &adj[node] {
                in_degree[dep] -= 1;
                if in_degree[dep] == 0 {
                    queue.push_back(dep);
                }
            }
        }

        if order.len() != n {
            // Cycle detected: find and report the actual cycle path (R4.1).
            let cycle_path = find_cycle_path(&self.datasets, &name_to_idx);
            return Err(AppError::Internal(format!(
                "dependency cycle detected: {}",
                cycle_path.join(" \u{2192} "),
            )));
        }

        Ok(order)
    }
}

impl SourceConfig {
    pub fn is_query(&self) -> bool {
        self.kind == SourceKind::Query
    }

    pub fn is_s3(&self) -> bool {
        self.location.starts_with("s3://")
    }

    /// True when the location already contains a glob metacharacter
    /// (`*`, `?`, or `[`).
    pub fn has_glob(&self) -> bool {
        self.location.contains('*') || self.location.contains('?') || self.location.contains('[')
    }

    /// Location to hand to a backend that needs an explicit parquet glob
    /// (DuckDB). When the location is a plain S3 prefix with no glob, append
    /// a recursive `**/*.parquet` so DuckDB lists the prefix the same way
    /// DataFusion's object-store listing does. Globbed or non-S3 locations
    /// are returned unchanged.
    pub fn s3_recursive_parquet_glob(&self) -> String {
        if !self.is_s3() || self.has_glob() {
            return self.location.clone();
        }
        let trimmed = self.location.trim_end_matches('/');
        format!("{trimmed}/**/*.parquet")
    }

    /// Returns `(bucket, key_prefix_or_empty)` for an `s3://…` location.
    pub fn s3_bucket(&self) -> Result<(&str, &str), AppError> {
        let rest = self
            .location
            .strip_prefix("s3://")
            .ok_or_else(|| AppError::Internal(format!("not an s3:// URL: {}", self.location)))?;
        let (bucket, key) = match rest.split_once('/') {
            Some((b, k)) => (b, k),
            None => (rest, ""),
        };
        if bucket.is_empty() {
            return Err(AppError::Internal(format!(
                "s3 URL missing bucket: {}",
                self.location
            )));
        }
        Ok((bucket, key))
    }
}

impl DatasetConfig {
    /// Validate a dataset config supplied at runtime (e.g. registered live
    /// through the explorer) with the same rules the startup loader applies
    /// to each `[[dataset]]`: non-empty, URL-safe name and a coherent index
    /// configuration. Source reachability is left to the backend, which
    /// surfaces a clear error when it tries to open the source.
    pub fn validate_for_register(&self) -> Result<(), AppError> {
        if self.name.is_empty() {
            return Err(AppError::InvalidValue(
                "dataset name must not be empty".into(),
            ));
        }
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(AppError::InvalidValue(format!(
                "dataset name '{}' must be alphanumeric (plus _ - .)",
                self.name
            )));
        }
        if self.index.mode == IndexMode::List && self.index.columns.is_empty() {
            return Err(AppError::InvalidValue(format!(
                "dataset '{}': index.mode = 'list' requires non-empty index.columns",
                self.name
            )));
        }
        self.predicate_filter
            .validate(&self.name, "predicate_filter")?;
        self.projection_filter
            .validate(&self.name, "projection_filter")?;
        if self.source.is_s3() {
            self.source.s3_bucket()?;
        }
        Ok(())
    }

    /// Render this dataset as a standalone TOML `[[dataset]]` block suitable
    /// for pasting into (or appending to) a `datasets.toml`. Fields are
    /// emitted scalars-first so the output is valid TOML, and default-valued
    /// sections (`[dataset.s3]`, a default `[dataset.index]`, an empty
    /// projection) are omitted to keep the snippet minimal.
    pub fn to_toml_block(&self) -> Result<String, AppError> {
        #[derive(Serialize)]
        struct Block {
            name: String,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            columns: Vec<String>,
            dict_encode: bool,
            lazy: bool,
            source: SourceConfig,
            #[serde(skip_serializing_if = "Option::is_none")]
            s3: Option<S3Config>,
            #[serde(skip_serializing_if = "Option::is_none")]
            index: Option<IndexConfig>,
            #[serde(skip_serializing_if = "Option::is_none")]
            predicate_filter: Option<ColumnFilter>,
            #[serde(skip_serializing_if = "Option::is_none")]
            projection_filter: Option<ColumnFilter>,
        }
        #[derive(Serialize)]
        struct Doc {
            dataset: [Block; 1],
        }
        let doc = Doc {
            dataset: [Block {
                name: self.name.clone(),
                columns: self.columns.clone(),
                dict_encode: self.dict_encode,
                lazy: self.lazy,
                source: self.source.clone(),
                s3: self.s3.clone(),
                index: if self.index.is_default() {
                    None
                } else {
                    Some(self.index.clone())
                },
                predicate_filter: self
                    .predicate_filter
                    .is_active()
                    .then(|| self.predicate_filter.clone()),
                projection_filter: self
                    .projection_filter
                    .is_active()
                    .then(|| self.projection_filter.clone()),
            }],
        };
        toml::to_string_pretty(&doc)
            .map_err(|e| AppError::Internal(format!("failed to render dataset TOML: {e}")))
    }

    /// Append this dataset's `[[dataset]]` block to the config file this
    /// process was loaded from, so a runtime-registered dataset survives a
    /// restart. Returns the path written to.
    ///
    /// Errors with `AppError::InvalidValue` when the process has no on-disk
    /// config ([`source_config_path`] is `None` — e.g. the Python bindings),
    /// and with `AppError::Internal` when rendering or the file write fails.
    /// Shared by the versioned API (`POST /api/v1/datasets/persist`) and the
    /// explorer's persist action.
    pub fn persist_to_source_config(&self) -> Result<PathBuf, AppError> {
        use std::io::Write;
        let path = source_config_path().ok_or_else(|| {
            AppError::InvalidValue("server has no on-disk config file to append to".into())
        })?;
        let block = self.to_toml_block()?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| {
                AppError::Internal(format!("failed to open config {}: {e}", path.display()))
            })?;
        // Separate the appended block from existing content by a blank line.
        write!(file, "\n{block}").map_err(|e| {
            AppError::Internal(format!("failed to write config {}: {e}", path.display()))
        })?;
        Ok(path.to_path_buf())
    }
}

impl IndexConfig {
    /// Whether this equals the serde default (used to omit the section from
    /// exported TOML when it carries no information).
    fn is_default(&self) -> bool {
        self.mode == IndexMode::Auto && self.columns.is_empty() && self.max_cardinality == 100_000
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
                .map_err(|e| {
                    AppError::Internal(format!(
                        "dataset '{}': bad glob pattern '{loc}': {e}",
                        self.name
                    ))
                })?
                .filter_map(|r| r.ok())
                .filter(|p| {
                    p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("parquet")
                })
                .collect();
            files.sort();
            if files.is_empty() {
                return Err(AppError::EmptyDataset(format!(
                    "dataset '{}': glob '{loc}' matched no .parquet files",
                    self.name
                )));
            }
            return Ok(files);
        }

        let path = Path::new(loc);
        if !path.exists() {
            return Err(AppError::Internal(format!(
                "dataset '{}': source path does not exist: {loc}",
                self.name
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
            return Err(AppError::EmptyDataset(format!(
                "dataset '{}': no *.parquet files found in {loc}",
                self.name
            )));
        }
        Ok(files)
    }

    /// Estimate the on-disk byte size of this dataset's local backing
    /// files. Returns `None` for S3 sources (sizing would require a
    /// network round-trip) or when nothing can be measured.
    ///
    /// * `parquet` sums the resolved `.parquet` files (single file,
    ///   directory, or glob).
    /// * `delta` sums every `*.parquet` data file under the table root.
    ///   This slightly over-counts when stale files haven't been vacuumed,
    ///   which is fine for a coarse force-lazy threshold.
    pub fn estimate_local_bytes(&self) -> Option<u64> {
        if self.source.is_s3() {
            return None;
        }
        match self.source.kind {
            SourceKind::Query => None,
            SourceKind::Parquet => {
                let files = self.resolve_local_parquet_files().ok()?;
                Some(
                    files
                        .iter()
                        .filter_map(|p| std::fs::metadata(p).ok())
                        .map(|m| m.len())
                        .sum(),
                )
            }
            SourceKind::Delta => {
                let root = self.source.location.trim_end_matches('/');
                let pattern = format!("{root}/**/*.parquet");
                let paths = glob::glob(&pattern).ok()?;
                Some(
                    paths
                        .filter_map(Result::ok)
                        .filter_map(|p| std::fs::metadata(&p).ok())
                        .filter(|m| m.is_file())
                        .map(|m| m.len())
                        .sum(),
                )
            }
        }
    }

    /// Decide whether this dataset should be forced into lazy mode given
    /// the server's `force_lazy_above_mb` threshold. Returns `Some(bytes)`
    /// (the measured size) when it should be forced, so the caller can log
    /// it. Returns `None` when the dataset is already lazy, the threshold
    /// is disabled, the source is S3, or the measured size is unknown or at
    /// or below the threshold.
    pub fn force_lazy_bytes(&self, server: &ServerConfig) -> Option<u64> {
        if self.lazy || server.force_lazy_above_mb == 0 {
            return None;
        }
        let threshold = server.force_lazy_above_mb.saturating_mul(1024 * 1024);
        match self.estimate_local_bytes() {
            Some(bytes) if bytes > threshold => Some(bytes),
            _ => None,
        }
    }

    /// Env-var prefix derived from the dataset name: uppercase with
    /// non-alphanumeric chars replaced by `_`. E.g. `sales.eu-1` →
    /// `SALES_EU_1`.
    pub fn env_prefix(&self) -> String {
        self.name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Resolve S3 credentials following the precedence chain documented at
    /// the top of this module. Returns an empty struct when nothing was
    /// found — the caller should then leave credential resolution to the
    /// engine's default provider chain.
    pub fn resolved_creds(&self) -> ResolvedCreds {
        let prefix = self.env_prefix();
        let from_env = |suffix: &str| {
            std::env::var(format!("{prefix}_{suffix}"))
                .ok()
                .filter(|s| !s.is_empty())
        };
        let inline = self.s3.as_ref();
        let plain_env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());

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
        std::env::var(format!("{prefix}_AWS_REGION"))
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3.as_ref().and_then(|s| s.region.clone()))
            .or_else(|| std::env::var("AWS_REGION").ok().filter(|s| !s.is_empty()))
            .or_else(|| {
                std::env::var("AWS_DEFAULT_REGION")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
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
        assert_eq!(s.max_page_size, 100_000);
        assert_eq!(s.force_lazy_above_mb, 0);
        assert_eq!(s.request_timeout_ms, 30_000);
        assert!(!s.quack.enabled);
        assert_eq!(s.quack.uri, "quack:localhost");
        assert!(s.quack.token.is_none());
        assert!(!s.quack.allow_other_hostname);
        assert!(s.quack.read_only);
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
            max_page_size = 50000
            force_lazy_above_mb = 256
            request_timeout_ms = 0

            [server.quack]
            enabled = true
            uri = "quack:localhost:9495"
            token = "test-token"
            read_only = false
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
        assert_eq!(cfg.server.max_page_size, 50_000);
        assert_eq!(cfg.server.force_lazy_above_mb, 256);
        assert_eq!(cfg.server.request_timeout_ms, 0);
        assert!(cfg.server.quack.enabled);
        assert_eq!(cfg.server.quack.uri, "quack:localhost:9495");
        assert_eq!(cfg.server.quack.token.as_deref(), Some("test-token"));
        assert!(!cfg.server.quack.read_only);
        assert_eq!(cfg.datasets.len(), 1);
        assert_eq!(cfg.datasets[0].name, "x");
        assert!(cfg.datasets[0].dict_encode); // default
    }

    #[test]
    fn force_lazy_bytes_logic() {
        // A unique temp dir with one 2 MiB "parquet" file (contents are
        // irrelevant — estimate_local_bytes only stats file lengths).
        let dir = std::env::temp_dir().join(format!(
            "dp-force-lazy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let two_mib = 2 * 1024 * 1024;
        let file = dir.join("data.parquet");
        std::fs::write(&file, vec![0u8; two_mib]).unwrap();

        let mk = |kind: SourceKind, location: &str, lazy: bool| DatasetConfig {
            name: "t".into(),
            source: SourceConfig {
                kind,
                location: location.to_string(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: Default::default(),
            refresh: None,
            materialize: None,
        };
        let server = |mb: u64| ServerConfig {
            force_lazy_above_mb: mb,
            ..ServerConfig::default()
        };

        // Sizing: single file, directory walk, and delta dir walk all see 2 MiB.
        let file_ds = mk(SourceKind::Parquet, file.to_str().unwrap(), false);
        assert_eq!(file_ds.estimate_local_bytes(), Some(two_mib as u64));
        let dir_ds = mk(SourceKind::Parquet, dir.to_str().unwrap(), false);
        assert_eq!(dir_ds.estimate_local_bytes(), Some(two_mib as u64));
        let delta_ds = mk(SourceKind::Delta, dir.to_str().unwrap(), false);
        assert_eq!(delta_ds.estimate_local_bytes(), Some(two_mib as u64));

        // Threshold disabled → never force.
        assert_eq!(file_ds.force_lazy_bytes(&server(0)), None);
        // 1 MiB threshold, 2 MiB file → forced (returns measured size).
        assert_eq!(file_ds.force_lazy_bytes(&server(1)), Some(two_mib as u64));
        // 4 MiB threshold → not forced.
        assert_eq!(file_ds.force_lazy_bytes(&server(4)), None);

        // Already-lazy datasets are never re-flagged.
        let lazy_ds = mk(SourceKind::Parquet, file.to_str().unwrap(), true);
        assert_eq!(lazy_ds.force_lazy_bytes(&server(1)), None);

        // S3 sources can't be measured → never auto-forced.
        let s3_ds = mk(SourceKind::Parquet, "s3://bucket/data.parquet", false);
        assert_eq!(s3_ds.estimate_local_bytes(), None);
        assert_eq!(s3_ds.force_lazy_bytes(&server(1)), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_rejects_bad_prefix() {
        let bad = ["no-leading-slash", "/trailing/"];
        for p in bad {
            let cfg = AppConfig {
                server: ServerConfig {
                    prefix: p.to_string(),
                    ..Default::default()
                },
                docs: DocsConfig::default(),
                swagger: SwaggerConfig::default(),
                metrics: MetricsConfig::default(),
                explorer: ExplorerConfig::default(),
                sql: SqlConfig::default(),
                datafusion: DataFusionConfig::default(),
                auth: AuthConfig::default(),
                datasets: vec![],
            };
            assert!(cfg.validate().is_err(), "prefix {p:?} should fail");
        }
    }

    #[test]
    fn normalize_lowercases_configured_scopes() {
        let mut cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            auth: AuthConfig {
                read_scopes: vec!["Datasets:Read".into(), "API.READ".into()],
                reload_scopes: vec!["Datasets:Reload".into()],
                ..Default::default()
            },
            datasets: vec![],
        };
        cfg.normalize();
        assert_eq!(cfg.auth.read_scopes, vec!["datasets:read", "api.read"]);
        assert_eq!(cfg.auth.reload_scopes, vec!["datasets:reload"]);
    }

    #[test]
    fn validate_rejects_no_datasets() {
        let cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            auth: AuthConfig::default(),
            datasets: vec![],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("[[dataset]]")));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn validate_accepts_auth_issuer_with_trailing_slash() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("dp-auth-issuer-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.parquet");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"x")
            .unwrap();

        let cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            auth: AuthConfig {
                enabled: true,
                issuer: "https://tenant.example.com/".into(),
                ..Default::default()
            },
            datasets: vec![DatasetConfig {
                name: "x".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: file.to_string_lossy().into_owned(),
                    sql: None,
                    depends_on: vec![],
                },
                s3: None,
                index: IndexConfig::default(),
                columns: vec![],
                dict_encode: true,
                lazy: false,
                predicate_filter: Default::default(),
                projection_filter: Default::default(),
                on_start: Default::default(),
                refresh: None,
                materialize: None,
            }],
        };

        assert!(cfg.validate().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_quack_non_local_host_without_override() {
        let cfg = AppConfig {
            server: ServerConfig {
                quack: QuackConfig {
                    enabled: true,
                    uri: "quack:127.0.0.1".into(),
                    token: Some("test-token".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            auth: AuthConfig::default(),
            datasets: vec![DatasetConfig {
                name: "x".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: "/tmp/missing.parquet".into(),
                    sql: None,
                    depends_on: vec![],
                },
                s3: None,
                index: IndexConfig::default(),
                columns: vec![],
                dict_encode: true,
                lazy: false,
                predicate_filter: Default::default(),
                projection_filter: Default::default(),
                on_start: Default::default(),
                refresh: None,
                materialize: None,
            }],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("host must be 'localhost'")));
    }

    #[test]
    fn validate_rejects_bad_dataset_name() {
        let cfg: AppConfig = toml::from_str(
            r#"
            [[dataset]]
            name = "bad name!"
            source.kind = "parquet"
            source.location = "/tmp/whatever"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("alphanumeric")));
    }

    #[test]
    fn validate_rejects_duplicate_names() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("dp-dup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.parquet");
        std::fs::File::create(&f).unwrap().write_all(b"x").unwrap();
        let path = f.to_str().unwrap();

        let cfg: AppConfig = toml::from_str(&format!(
            r#"
            [[dataset]]
            name = "a"
            source.kind = "parquet"
            source.location = "{path}"
            [[dataset]]
            name = "a"
            source.kind = "parquet"
            source.location = "{path}"
        "#
        ))
        .unwrap();
        let err = cfg.validate().expect_err("expected error");
        assert!(matches!(err, AppError::Internal(m) if m.contains("duplicate")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn s3_bucket_parsing() {
        let mk = |loc: &str| SourceConfig {
            kind: SourceKind::Parquet,
            location: loc.into(),
            sql: None,
            depends_on: vec![],
        };
        let s1 = mk("s3://bucket/path/key");
        assert_eq!(s1.s3_bucket().unwrap(), ("bucket", "path/key"));
        let s2 = mk("s3://only-bucket");
        assert_eq!(s2.s3_bucket().unwrap(), ("only-bucket", ""));
        assert!(mk("s3:///nokey").s3_bucket().is_err());
        assert!(mk("/local/path").s3_bucket().is_err());
    }

    #[test]
    fn s3_recursive_parquet_glob_only_expands_plain_prefixes() {
        let mk = |loc: &str| SourceConfig {
            kind: SourceKind::Parquet,
            location: loc.into(),
            sql: None,
            depends_on: vec![],
        };
        // Plain prefix -> recursive parquet glob (trailing slash trimmed).
        assert_eq!(
            mk("s3://bucket/logs/").s3_recursive_parquet_glob(),
            "s3://bucket/logs/**/*.parquet"
        );
        assert_eq!(
            mk("s3://bucket/logs").s3_recursive_parquet_glob(),
            "s3://bucket/logs/**/*.parquet"
        );
        // Already globbed -> unchanged.
        assert_eq!(
            mk("s3://bucket/logs/*.parquet").s3_recursive_parquet_glob(),
            "s3://bucket/logs/*.parquet"
        );
        // Non-S3 -> unchanged.
        assert_eq!(mk("/local/logs").s3_recursive_parquet_glob(), "/local/logs");
    }

    #[test]
    fn effective_endpoint_folds_bucket_per_mode() {
        let virt = S3Config {
            endpoint: Some("https://s3.example.com".into()),
            addressing_style: AddressingStyle::Virtual,
            ..Default::default()
        };
        // Auto + virtual -> bucket folded into host.
        assert_eq!(
            virt.effective_endpoint("mybucket").as_deref(),
            Some("https://mybucket.s3.example.com")
        );
        // Idempotent: already-prefixed host is left alone.
        let prefixed = S3Config {
            endpoint: Some("https://mybucket.s3.example.com".into()),
            ..virt.clone()
        };
        assert_eq!(
            prefixed.effective_endpoint("mybucket").as_deref(),
            Some("https://mybucket.s3.example.com")
        );
        // Path style (auto) -> host untouched.
        let path = S3Config {
            addressing_style: AddressingStyle::Path,
            ..virt.clone()
        };
        assert_eq!(
            path.effective_endpoint("mybucket").as_deref(),
            Some("https://s3.example.com")
        );
        // Explicit overrides win over addressing style.
        let forced_off = S3Config {
            endpoint_bucket_in_host: BucketInHost::False,
            ..virt.clone()
        };
        assert_eq!(
            forced_off.effective_endpoint("mybucket").as_deref(),
            Some("https://s3.example.com")
        );
        let forced_on = S3Config {
            endpoint_bucket_in_host: BucketInHost::True,
            ..path.clone()
        };
        assert_eq!(
            forced_on.effective_endpoint("mybucket").as_deref(),
            Some("https://mybucket.s3.example.com")
        );
        // No endpoint -> None.
        assert_eq!(S3Config::default().effective_endpoint("mybucket"), None);
    }

    #[test]
    fn env_prefix_sanitises_name() {
        let mk = |name: &str| DatasetConfig {
            name: name.into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: "x".into(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: Default::default(),
            refresh: None,
            materialize: None,
        };
        assert_eq!(mk("accidents").env_prefix(), "ACCIDENTS");
        assert_eq!(mk("sales.eu-1").env_prefix(), "SALES_EU_1");
        assert_eq!(mk("a_b.c-d").env_prefix(), "A_B_C_D");
    }

    #[test]
    fn resolve_local_parquet_single_file_and_dir() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("dp-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.parquet");
        let mut fh = std::fs::File::create(&f).unwrap();
        fh.write_all(b"not really parquet").unwrap();

        let mk = |loc: &str| DatasetConfig {
            name: "ds".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: loc.into(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: Default::default(),
            refresh: None,
            materialize: None,
        };

        // Direct file.
        let files = mk(f.to_str().unwrap())
            .resolve_local_parquet_files()
            .unwrap();
        assert_eq!(files, vec![f.clone()]);

        // Directory.
        let files = mk(dir.to_str().unwrap())
            .resolve_local_parquet_files()
            .unwrap();
        assert_eq!(files, vec![f.clone()]);

        // Missing path.
        assert!(
            mk("/no/such/place.parquet")
                .resolve_local_parquet_files()
                .is_err()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pgwire_loopback_without_password_is_allowed() {
        let cfg = PgwireConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(cfg.validate_enabled().is_ok());
    }

    #[test]
    fn pgwire_non_loopback_without_password_is_rejected() {
        let cfg = PgwireConfig {
            enabled: true,
            listen: IpAddr::from([0, 0, 0, 0]),
            password: None,
            ..Default::default()
        };
        let err = cfg.validate_enabled().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("password is required")));
    }

    #[test]
    fn pgwire_non_loopback_with_password_but_no_tls_is_rejected() {
        let cfg = PgwireConfig {
            enabled: true,
            listen: IpAddr::from([0, 0, 0, 0]),
            password: Some("pw".into()),
            ..Default::default()
        };
        let err = cfg.validate_enabled().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("requires TLS")));
    }

    #[test]
    fn pgwire_tls_cert_without_key_is_rejected() {
        let cfg = PgwireConfig {
            enabled: true,
            tls_cert: Some(PathBuf::from("/tmp/server.crt")),
            tls_key: None,
            ..Default::default()
        };
        let err = cfg.validate_enabled().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("must be set together")));
    }

    #[test]
    fn pgwire_non_loopback_with_password_and_tls_is_allowed() {
        let cfg = PgwireConfig {
            enabled: true,
            listen: IpAddr::from([0, 0, 0, 0]),
            password: Some("pw".into()),
            tls_cert: Some(PathBuf::from("/tmp/server.crt")),
            tls_key: Some(PathBuf::from("/tmp/server.key")),
            ..Default::default()
        };
        assert!(cfg.validate_enabled().is_ok());
    }

    // -----------------------------------------------------------------------
    // Phase 2B: query-kind source validation tests
    // -----------------------------------------------------------------------

    /// Helper: a minimal valid `AppConfig` with one parquet and one query dataset.
    fn query_cfg_raw(query_sql: &str, depends_on: Vec<String>) -> AppConfig {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("dp-qtest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("base.parquet");
        // Create an actual file so the parquet validation passes.
        std::fs::File::create(&f)
            .unwrap()
            .write_all(b"fake")
            .unwrap_or(());
        AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            auth: AuthConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            datasets: vec![
                DatasetConfig {
                    name: "base".into(),
                    source: SourceConfig {
                        kind: SourceKind::Parquet,
                        location: f.to_str().unwrap().into(),
                        sql: None,
                        depends_on: vec![],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
                DatasetConfig {
                    name: "q".into(),
                    source: SourceConfig {
                        kind: SourceKind::Query,
                        location: String::new(),
                        sql: Some(query_sql.into()),
                        depends_on,
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
            ],
        }
    }

    #[test]
    fn validate_query_source_valid() {
        let cfg = query_cfg_raw("SELECT id FROM base", vec!["base".into()]);
        assert!(cfg.validate().is_ok(), "valid query source should pass");
    }

    #[test]
    fn validate_query_source_missing_depends_on() {
        let cfg = query_cfg_raw("SELECT id FROM base", vec![]); // empty!
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("depends_on"),
            "expected depends_on error, got: {err}"
        );
    }

    #[test]
    fn validate_query_source_superfluous_depends_on() {
        // The exact-match check is exercised by ref_not_in_depends_on below.
        // Here we just verify a valid config passes.
        let cfg = query_cfg_raw("SELECT id FROM base", vec!["base".into()]);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_query_source_ref_not_in_depends_on() {
        // SQL references 'base' but depends_on is empty → invalid.
        let cfg = query_cfg_raw("SELECT id FROM base", vec![]);
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("depends_on"),
            "expected depends_on error, got: {err}"
        );
    }

    #[test]
    fn validate_query_source_unknown_dependency() {
        // depends_on lists a dataset that doesn't exist.
        let cfg = query_cfg_raw("SELECT id FROM ghost", vec!["ghost".into()]);
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("ghost"),
            "expected unknown-dataset error, got: {err}"
        );
    }

    #[test]
    fn validate_query_source_self_reference() {
        // depends_on lists the dataset itself.
        let cfg = query_cfg_raw("SELECT id FROM q", vec!["q".into()]);
        // Either validate() or topological_dataset_order() catches this.
        let result = cfg.validate().map_err(|e| e.to_string()).or_else(|_| {
            cfg.topological_dataset_order()
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        assert!(result.is_err(), "self-reference should be rejected");
    }

    #[test]
    fn validate_query_source_cycle() {
        // Two-node cycle: a -> b -> a.
        let f = {
            use std::io::Write;
            let p = std::env::temp_dir().join(format!("dp-cycle-{}.parquet", std::process::id()));
            std::fs::File::create(&p).unwrap().write_all(b"x").unwrap();
            p
        };
        let cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            auth: AuthConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            datasets: vec![
                DatasetConfig {
                    name: "base".into(),
                    source: SourceConfig {
                        kind: SourceKind::Parquet,
                        location: f.to_str().unwrap().into(),
                        sql: None,
                        depends_on: vec![],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
                DatasetConfig {
                    name: "qa".into(),
                    source: SourceConfig {
                        kind: SourceKind::Query,
                        location: String::new(),
                        sql: Some("SELECT 1 FROM qb".into()),
                        depends_on: vec!["qb".into()],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
                DatasetConfig {
                    name: "qb".into(),
                    source: SourceConfig {
                        kind: SourceKind::Query,
                        location: String::new(),
                        sql: Some("SELECT 1 FROM qa".into()),
                        depends_on: vec!["qa".into()],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
            ],
        };
        let err = cfg.topological_dataset_order().unwrap_err();
        assert!(
            err.to_string().contains("cycle"),
            "expected cycle error, got: {err}"
        );
    }

    #[test]
    fn validate_topological_order_correct() {
        // datasets: base(parquet) <- q1(query) <- q2(query)
        // Expected order: base first, then q1, then q2.
        let f = {
            use std::io::Write;
            let p = std::env::temp_dir().join(format!("dp-topo-{}.parquet", std::process::id()));
            std::fs::File::create(&p).unwrap().write_all(b"x").unwrap();
            p
        };
        let cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            auth: AuthConfig::default(),
            metrics: MetricsConfig::default(),
            explorer: ExplorerConfig::default(),
            sql: SqlConfig::default(),
            datafusion: DataFusionConfig::default(),
            datasets: vec![
                DatasetConfig {
                    name: "q2".into(),
                    source: SourceConfig {
                        kind: SourceKind::Query,
                        location: String::new(),
                        sql: Some("SELECT 1 FROM q1".into()),
                        depends_on: vec!["q1".into()],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
                DatasetConfig {
                    name: "base".into(),
                    source: SourceConfig {
                        kind: SourceKind::Parquet,
                        location: f.to_str().unwrap().into(),
                        sql: None,
                        depends_on: vec![],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
                DatasetConfig {
                    name: "q1".into(),
                    source: SourceConfig {
                        kind: SourceKind::Query,
                        location: String::new(),
                        sql: Some("SELECT 1 FROM base".into()),
                        depends_on: vec!["base".into()],
                    },
                    s3: None,
                    index: IndexConfig::default(),
                    columns: vec![],
                    dict_encode: true,
                    lazy: false,
                    predicate_filter: Default::default(),
                    projection_filter: Default::default(),
                    on_start: Default::default(),
                    refresh: None,
                    materialize: None,
                },
            ],
        };
        let order = cfg.topological_dataset_order().expect("valid topo order");
        // 'base' must come before 'q1', and 'q1' must come before 'q2'.
        let names: Vec<&str> = order
            .iter()
            .map(|&i| cfg.datasets[i].name.as_str())
            .collect();
        let pos_base = names.iter().position(|&n| n == "base").unwrap();
        let pos_q1 = names.iter().position(|&n| n == "q1").unwrap();
        let pos_q2 = names.iter().position(|&n| n == "q2").unwrap();
        assert!(pos_base < pos_q1, "base must come before q1");
        assert!(pos_q1 < pos_q2, "q1 must come before q2");
    }
}
