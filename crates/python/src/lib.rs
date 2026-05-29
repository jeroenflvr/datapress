//! `datapress` — Python bindings for the DataPress HTTP server.
//!
//! Exposes a small Python API:
//!
//! ```python
//! from datapress import DataPress, DataPressConfig, DatasetConfig, S3Config
//!
//! cfg = DataPressConfig(backend="duckdb", listen="0.0.0.0", port=8000, workers=8)
//! ds  = DatasetConfig(name="accidents", source="data/accidents.parquet")
//! dp  = DataPress(cfg, datasets=[ds])
//! await dp.run()   # blocks until SIGINT
//! ```
//!
//! Both backends are compiled into the wheel; selection is runtime via
//! `DataPressConfig(backend=...)`.

use std::net::IpAddr;
use std::str::FromStr;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use datapress_core::config::{
    AddressingStyle, AppConfig, AuthConfig as CoreAuthConfig, Backend,
    DatasetConfig as CoreDatasetConfig, IndexConfig, IndexMode,
    MetricsConfig as CoreMetricsConfig, S3Config as CoreS3Config, ServerConfig,
    SourceConfig, SourceKind,
};

// ---------------------------------------------------------------------------
// S3Config
// ---------------------------------------------------------------------------

/// S3 / S3-compatible object-store credentials and endpoint config.
///
/// Attached to a :class:`DatasetConfig` whose ``source`` is an ``s3://`` URI.
/// All fields are optional — anything left unset falls back to the standard
/// AWS environment variables (``AWS_REGION``, ``AWS_ACCESS_KEY_ID``, …).
///
/// Args:
///     region (str | None): AWS region, e.g. ``"us-east-1"``.
///     endpoint (str | None): Custom endpoint URL for S3-compatible stores
///         (MinIO, Cloudflare R2, …).
///     addressing_style (str): ``"virtual"`` (default) or ``"path"``.
///     allow_http (bool): Allow plain-HTTP endpoints (default ``False``).
///     access_key_id (str | None): Static credential override.
///     secret_access_key (str | None): Static credential override.
///     session_token (str | None): Temporary STS session token.
#[pyclass(name = "S3Config", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone, Default)]
pub struct PyS3Config {
    #[pyo3(get, set)] pub region:            Option<String>,
    #[pyo3(get, set)] pub endpoint:          Option<String>,
    /// `"virtual"` (default) or `"path"`.
    #[pyo3(get, set)] pub addressing_style:  String,
    #[pyo3(get, set)] pub allow_http:        bool,
    #[pyo3(get, set)] pub access_key_id:     Option<String>,
    #[pyo3(get, set)] pub secret_access_key: Option<String>,
    #[pyo3(get, set)] pub session_token:     Option<String>,
}

#[pymethods]
impl PyS3Config {
    /// Build an :class:`S3Config`.
    ///
    /// Args:
    ///     region (str | None): AWS region, e.g. ``"us-east-1"``.
    ///     endpoint (str | None): Custom S3-compatible endpoint URL.
    ///     addressing_style (str): ``"virtual"`` (default) or ``"path"``.
    ///     allow_http (bool): Allow plain-HTTP endpoints. Defaults to ``False``.
    ///     access_key_id (str | None): Static access-key override.
    ///     secret_access_key (str | None): Static secret-key override.
    ///     session_token (str | None): Temporary STS session token.
    #[new]
    #[pyo3(signature = (
        region            = None,
        endpoint          = None,
        addressing_style  = "virtual".to_string(),
        allow_http        = false,
        access_key_id     = None,
        secret_access_key = None,
        session_token     = None,
    ))]
    fn new(
        region:            Option<String>,
        endpoint:          Option<String>,
        addressing_style:  String,
        allow_http:        bool,
        access_key_id:     Option<String>,
        secret_access_key: Option<String>,
        session_token:     Option<String>,
    ) -> Self {
        Self {
            region, endpoint, addressing_style, allow_http,
            access_key_id, secret_access_key, session_token,
        }
    }
}

impl PyS3Config {
    fn into_core(self) -> PyResult<CoreS3Config> {
        let addressing_style = match self.addressing_style.as_str() {
            "virtual" => AddressingStyle::Virtual,
            "path"    => AddressingStyle::Path,
            other     => return Err(PyValueError::new_err(
                format!("S3Config.addressing_style must be 'virtual' or 'path' (got '{other}')")
            )),
        };
        Ok(CoreS3Config {
            region:            self.region,
            endpoint:          self.endpoint,
            addressing_style,
            allow_http:        self.allow_http,
            access_key_id:     self.access_key_id,
            secret_access_key: self.secret_access_key,
            session_token:     self.session_token,
        })
    }
}

