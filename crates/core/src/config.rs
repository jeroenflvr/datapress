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
    pub auth: AuthConfig,
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
    pub listen: IpAddr,
    /// TCP port.
    pub port: u16,
    /// Number of actix worker threads. `None` (= unset) → one per CPU.
    pub workers: Option<usize>,
    /// Optional URL path prefix — useful when sitting behind a reverse
    /// proxy that rewrites e.g. `/datapress/...` → `/...`. When set, every
    /// route is mounted under this prefix (so the proxy can pass the URL
    /// through unchanged). Must start with `/` and not end with `/`; the
    /// empty string (default) means no prefix.
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
            request_timeout_ms: 30_000,
            shutdown_timeout_secs: 30,
            quack: QuackConfig::default(),
        }
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
/// [`MetricsConfig::path`] (default `/metrics`).
///
/// The endpoint is mounted at a fixed, *unprefixed* path — like the
/// health probes — so a scrape config doesn't need to know about any
/// reverse-proxy `server.prefix`. It is **not** behind the `[auth]`
/// layer: Prometheus scrapers rarely carry bearer tokens, and the
/// endpoint exposes only aggregate request metrics (no row data). Keep
/// it on a network the scraper can reach but the public cannot, e.g. by
/// binding `server.listen` to a private interface.
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
    /// token. Required when `enabled = true`. Must not end in `/`.
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

#[derive(Debug, Clone, Deserialize)]
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
    /// for bounded memory use on large / multi-file sources. Currently
    /// honoured by the DataFusion backend for local parquet.
    #[serde(default)]
    pub lazy: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub kind: SourceKind,
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
            SourceKind::Delta => "delta",
        }
    }
}

/// Non-secret S3 connection settings. Credentials are pulled from env / the
/// AWS credential chain — see [`DatasetConfig::resolved_creds`].
#[derive(Debug, Clone, Deserialize)]
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
            AddressingStyle::Path => "path",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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
        let cfg: AppConfig =
            toml::from_str(&raw).map_err(|e| AppError::Internal(format!("invalid {path}: {e}")))?;
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

        if self.server.quack.enabled {
            self.server.quack.validate_enabled()?;
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
                if o.issuer.ends_with('/') {
                    return Err(AppError::Internal(format!(
                        "swagger.oauth2.issuer must not end with '/' (got '{}')",
                        o.issuer
                    )));
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
            if a.issuer.ends_with('/') {
                return Err(AppError::Internal(format!(
                    "auth.issuer must not end with '/' (got '{}')",
                    a.issuer
                )));
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
                auth: AuthConfig::default(),
                datasets: vec![],
            };
            assert!(cfg.validate().is_err(), "prefix {p:?} should fail");
        }
    }

    #[test]
    fn validate_rejects_no_datasets() {
        let cfg = AppConfig {
            server: ServerConfig::default(),
            docs: DocsConfig::default(),
            swagger: SwaggerConfig::default(),
            metrics: MetricsConfig::default(),
            auth: AuthConfig::default(),
            datasets: vec![],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AppError::Internal(m) if m.contains("[[dataset]]")));
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
            auth: AuthConfig::default(),
            datasets: vec![DatasetConfig {
                name: "x".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: "/tmp/missing.parquet".into(),
                },
                s3: None,
                index: IndexConfig::default(),
                columns: vec![],
                dict_encode: true,
                lazy: false,
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
        };
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
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: "x".into(),
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
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
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
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
}