// ---------------------------------------------------------------------------
// DatasetConfig
// ---------------------------------------------------------------------------

/// Declarative description of a single queryable dataset.
///
/// A :class:`DataPress` instance is constructed from a list of these.
/// The ``name`` becomes the URL slug (``/api/datasets/<name>/…``).
///
/// Args:
///     name (str): URL-safe identifier; matches ``[A-Za-z0-9_.\-]+``.
///     source (str): Local path, glob pattern (``data/*.parquet``,
///         ``data/year=*/*.parquet``) or ``s3://bucket/prefix`` URI.
///     format (str): ``"parquet"`` (default) or ``"delta"``.
///     mode (str): Index mode — ``"auto"`` (default), ``"none"`` or ``"list"``.
///     description (str | None): Free-text shown in the listing endpoint.
///     s3 (S3Config | None): Required when ``source`` starts with ``s3://``.
///     columns (list[str] | None): Restrict the dataset to these columns
///         at load time. Only the listed columns are read from the source
///         and held in RAM — everything else is skipped. Names are matched
///         case-insensitively. ``None`` (default) = read all columns.
///     index_columns (list[str] | None): Columns to build an index over
///         when ``mode="list"``.
///     index_max_cardinality (int | None): Upper bound on distinct values
///         per indexed column.
///     lazy (bool): When ``True`` the dataset is **not** materialised into
///         RAM at startup. Queries stream from disk via DataFusion's
///         ``ListingTable``, with column-projection and predicate pushdown.
///         Trades the in-memory hot paths (raw slice, equality index) for
///         bounded memory — essential for wide (hundreds of columns) or
///         multi-file parquet datasets. DataFusion backend, local parquet
///         only.
#[pyclass(name = "DatasetConfig", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone)]
pub struct PyDatasetConfig {
    #[pyo3(get, set)] pub name:                  String,
    #[pyo3(get, set)] pub source:                String,
    /// `"parquet"` (default) or `"delta"`.
    #[pyo3(get, set)] pub format:                String,
    /// `"auto"` (default), `"none"`, or `"list"`.
    #[pyo3(get, set)] pub mode:                  String,
    #[pyo3(get, set)] pub description:           Option<String>,
    #[pyo3(get, set)] pub s3:                    Option<PyS3Config>,
    #[pyo3(get, set)] pub columns:               Option<Vec<String>>,
    /// When ``True`` (default), Utf8 columns that are dictionary-encoded in
    /// the source parquet are read as Arrow ``Dictionary(Int32, Utf8)``.
    /// Set to ``False`` to bypass the override.
    #[pyo3(get, set)] pub dict_encode:           bool,
    #[pyo3(get, set)] pub index_columns:         Option<Vec<String>>,
    #[pyo3(get, set)] pub index_max_cardinality: Option<usize>,
    /// Stream from disk instead of materialising into RAM.
    #[pyo3(get, set)] pub lazy:                  bool,
}

#[pymethods]
impl PyDatasetConfig {
    /// Build a :class:`DatasetConfig`.
    ///
    /// Args:
    ///     name (str): URL-safe identifier.
    ///     source (str): Local path, glob (``data/*.parquet``) or ``s3://`` URI.
    ///     format (str): ``"parquet"`` (default) or ``"delta"``.
    ///     mode (str): ``"auto"`` (default), ``"none"`` or ``"list"``.
    ///     description (str | None): Free-text description.
    ///     s3 (S3Config | None): S3 credentials/endpoint, if ``source`` is ``s3://``.
    ///     columns (list[str] | None): Read only these columns from the source.
    ///     dict_encode (bool): Keep dictionary-encoded Utf8 columns as Arrow
    ///         ``Dictionary(Int32, Utf8)``. Defaults to ``True``. Disable as a
    ///         workaround for null-handling oddities on a specific file.
    ///     index_columns (list[str] | None): Columns to index when ``mode="list"``.
    ///     index_max_cardinality (int | None): Max distinct values per indexed column.
    ///     lazy (bool): Stream from disk instead of loading into RAM.
    ///         DataFusion backend / local parquet only. Defaults to ``False``.
    #[new]
    #[pyo3(signature = (
        name,
        source,
        format                = "parquet".to_string(),
        mode                  = "auto".to_string(),
        description           = None,
        s3                    = None,
        columns               = None,
        dict_encode           = true,
        index_columns         = None,
        index_max_cardinality = None,
        lazy                  = false,
    ))]
    #[allow(clippy::too_many_arguments)] // mirrors the user-facing Python kwargs surface
    fn new(
        name:                  String,
        source:                String,
        format:                String,
        mode:                  String,
        description:           Option<String>,
        s3:                    Option<PyS3Config>,
        columns:               Option<Vec<String>>,
        dict_encode:           bool,
        index_columns:         Option<Vec<String>>,
        index_max_cardinality: Option<usize>,
        lazy:                  bool,
    ) -> Self {
        Self {
            name, source, format, mode, description, s3,
            columns, dict_encode, index_columns, index_max_cardinality, lazy,
        }
    }
}

impl PyDatasetConfig {
    fn into_core(self) -> PyResult<CoreDatasetConfig> {
        let kind = match self.format.as_str() {
            "parquet" => SourceKind::Parquet,
            "delta"   => SourceKind::Delta,
            other     => return Err(PyValueError::new_err(
                format!("DatasetConfig.format must be 'parquet' or 'delta' (got '{other}')")
            )),
        };
        let mode = match self.mode.as_str() {
            "auto" => IndexMode::Auto,
            "none" => IndexMode::None,
            "list" => IndexMode::List,
            other  => return Err(PyValueError::new_err(
                format!("DatasetConfig.mode must be 'auto', 'none', or 'list' (got '{other}')")
            )),
        };

        let mut index = IndexConfig { mode, ..IndexConfig::default() };
        if let Some(cols) = self.index_columns {
            index.columns = cols;
        }
        if let Some(n) = self.index_max_cardinality {
            index.max_cardinality = n;
        }

        let s3 = self.s3.map(|s| s.into_core()).transpose()?;

        Ok(CoreDatasetConfig {
            name:    self.name,
            source:  SourceConfig { kind, location: self.source },
            s3,
            index,
            columns:     self.columns.unwrap_or_default(),
            dict_encode: self.dict_encode,
            lazy:        self.lazy,
        })
    }
}

// ---------------------------------------------------------------------------
// DataPressConfig
// ---------------------------------------------------------------------------

/// Server-side configuration for a :class:`DataPress` instance.
///
/// Selects the query engine and controls how the HTTP server binds.
///
/// Args:
///     backend (str): ``"duckdb"`` (default) or ``"datafusion"``. Both are
///         compiled into the wheel; selection is purely runtime.
///     listen (str): Bind address. Defaults to ``"127.0.0.1"`` — use
///         ``"0.0.0.0"`` to expose the port outside localhost.
///     port (int): TCP port. Default ``8000``.
///     workers (int | None): Number of actix worker threads. ``None``
///         (default) means one per CPU.
///     prefix (str): URL path prefix mounted in front of every route, e.g.
///         ``"/datapress"`` when running behind a reverse proxy that passes
///         the path through unchanged. Must start with ``/`` and not end
///         with ``/``. Empty string (default) = mount at root.
///     compress (bool): Enable HTTP response compression negotiated via
///         the ``Accept-Encoding`` request header (gzip / brotli / zstd).
///         Default ``True``. Disable when behind a proxy that already
///         compresses.
///     max_body_bytes (int): Maximum accepted JSON request body, in bytes.
///         Larger bodies are rejected with ``413``. Default ``1_048_576``
///         (1 MiB).
///     request_timeout_ms (int): Per-request handler timeout, in
///         milliseconds. ``0`` disables the timeout. Default ``30_000``.
///     shutdown_timeout_secs (int): Grace period for in-flight requests
///         after the server receives ``SIGTERM`` / ``SIGINT``, in
///         seconds. Default ``30``.
#[pyclass(name = "DataPressConfig", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone)]
pub struct PyDataPressConfig {
    /// `"duckdb"` or `"datafusion"`.
    #[pyo3(get, set)] pub backend: String,
    #[pyo3(get, set)] pub listen:  String,
    #[pyo3(get, set)] pub port:    u16,
    #[pyo3(get, set)] pub workers: Option<usize>,
    /// Optional URL prefix for all routes — e.g. `"/datapress"` when sitting
    /// behind a reverse proxy that passes the path through unchanged.
    #[pyo3(get, set)] pub prefix:  String,
    /// Negotiate response compression via `Accept-Encoding`.
    #[pyo3(get, set)] pub compress: bool,
    /// Max accepted request body, in bytes.
    #[pyo3(get, set)] pub max_body_bytes:     usize,
    /// Per-request handler timeout, in ms. `0` = disabled.
    #[pyo3(get, set)] pub request_timeout_ms: u64,
    /// Grace period for in-flight requests on shutdown, in seconds.
    #[pyo3(get, set)] pub shutdown_timeout_secs: u64,
    /// Expose a Prometheus metrics endpoint. Requires the wheel to be built
    /// with the ``metrics`` Cargo feature. Default ``False``.
    #[pyo3(get, set)] pub metrics_enabled: bool,
    /// Path the metrics endpoint is served on. Must start with ``/`` and not
    /// end with ``/``. The endpoint is unauthenticated — isolate it at the
    /// network layer. Default ``"/metrics"``.
    #[pyo3(get, set)] pub metrics_path: String,
}

#[pymethods]
impl PyDataPressConfig {
    /// Build a :class:`DataPressConfig`.
    ///
    /// Args:
    ///     backend (str): ``"duckdb"`` (default) or ``"datafusion"``.
    ///     listen (str): Bind address. Default ``"127.0.0.1"``.
    ///     port (int): TCP port. Default ``8000``.
    ///     workers (int | None): Worker thread count. ``None`` = one per CPU.
    ///     prefix (str): URL prefix for all routes (e.g. ``"/datapress"``).
    ///         Must start with ``/`` and not end with ``/``. Default ``""``.
    ///     compress (bool): Enable response compression negotiation.
    ///         Default ``True``.
    ///     max_body_bytes (int): Max accepted JSON body, in bytes.
    ///         Default ``1_048_576``.
    ///     request_timeout_ms (int): Per-request handler timeout, in ms.
    ///         ``0`` disables. Default ``30_000``.
    ///     shutdown_timeout_secs (int): Grace period for in-flight
    ///         requests on ``SIGTERM``/``SIGINT``, in seconds.
    ///         Default ``30``.
    ///     metrics_enabled (bool): Expose a Prometheus metrics endpoint.
    ///         Requires a wheel built with the ``metrics`` feature.
    ///         Default ``False``.
    ///     metrics_path (str): Path the metrics endpoint is served on.
    ///         Must start with ``/`` and not end with ``/``. The endpoint
    ///         is unauthenticated. Default ``"/metrics"``.
    #[new]
    #[pyo3(signature = (
        backend            = "duckdb".to_string(),
        listen             = "127.0.0.1".to_string(),
        port               = 8000,
        workers            = None,
        prefix             = String::new(),
        compress           = true,
        max_body_bytes     = 1_048_576,
        request_timeout_ms = 30_000,
        shutdown_timeout_secs = 30,
        metrics_enabled    = false,
        metrics_path       = "/metrics".to_string(),
    ))]
    #[allow(clippy::too_many_arguments)] // user-facing kwargs surface
    fn new(
        backend:            String,
        listen:             String,
        port:               u16,
        workers:            Option<usize>,
        prefix:             String,
        compress:           bool,
        max_body_bytes:     usize,
        request_timeout_ms: u64,
        shutdown_timeout_secs: u64,
        metrics_enabled:    bool,
        metrics_path:       String,
    ) -> Self {
        Self {
            backend, listen, port, workers, prefix, compress,
            max_body_bytes, request_timeout_ms, shutdown_timeout_secs,
            metrics_enabled, metrics_path,
        }
    }
}

impl PyDataPressConfig {
    fn into_core(self) -> PyResult<ServerConfig> {
        let backend = match self.backend.as_str() {
            "duckdb"     => Backend::Duckdb,
            "datafusion" => Backend::Datafusion,
            other        => return Err(PyValueError::new_err(
                format!("DataPressConfig.backend must be 'duckdb' or 'datafusion' (got '{other}')")
            )),
        };
        let listen = IpAddr::from_str(&self.listen).map_err(|e| {
            PyValueError::new_err(format!("invalid listen address '{}': {e}", self.listen))
        })?;
        if !self.prefix.is_empty() {
            if !self.prefix.starts_with('/') {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.prefix must start with '/' (got '{}')", self.prefix
                )));
            }
            if self.prefix.ends_with('/') {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.prefix must not end with '/' (got '{}')", self.prefix
                )));
            }
        }
        Ok(ServerConfig {
            backend,
            listen,
            port: self.port,
            workers: self.workers,
            prefix: self.prefix,
            compress: self.compress,
            max_body_bytes:     self.max_body_bytes,
            request_timeout_ms: self.request_timeout_ms,
            shutdown_timeout_secs: self.shutdown_timeout_secs,
        })
    }

    /// Build the core `MetricsConfig` from the Python-facing fields,
    /// validating the path the same way `AppConfig::validate()` does.
    fn metrics_into_core(&self) -> PyResult<CoreMetricsConfig> {
        if !self.metrics_path.starts_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.metrics_path must start with '/' (got '{}')",
                self.metrics_path
            )));
        }
        if self.metrics_path.len() > 1 && self.metrics_path.ends_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.metrics_path must not end with '/' (got '{}')",
                self.metrics_path
            )));
        }
        Ok(CoreMetricsConfig {
            enabled: self.metrics_enabled,
            path:    self.metrics_path.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// AuthConfig
// ---------------------------------------------------------------------------

/// OIDC / OAuth2 bearer-token enforcement for the HTTP API.
///
/// Pass an instance to :class:`DataPress` as the ``auth`` kwarg. Requires
/// the wheel to be built with the ``auth`` Cargo feature (the published
/// wheels include it). When ``enabled=False`` (default) the entire auth
/// layer is a no-op and existing ``X-Admin-Token`` semantics apply.
///
/// Args:
///     enabled (bool): Master switch. Default ``False``.
///     issuer (str): OIDC issuer URL — must equal the JWT ``iss`` claim.
///         Required when ``enabled=True``. Must be ``https://...`` (or
///         ``http://localhost...`` for local development).
///     audience (str): Expected JWT ``aud`` claim. Empty disables ``aud``
///         validation (not recommended in production).
///     read_scopes (list[str]): Scopes required on every read endpoint
///         (``GET /datasets``, schema, query, count). Empty list = any
///         valid token is enough.
///     reload_scopes (list[str]): Scopes required on ``POST .../reload``.
///     anonymous_read (bool): Allow unauthenticated reads. Default
///         ``False``.
///     algorithms (list[str]): Allowed JWS algorithms. Default
///         ``["RS256"]``. Only RS/ES/PS variants are accepted.
///     leeway_secs (int): Clock-skew tolerance for ``exp``/``nbf``.
///         Default ``60``.
///     jwks_refresh_secs (int): Background JWKS refresh interval.
///         Default ``3600`` (clamped to ≥ 60).
///     tenant_claim (str): JSON-pointer into the JWT claims to extract a
///         tenant id (e.g. ``"/tid"`` for Entra ID). Empty disables.
///     allowed_tenants (list[str]): If non-empty, the token's tenant
///         value must be in this list. Has no effect without
///         ``tenant_claim``.
///     admin_token_fallback (bool): Keep ``X-Admin-Token`` working in
///         parallel with OIDC for ``POST .../reload``. Default ``True``.
///     start_degraded (bool): If ``True`` (default) the server starts
///         even when the IdP is unreachable and serves 503 for
///         authenticated requests until JWKS becomes available.
///         If ``False``, an unreachable IdP at boot fails startup.
#[pyclass(name = "AuthConfig", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone)]
pub struct PyAuthConfig {
    #[pyo3(get, set)] pub enabled:              bool,
    #[pyo3(get, set)] pub issuer:               String,
    #[pyo3(get, set)] pub audience:             String,
    #[pyo3(get, set)] pub read_scopes:          Vec<String>,
    #[pyo3(get, set)] pub reload_scopes:        Vec<String>,
    #[pyo3(get, set)] pub anonymous_read:       bool,
    #[pyo3(get, set)] pub algorithms:           Vec<String>,
    #[pyo3(get, set)] pub leeway_secs:          u64,
    #[pyo3(get, set)] pub jwks_refresh_secs:    u64,
    #[pyo3(get, set)] pub tenant_claim:         String,
    #[pyo3(get, set)] pub allowed_tenants:      Vec<String>,
    #[pyo3(get, set)] pub admin_token_fallback: bool,
    #[pyo3(get, set)] pub start_degraded:       bool,
}

impl Default for PyAuthConfig {
    fn default() -> Self {
        let d = CoreAuthConfig::default();
        Self {
            enabled:              d.enabled,
            issuer:               d.issuer,
            audience:             d.audience,
            read_scopes:          d.read_scopes,
            reload_scopes:        d.reload_scopes,
            anonymous_read:       d.anonymous_read,
            algorithms:           d.algorithms,
            leeway_secs:          d.leeway_secs,
            jwks_refresh_secs:    d.jwks_refresh_secs,
            tenant_claim:         d.tenant_claim,
            allowed_tenants:      d.allowed_tenants,
            admin_token_fallback: d.admin_token_fallback,
            start_degraded:       d.start_degraded,
        }
    }
}

#[pymethods]
impl PyAuthConfig {
    /// Build an :class:`AuthConfig`. All kwargs match the TOML ``[auth]``
    /// block; see the class docstring for semantics.
    #[new]
    #[pyo3(signature = (
        enabled              = false,
        issuer               = String::new(),
        audience             = String::new(),
        read_scopes          = Vec::new(),
        reload_scopes        = Vec::new(),
        anonymous_read       = false,
        algorithms           = vec!["RS256".to_string()],
        leeway_secs          = 60,
        jwks_refresh_secs    = 3600,
        tenant_claim         = String::new(),
        allowed_tenants      = Vec::new(),
        admin_token_fallback = true,
        start_degraded       = true,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        enabled:              bool,
        issuer:               String,
        audience:             String,
        read_scopes:          Vec<String>,
        reload_scopes:        Vec<String>,
        anonymous_read:       bool,
        algorithms:           Vec<String>,
        leeway_secs:          u64,
        jwks_refresh_secs:    u64,
        tenant_claim:         String,
        allowed_tenants:      Vec<String>,
        admin_token_fallback: bool,
        start_degraded:       bool,
    ) -> Self {
        Self {
            enabled, issuer, audience, read_scopes, reload_scopes,
            anonymous_read, algorithms, leeway_secs, jwks_refresh_secs,
            tenant_claim, allowed_tenants, admin_token_fallback,
            start_degraded,
        }
    }
}

impl PyAuthConfig {
    fn into_core(self) -> PyResult<CoreAuthConfig> {
        if self.enabled {
            if self.issuer.is_empty() {
                return Err(PyValueError::new_err(
                    "AuthConfig.issuer is required when enabled=True",
                ));
            }
            if self.tenant_claim.is_empty() && !self.allowed_tenants.is_empty() {
                return Err(PyValueError::new_err(
                    "AuthConfig.allowed_tenants requires tenant_claim to be set",
                ));
            }
            if !self.tenant_claim.is_empty() && !self.tenant_claim.starts_with('/') {
                return Err(PyValueError::new_err(
                    "AuthConfig.tenant_claim must be a JSON-pointer (start with '/')",
                ));
            }
        }
        Ok(CoreAuthConfig {
            enabled:              self.enabled,
            issuer:               self.issuer,
            audience:             self.audience,
            read_scopes:          self.read_scopes,
            reload_scopes:        self.reload_scopes,
            anonymous_read:       self.anonymous_read,
            algorithms:           self.algorithms,
            leeway_secs:          self.leeway_secs,
            jwks_refresh_secs:    self.jwks_refresh_secs,
            tenant_claim:         self.tenant_claim,
            allowed_tenants:      self.allowed_tenants,
            admin_token_fallback: self.admin_token_fallback,
            start_degraded:       self.start_degraded,
        })
    }
}

// ---------------------------------------------------------------------------
// DataPress
// ---------------------------------------------------------------------------

/// A configured DataPress HTTP server, ready to :meth:`run`.
///
/// Construct with a :class:`DataPressConfig` and a list of
/// :class:`DatasetConfig`. The server is not started until
/// :meth:`run` is awaited.
///
/// Args:
///     config (DataPressConfig): Server-side configuration.
///     datasets (list[DatasetConfig]): One or more datasets to publish.
///
/// Example:
///     >>> import asyncio
///     >>> from datapress import DataPress, DataPressConfig, DatasetConfig
///     >>> dp = DataPress(
///     ...     DataPressConfig(backend="datafusion", port=8000),
///     ...     datasets=[DatasetConfig(name="accidents", source="data/x.parquet")],
///     ... )
///     >>> asyncio.run(dp.run())
#[pyclass(name = "DataPress", module = "datap_rs.datapress")]
pub struct PyDataPress {
    cfg: AppConfig,
}

#[pymethods]
impl PyDataPress {
    /// Build a :class:`DataPress` instance.
    ///
    /// Args:
    ///     config (DataPressConfig): Server-side configuration.
    ///     datasets (list[DatasetConfig]): Datasets to publish. Must be non-empty.
    ///     auth (AuthConfig | None): Optional OIDC/OAuth2 enforcement.
    ///         Defaults to disabled.
    ///
    /// Raises:
    ///     ValueError: If any field is invalid (bad backend name, bad prefix,
    ///         duplicate dataset name, …).
    #[new]
    #[pyo3(signature = (config, datasets, auth = None))]
    fn new(
        config:   PyDataPressConfig,
        datasets: Vec<PyDatasetConfig>,
        auth:     Option<PyAuthConfig>,
    ) -> PyResult<Self> {
        let metrics = config.metrics_into_core()?;
        let server = config.into_core()?;
        let datasets = datasets.into_iter()
            .map(|d| d.into_core())
            .collect::<PyResult<Vec<_>>>()?;
        let auth = match auth {
            Some(a) => a.into_core()?,
            None    => CoreAuthConfig::default(),
        };
        Ok(Self { cfg: AppConfig {
            server,
            docs:     datapress_core::config::DocsConfig::default(),
            swagger:  datapress_core::config::SwaggerConfig::default(),
            metrics,
            auth,
            datasets,
        } })
    }

    /// Start the HTTP server and run until SIGINT (Ctrl-C).
    ///
    /// Returns a Python awaitable that resolves when the server stops.
    /// The server runs on a dedicated OS thread with its own actix
    /// runtime, so the caller's asyncio event loop is not blocked.
    ///
    /// Returns:
    ///     Awaitable[None]: Completes cleanly on graceful shutdown.
    ///
    /// Raises:
    ///     RuntimeError: If the server thread panics or bind fails.
    fn run<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cfg = clone_app_config(&self.cfg);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (tx, rx) = tokio::sync::oneshot::channel::<std::io::Result<()>>();
            std::thread::spawn(move || {
                let result = actix_web::rt::System::new().block_on(async move {
                    match cfg.server.backend {
                        Backend::Duckdb     => datapress_duckdb::serve(cfg).await,
                        Backend::Datafusion => datapress_datafusion::serve(cfg).await,
                    }
                });
                let _ = tx.send(result);
            });
            match rx.await {
                Ok(Ok(()))  => Ok(()),
                Ok(Err(e))  => Err(PyRuntimeError::new_err(e.to_string())),
                Err(_)      => Err(PyRuntimeError::new_err("DataPress server thread panicked")),
            }
        })
    }
}

/// `AppConfig` doesn't derive `Clone` upstream (it holds parsed TOML state).
/// We reconstruct it field-by-field — every contained type is `Clone`.
fn clone_app_config(cfg: &AppConfig) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            backend: cfg.server.backend,
            listen:  cfg.server.listen,
            port:    cfg.server.port,
            workers: cfg.server.workers,
            prefix:  cfg.server.prefix.clone(),
            compress: cfg.server.compress,
            max_body_bytes:     cfg.server.max_body_bytes,
            request_timeout_ms: cfg.server.request_timeout_ms,
            shutdown_timeout_secs: cfg.server.shutdown_timeout_secs,
        },
        docs:     cfg.docs.clone(),
        swagger:  cfg.swagger.clone(),
        metrics:  cfg.metrics.clone(),
        auth:     cfg.auth.clone(),
        datasets: cfg.datasets.clone(),
    }
}

// ---------------------------------------------------------------------------
// Module entry point
// ---------------------------------------------------------------------------

#[pymodule]
fn datapress(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Best-effort init of env_logger so RUST_LOG=info works from Python.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    ).try_init();

    m.add_class::<PyS3Config>()?;
    m.add_class::<PyDatasetConfig>()?;
    m.add_class::<PyDataPressConfig>()?;
    m.add_class::<PyAuthConfig>()?;
    m.add_class::<PyDataPress>()?;
    Ok(())
}
