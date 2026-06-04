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
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use datapress_core::config::{
    AddressingStyle, AppConfig, AuthConfig as CoreAuthConfig, Backend, BucketInHost,
    DatasetConfig as CoreDatasetConfig, ExplorerConfig as CoreExplorerConfig, IndexConfig,
    IndexMode, MetricsConfig as CoreMetricsConfig, Partitioning, S3Config as CoreS3Config,
    ServerConfig, SourceConfig, SourceKind, SqlConfig as CoreSqlConfig,
    SwaggerConfig as CoreSwaggerConfig, SwaggerOAuth2Config as CoreSwaggerOAuth2Config,
};

// ---------------------------------------------------------------------------
// HMACKeyPair
// ---------------------------------------------------------------------------

/// An access-key / secret-key pair returned by an
/// :class:`S3Config` ``credentials_provider`` callable.
///
/// The provider is invoked once and the resulting pair is cached
/// indefinitely for that :class:`S3Config`, so the callable is never
/// asked for credentials a second time.
///
/// Args:
///     access_key (str): The S3 access-key id.
///     secret_key (str): The S3 secret access-key.
#[pyclass(name = "HMACKeyPair", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone)]
pub struct PyHMACKeyPair {
    #[pyo3(get, set)]
    pub access_key: String,
    #[pyo3(get, set)]
    pub secret_key: String,
}

#[pymethods]
impl PyHMACKeyPair {
    /// Build an :class:`HMACKeyPair`.
    ///
    /// Args:
    ///     access_key (str): The S3 access-key id.
    ///     secret_key (str): The S3 secret access-key.
    #[new]
    fn new(access_key: String, secret_key: String) -> Self {
        Self {
            access_key,
            secret_key,
        }
    }

    /// Redacted repr — never prints the secret key.
    fn __repr__(&self) -> String {
        format!("HMACKeyPair(access_key={:?}, secret_key='***')", self.access_key)
    }
}

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
///     credentials_provider (Callable[[], HMACKeyPair] | None): A zero-arg
///         callable that returns an :class:`HMACKeyPair`. When provided it
///         takes precedence over the static ``access_key_id`` /
///         ``secret_access_key`` / ``session_token`` fields (those are
///         ignored). The callable is invoked once when the server is built
///         and the returned pair is cached indefinitely.
#[pyclass(name = "S3Config", module = "datap_rs.datapress", from_py_object)]
#[derive(Clone)]
pub struct PyS3Config {
    #[pyo3(get, set)]
    pub region: Option<String>,
    #[pyo3(get, set)]
    pub endpoint: Option<String>,
    /// `"virtual"` (default) or `"path"`.
    #[pyo3(get, set)]
    pub addressing_style: String,
    #[pyo3(get, set)]
    pub allow_http: bool,
    #[pyo3(get, set)]
    pub access_key_id: Option<String>,
    #[pyo3(get, set)]
    pub secret_access_key: Option<String>,
    #[pyo3(get, set)]
    pub session_token: Option<String>,
    /// Hive partition discovery: `"auto"` (default), `"hive"`, or `"none"`.
    #[pyo3(get, set)]
    pub partitioning: String,
    /// Whether to fold the bucket name into the endpoint host:
    /// `"auto"` (default, follows `addressing_style`), `"true"`, or `"false"`.
    #[pyo3(get, set)]
    pub endpoint_bucket_in_host: String,
    /// Optional zero-arg Python callable returning an `HMACKeyPair`.
    /// When set, it overrides the static HMAC credentials above. Stored
    /// behind an `Arc` because `Py<PyAny>` is not `Clone` on its own.
    /// Exposed to Python via the `credentials_provider` getter/setter.
    credentials_provider: Option<Arc<Py<PyAny>>>,
    /// Caches the keypair returned by `credentials_provider`, so the
    /// callable is invoked at most once. Shared across clones of this
    /// config (the cache lives behind an `Arc`).
    cached_creds: Arc<OnceLock<PyHMACKeyPair>>,
}

impl Default for PyS3Config {
    fn default() -> Self {
        Self {
            region: None,
            endpoint: None,
            addressing_style: "virtual".to_string(),
            allow_http: false,
            access_key_id: None,
            secret_access_key: None,
            session_token: None,
            partitioning: "auto".to_string(),
            endpoint_bucket_in_host: "auto".to_string(),
            credentials_provider: None,
            cached_creds: Arc::new(OnceLock::new()),
        }
    }
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
    ///     partitioning (str): Hive partition discovery: ``"auto"`` (default),
    ///         ``"hive"``, or ``"none"``.
    ///     endpoint_bucket_in_host (str): Fold the bucket into the endpoint
    ///         host: ``"auto"`` (default, follows ``addressing_style``),
    ///         ``"true"``, or ``"false"``.
    ///     credentials_provider (Callable[[], HMACKeyPair] | None): Zero-arg
    ///         callable returning an :class:`HMACKeyPair`. Overrides (and
    ///         ignores) the static HMAC credentials when set; invoked once
    ///         and cached indefinitely.
    #[new]
    #[pyo3(signature = (
        region            = None,
        endpoint          = None,
        addressing_style  = "virtual".to_string(),
        allow_http        = false,
        access_key_id     = None,
        secret_access_key = None,
        session_token     = None,
        partitioning      = "auto".to_string(),
        endpoint_bucket_in_host = "auto".to_string(),
        credentials_provider = None,
    ))]
    #[allow(clippy::too_many_arguments)] // mirrors the user-facing Python kwargs surface
    fn new(
        region: Option<String>,
        endpoint: Option<String>,
        addressing_style: String,
        allow_http: bool,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
        session_token: Option<String>,
        partitioning: String,
        endpoint_bucket_in_host: String,
        credentials_provider: Option<Py<PyAny>>,
    ) -> Self {
        Self {
            region,
            endpoint,
            addressing_style,
            allow_http,
            access_key_id,
            secret_access_key,
            session_token,
            partitioning,
            endpoint_bucket_in_host,
            credentials_provider: credentials_provider.map(Arc::new),
            cached_creds: Arc::new(OnceLock::new()),
        }
    }

    /// The configured ``credentials_provider`` callable, or ``None``.
    #[getter]
    fn get_credentials_provider(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.credentials_provider
            .as_ref()
            .map(|p| p.clone_ref(py))
    }

    /// Set or clear the ``credentials_provider`` callable.
    #[setter]
    fn set_credentials_provider(&mut self, value: Option<Py<PyAny>>) {
        self.credentials_provider = value.map(Arc::new);
    }
}

impl PyS3Config {
    /// Resolve the keypair from `credentials_provider`, caching it
    /// indefinitely so the callable runs at most once. Errors if the
    /// callable raises or returns an `HMACKeyPair` with an empty key.
    fn resolve_provider_creds(
        &self,
        py: Python<'_>,
        provider: &Py<PyAny>,
    ) -> PyResult<PyHMACKeyPair> {
        if let Some(cached) = self.cached_creds.get() {
            return Ok(cached.clone());
        }
        let result = provider.bind(py).call0()?;
        let pair: PyHMACKeyPair = result.extract()?;
        if pair.access_key.is_empty() || pair.secret_key.is_empty() {
            return Err(PyValueError::new_err(
                "S3Config.credentials_provider must return an HMACKeyPair with \
                 non-empty access_key and secret_key",
            ));
        }
        // First writer wins; concurrent callers are serialised by the GIL.
        let _ = self.cached_creds.set(pair.clone());
        Ok(pair)
    }

    fn into_core(self, py: Python<'_>) -> PyResult<CoreS3Config> {
        let addressing_style = match self.addressing_style.as_str() {
            "virtual" => AddressingStyle::Virtual,
            "path" => AddressingStyle::Path,
            other => {
                return Err(PyValueError::new_err(format!(
                    "S3Config.addressing_style must be 'virtual' or 'path' (got '{other}')"
                )));
            }
        };

        let partitioning = match self.partitioning.as_str() {
            "auto" => Partitioning::Auto,
            "hive" => Partitioning::Hive,
            "none" => Partitioning::None,
            other => {
                return Err(PyValueError::new_err(format!(
                    "S3Config.partitioning must be 'auto', 'hive', or 'none' (got '{other}')"
                )));
            }
        };

        let endpoint_bucket_in_host = match self.endpoint_bucket_in_host.as_str() {
            "auto" => BucketInHost::Auto,
            "true" => BucketInHost::True,
            "false" => BucketInHost::False,
            other => {
                return Err(PyValueError::new_err(format!(
                    "S3Config.endpoint_bucket_in_host must be 'auto', 'true', or 'false' (got '{other}')"
                )));
            }
        };

        // A credentials provider takes precedence over (and ignores) the
        // static HMAC credentials. The session token is dropped too, since
        // the provider yields a long-lived access/secret keypair.
        let (access_key_id, secret_access_key, session_token) =
            match &self.credentials_provider {
                Some(provider) => {
                    let pair = self.resolve_provider_creds(py, provider.as_ref())?;
                    (Some(pair.access_key), Some(pair.secret_key), None)
                }
                None => (
                    self.access_key_id.clone(),
                    self.secret_access_key.clone(),
                    self.session_token.clone(),
                ),
            };

        Ok(CoreS3Config {
            region: self.region.clone(),
            endpoint: self.endpoint.clone(),
            addressing_style,
            allow_http: self.allow_http,
            access_key_id,
            secret_access_key,
            session_token,
            partitioning,
            endpoint_bucket_in_host,
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
    #[pyo3(get, set)]
    pub name: String,
    #[pyo3(get, set)]
    pub source: String,
    /// `"parquet"` (default) or `"delta"`.
    #[pyo3(get, set)]
    pub format: String,
    /// `"auto"` (default), `"none"`, or `"list"`.
    #[pyo3(get, set)]
    pub mode: String,
    #[pyo3(get, set)]
    pub description: Option<String>,
    #[pyo3(get, set)]
    pub s3: Option<PyS3Config>,
    #[pyo3(get, set)]
    pub columns: Option<Vec<String>>,
    /// When ``True`` (default), Utf8 columns that are dictionary-encoded in
    /// the source parquet are read as Arrow ``Dictionary(Int32, Utf8)``.
    /// Set to ``False`` to bypass the override.
    #[pyo3(get, set)]
    pub dict_encode: bool,
    #[pyo3(get, set)]
    pub index_columns: Option<Vec<String>>,
    #[pyo3(get, set)]
    pub index_max_cardinality: Option<usize>,
    /// Stream from disk instead of materialising into RAM.
    #[pyo3(get, set)]
    pub lazy: bool,
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
        name: String,
        source: String,
        format: String,
        mode: String,
        description: Option<String>,
        s3: Option<PyS3Config>,
        columns: Option<Vec<String>>,
        dict_encode: bool,
        index_columns: Option<Vec<String>>,
        index_max_cardinality: Option<usize>,
        lazy: bool,
    ) -> Self {
        Self {
            name,
            source,
            format,
            mode,
            description,
            s3,
            columns,
            dict_encode,
            index_columns,
            index_max_cardinality,
            lazy,
        }
    }
}

impl PyDatasetConfig {
    fn into_core(self, py: Python<'_>) -> PyResult<CoreDatasetConfig> {
        let kind = match self.format.as_str() {
            "parquet" => SourceKind::Parquet,
            "delta" => SourceKind::Delta,
            other => {
                return Err(PyValueError::new_err(format!(
                    "DatasetConfig.format must be 'parquet' or 'delta' (got '{other}')"
                )));
            }
        };
        let mode = match self.mode.as_str() {
            "auto" => IndexMode::Auto,
            "none" => IndexMode::None,
            "list" => IndexMode::List,
            other => {
                return Err(PyValueError::new_err(format!(
                    "DatasetConfig.mode must be 'auto', 'none', or 'list' (got '{other}')"
                )));
            }
        };

        let mut index = IndexConfig {
            mode,
            ..IndexConfig::default()
        };
        if let Some(cols) = self.index_columns {
            index.columns = cols;
        }
        if let Some(n) = self.index_max_cardinality {
            index.max_cardinality = n;
        }

        let s3 = self.s3.map(|s| s.into_core(py)).transpose()?;

        Ok(CoreDatasetConfig {
            name: self.name,
            source: SourceConfig {
                kind,
                location: self.source,
            },
            s3,
            index,
            columns: self.columns.unwrap_or_default(),
            dict_encode: self.dict_encode,
            lazy: self.lazy,
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
///     max_page_size (int): Maximum rows returned by one query page.
///         Larger ``page_size`` values are clamped. Default ``100_000``.
///     request_timeout_ms (int): Per-request handler timeout, in
///         milliseconds. ``0`` disables the timeout. Default ``30_000``.
///     shutdown_timeout_secs (int): Grace period for in-flight requests
///         after the server receives ``SIGTERM`` / ``SIGINT``, in
///         seconds. Default ``30``.
///     quack_enabled (bool): Enable DuckDB's experimental Quack remote
///         protocol server. DuckDB backend only. Default ``False``.
///     quack_uri (str): Quack listen URI. Default ``"quack:localhost"``.
///     quack_token (str | None): Optional explicit Quack auth token. If
///         unset, Quack generates one and DataPress logs it at startup.
///     quack_allow_other_hostname (bool): Allow non-local bind addresses.
///         Use only behind a TLS-terminating reverse proxy. Default ``False``.
///     quack_read_only (bool): Install a read-only Quack authorization hook.
///         Default ``True``.
#[pyclass(
    name = "DataPressConfig",
    module = "datap_rs.datapress",
    from_py_object
)]
#[derive(Clone)]
pub struct PyDataPressConfig {
    /// `"duckdb"` or `"datafusion"`.
    #[pyo3(get, set)]
    pub backend: String,
    #[pyo3(get, set)]
    pub listen: String,
    #[pyo3(get, set)]
    pub port: u16,
    #[pyo3(get, set)]
    pub workers: Option<usize>,
    /// Optional URL prefix for all routes — e.g. `"/datapress"` when sitting
    /// behind a reverse proxy that passes the path through unchanged.
    #[pyo3(get, set)]
    pub prefix: String,
    /// Negotiate response compression via `Accept-Encoding`.
    #[pyo3(get, set)]
    pub compress: bool,
    /// Max accepted request body, in bytes.
    #[pyo3(get, set)]
    pub max_body_bytes: usize,
    /// Max rows returned by one query page.
    #[pyo3(get, set)]
    pub max_page_size: u64,
    /// Per-request handler timeout, in ms. `0` = disabled.
    #[pyo3(get, set)]
    pub request_timeout_ms: u64,
    /// Grace period for in-flight requests on shutdown, in seconds.
    #[pyo3(get, set)]
    pub shutdown_timeout_secs: u64,
    /// Enable DuckDB's experimental Quack remote protocol server.
    #[pyo3(get, set)]
    pub quack_enabled: bool,
    /// Quack listen URI, for example `"quack:localhost"`.
    #[pyo3(get, set)]
    pub quack_uri: String,
    /// Optional explicit Quack authentication token.
    #[pyo3(get, set)]
    pub quack_token: Option<String>,
    /// Allow Quack to bind non-local hostnames.
    #[pyo3(get, set)]
    pub quack_allow_other_hostname: bool,
    /// Install a read-only Quack authorization hook.
    #[pyo3(get, set)]
    pub quack_read_only: bool,
    /// Expose a Prometheus metrics endpoint. Requires the wheel to be built
    /// with the ``metrics`` Cargo feature. Default ``False``.
    #[pyo3(get, set)]
    pub metrics_enabled: bool,
    /// Path the metrics endpoint is served on. Must start with ``/`` and not
    /// end with ``/``. The endpoint is unauthenticated — isolate it at the
    /// network layer. Default ``"/metrics"``.
    #[pyo3(get, set)]
    pub metrics_path: String,
    /// Serve the embedded Swagger UI. Requires the wheel to be built with the
    /// ``swagger`` Cargo feature. Default ``True``.
    #[pyo3(get, set)]
    pub swagger_enabled: bool,
    /// Path the Swagger UI is served on. Default ``"/docs"``.
    #[pyo3(get, set)]
    pub swagger_path: String,
    /// OIDC issuer used by Swagger UI's Authorize button. Empty disables UI
    /// OAuth2 login. This does not enable server-side auth; pass
    /// ``AuthConfig`` to ``DataPress`` for API enforcement.
    #[pyo3(get, set)]
    pub swagger_oauth2_issuer: String,
    /// Public OAuth2 client id registered for Swagger UI.
    #[pyo3(get, set)]
    pub swagger_oauth2_client_id: String,
    /// Scopes requested by default in Swagger UI.
    #[pyo3(get, set)]
    pub swagger_oauth2_scopes: Vec<String>,
    /// Use PKCE for Swagger UI's authorization-code flow.
    #[pyo3(get, set)]
    pub swagger_oauth2_pkce: bool,
    /// Serve the embedded dataset explorer UI (Discovery + DuckDB console).
    /// Requires the wheel to be built with the ``explorer`` Cargo feature.
    /// Default ``True``.
    #[pyo3(get, set)]
    pub explorer_enabled: bool,
    /// Path the explorer UI is served on. Must start with ``/`` and not end
    /// with ``/``. Default ``"/explore"``.
    #[pyo3(get, set)]
    pub explorer_path: String,
    /// Enable the raw-SQL endpoint ``POST /api/v1/sql``. Default ``False``.
    #[pyo3(get, set)]
    pub sql_enabled: bool,
    /// Hard cap on rows returned by one raw-SQL query. Default ``100_000``.
    #[pyo3(get, set)]
    pub sql_max_rows: u64,
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
    ///     max_page_size (int): Max rows returned by one query page.
    ///         Default ``100_000``.
    ///     request_timeout_ms (int): Per-request handler timeout, in ms.
    ///         ``0`` disables. Default ``30_000``.
    ///     shutdown_timeout_secs (int): Grace period for in-flight
    ///         requests on ``SIGTERM``/``SIGINT``, in seconds.
    ///         Default ``30``.
    ///     quack_enabled (bool): Enable DuckDB's experimental Quack remote
    ///         protocol server. DuckDB backend only. Default ``False``.
    ///     quack_uri (str): Quack listen URI. Default ``"quack:localhost"``.
    ///     quack_token (str | None): Optional explicit Quack auth token.
    ///     quack_allow_other_hostname (bool): Allow non-local bind addresses.
    ///         Default ``False``.
    ///     quack_read_only (bool): Install a read-only Quack authorization
    ///         hook. Default ``True``.
    ///     metrics_enabled (bool): Expose a Prometheus metrics endpoint.
    ///         Requires a wheel built with the ``metrics`` feature.
    ///         Default ``False``.
    ///     metrics_path (str): Path the metrics endpoint is served on.
    ///         Must start with ``/`` and not end with ``/``. The endpoint
    ///         is unauthenticated. Default ``"/metrics"``.
    ///     explorer_enabled (bool): Serve the embedded dataset explorer UI.
    ///         Requires a wheel built with the ``explorer`` feature.
    ///         Default ``True``.
    ///     explorer_path (str): Path the explorer UI is served on. Must
    ///         start with ``/`` and not end with ``/``. Default
    ///         ``"/explore"``.
    ///     sql_enabled (bool): Enable the raw-SQL endpoint
    ///         ``POST /api/v1/sql``. Default ``False``.
    ///     sql_max_rows (int): Hard cap on rows returned by one raw-SQL
    ///         query. Default ``100_000``.
    #[new]
    #[pyo3(signature = (
        backend            = "duckdb".to_string(),
        listen             = "127.0.0.1".to_string(),
        port               = 8000,
        workers            = None,
        prefix             = String::new(),
        compress           = true,
        max_body_bytes     = 1_048_576,
        max_page_size      = 100_000,
        request_timeout_ms = 30_000,
        shutdown_timeout_secs = 30,
        quack_enabled     = false,
        quack_uri         = "quack:localhost".to_string(),
        quack_token       = None,
        quack_allow_other_hostname = false,
        quack_read_only   = true,
        metrics_enabled    = false,
        metrics_path       = "/metrics".to_string(),
        swagger_enabled    = true,
        swagger_path       = "/docs".to_string(),
        swagger_oauth2_issuer    = String::new(),
        swagger_oauth2_client_id = String::new(),
        swagger_oauth2_scopes    = Vec::new(),
        swagger_oauth2_pkce      = true,
        explorer_enabled   = true,
        explorer_path      = "/explore".to_string(),
        sql_enabled        = false,
        sql_max_rows       = 100_000,
    ))]
    #[allow(clippy::too_many_arguments)] // user-facing kwargs surface
    fn new(
        backend: String,
        listen: String,
        port: u16,
        workers: Option<usize>,
        prefix: String,
        compress: bool,
        max_body_bytes: usize,
        max_page_size: u64,
        request_timeout_ms: u64,
        shutdown_timeout_secs: u64,
        quack_enabled: bool,
        quack_uri: String,
        quack_token: Option<String>,
        quack_allow_other_hostname: bool,
        quack_read_only: bool,
        metrics_enabled: bool,
        metrics_path: String,
        swagger_enabled: bool,
        swagger_path: String,
        swagger_oauth2_issuer: String,
        swagger_oauth2_client_id: String,
        swagger_oauth2_scopes: Vec<String>,
        swagger_oauth2_pkce: bool,
        explorer_enabled: bool,
        explorer_path: String,
        sql_enabled: bool,
        sql_max_rows: u64,
    ) -> Self {
        Self {
            backend,
            listen,
            port,
            workers,
            prefix,
            compress,
            max_body_bytes,
            max_page_size,
            request_timeout_ms,
            shutdown_timeout_secs,
            quack_enabled,
            quack_uri,
            quack_token,
            quack_allow_other_hostname,
            quack_read_only,
            metrics_enabled,
            metrics_path,
            swagger_enabled,
            swagger_path,
            swagger_oauth2_issuer,
            swagger_oauth2_client_id,
            swagger_oauth2_scopes,
            swagger_oauth2_pkce,
            explorer_enabled,
            explorer_path,
            sql_enabled,
            sql_max_rows,
        }
    }
}

impl PyDataPressConfig {
    fn into_core(self) -> PyResult<ServerConfig> {
        let backend = match self.backend.as_str() {
            "duckdb" => Backend::Duckdb,
            "datafusion" => Backend::Datafusion,
            other => {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.backend must be 'duckdb' or 'datafusion' (got '{other}')"
                )));
            }
        };
        let listen = IpAddr::from_str(&self.listen).map_err(|e| {
            PyValueError::new_err(format!("invalid listen address '{}': {e}", self.listen))
        })?;
        if !self.prefix.is_empty() {
            if !self.prefix.starts_with('/') {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.prefix must start with '/' (got '{}')",
                    self.prefix
                )));
            }
            if self.prefix.ends_with('/') {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.prefix must not end with '/' (got '{}')",
                    self.prefix
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
            max_body_bytes: self.max_body_bytes,
            max_page_size: self.max_page_size,
            request_timeout_ms: self.request_timeout_ms,
            shutdown_timeout_secs: self.shutdown_timeout_secs,
            quack: datapress_core::config::QuackConfig {
                enabled: self.quack_enabled,
                uri: self.quack_uri,
                token: self.quack_token,
                allow_other_hostname: self.quack_allow_other_hostname,
                read_only: self.quack_read_only,
            },
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
            path: self.metrics_path.clone(),
        })
    }

    fn swagger_into_core(&self) -> PyResult<CoreSwaggerConfig> {
        if !self.swagger_path.starts_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.swagger_path must start with '/' (got '{}')",
                self.swagger_path
            )));
        }
        if self.swagger_path.len() > 1 && self.swagger_path.ends_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.swagger_path must not end with '/' (got '{}')",
                self.swagger_path
            )));
        }

        let oauth2 = if self.swagger_oauth2_issuer.trim().is_empty()
            && self.swagger_oauth2_client_id.trim().is_empty()
        {
            None
        } else {
            if self.swagger_oauth2_issuer.trim().is_empty() {
                return Err(PyValueError::new_err(
                    "DataPressConfig.swagger_oauth2_issuer is required when swagger_oauth2_client_id is set",
                ));
            }
            if self.swagger_oauth2_client_id.trim().is_empty() {
                return Err(PyValueError::new_err(
                    "DataPressConfig.swagger_oauth2_client_id is required when swagger_oauth2_issuer is set",
                ));
            }
            if !(self.swagger_oauth2_issuer.starts_with("https://")
                || self.swagger_oauth2_issuer.starts_with("http://"))
            {
                return Err(PyValueError::new_err(format!(
                    "DataPressConfig.swagger_oauth2_issuer must be an absolute http(s) URL (got '{}')",
                    self.swagger_oauth2_issuer
                )));
            }
            Some(CoreSwaggerOAuth2Config {
                issuer: self.swagger_oauth2_issuer.clone(),
                client_id: self.swagger_oauth2_client_id.clone(),
                scopes: self.swagger_oauth2_scopes.clone(),
                pkce: self.swagger_oauth2_pkce,
            })
        };

        Ok(CoreSwaggerConfig {
            enabled: self.swagger_enabled,
            path: self.swagger_path.clone(),
            oauth2,
        })
    }

    /// Build the core `ExplorerConfig`, validating the path the same way
    /// `AppConfig::validate()` does.
    fn explorer_into_core(&self) -> PyResult<CoreExplorerConfig> {
        if !self.explorer_path.starts_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.explorer_path must start with '/' (got '{}')",
                self.explorer_path
            )));
        }
        if self.explorer_path.len() > 1 && self.explorer_path.ends_with('/') {
            return Err(PyValueError::new_err(format!(
                "DataPressConfig.explorer_path must not end with '/' (got '{}')",
                self.explorer_path
            )));
        }
        Ok(CoreExplorerConfig {
            enabled: self.explorer_enabled,
            path: self.explorer_path.clone(),
        })
    }

    /// Build the core `SqlConfig` from the Python-facing fields.
    fn sql_into_core(&self) -> CoreSqlConfig {
        CoreSqlConfig {
            enabled: self.sql_enabled,
            max_rows: self.sql_max_rows,
        }
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
    #[pyo3(get, set)]
    pub enabled: bool,
    #[pyo3(get, set)]
    pub issuer: String,
    #[pyo3(get, set)]
    pub audience: String,
    #[pyo3(get, set)]
    pub read_scopes: Vec<String>,
    #[pyo3(get, set)]
    pub reload_scopes: Vec<String>,
    #[pyo3(get, set)]
    pub anonymous_read: bool,
    #[pyo3(get, set)]
    pub algorithms: Vec<String>,
    #[pyo3(get, set)]
    pub leeway_secs: u64,
    #[pyo3(get, set)]
    pub jwks_refresh_secs: u64,
    #[pyo3(get, set)]
    pub tenant_claim: String,
    #[pyo3(get, set)]
    pub allowed_tenants: Vec<String>,
    #[pyo3(get, set)]
    pub admin_token_fallback: bool,
    #[pyo3(get, set)]
    pub start_degraded: bool,
}

impl Default for PyAuthConfig {
    fn default() -> Self {
        let d = CoreAuthConfig::default();
        Self {
            enabled: d.enabled,
            issuer: d.issuer,
            audience: d.audience,
            read_scopes: d.read_scopes,
            reload_scopes: d.reload_scopes,
            anonymous_read: d.anonymous_read,
            algorithms: d.algorithms,
            leeway_secs: d.leeway_secs,
            jwks_refresh_secs: d.jwks_refresh_secs,
            tenant_claim: d.tenant_claim,
            allowed_tenants: d.allowed_tenants,
            admin_token_fallback: d.admin_token_fallback,
            start_degraded: d.start_degraded,
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
        enabled: bool,
        issuer: String,
        audience: String,
        read_scopes: Vec<String>,
        reload_scopes: Vec<String>,
        anonymous_read: bool,
        algorithms: Vec<String>,
        leeway_secs: u64,
        jwks_refresh_secs: u64,
        tenant_claim: String,
        allowed_tenants: Vec<String>,
        admin_token_fallback: bool,
        start_degraded: bool,
    ) -> Self {
        Self {
            enabled,
            issuer,
            audience,
            read_scopes,
            reload_scopes,
            anonymous_read,
            algorithms,
            leeway_secs,
            jwks_refresh_secs,
            tenant_claim,
            allowed_tenants,
            admin_token_fallback,
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
            enabled: self.enabled,
            issuer: self.issuer,
            audience: self.audience,
            read_scopes: self.read_scopes,
            reload_scopes: self.reload_scopes,
            anonymous_read: self.anonymous_read,
            algorithms: self.algorithms,
            leeway_secs: self.leeway_secs,
            jwks_refresh_secs: self.jwks_refresh_secs,
            tenant_claim: self.tenant_claim,
            allowed_tenants: self.allowed_tenants,
            admin_token_fallback: self.admin_token_fallback,
            start_degraded: self.start_degraded,
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
        py: Python<'_>,
        config: PyDataPressConfig,
        datasets: Vec<PyDatasetConfig>,
        auth: Option<PyAuthConfig>,
    ) -> PyResult<Self> {
        let metrics = config.metrics_into_core()?;
        let swagger = config.swagger_into_core()?;
        let explorer = config.explorer_into_core()?;
        let sql = config.sql_into_core();
        let server = config.into_core()?;
        let datasets = datasets
            .into_iter()
            .map(|d| d.into_core(py))
            .collect::<PyResult<Vec<_>>>()?;
        let auth = match auth {
            Some(a) => a.into_core()?,
            None => CoreAuthConfig::default(),
        };
        Ok(Self {
            cfg: AppConfig {
                server,
                docs: datapress_core::config::DocsConfig::default(),
                swagger,
                metrics,
                explorer,
                auth,
                sql,
                datasets,
            },
        })
    }

    /// Start the HTTP server and run until interrupted (Ctrl-C / SIGINT).
    ///
    /// Returns a Python awaitable that resolves when the server stops.
    /// The server runs on a dedicated OS thread with its own actix
    /// runtime, so the caller's asyncio event loop is not blocked.
    ///
    /// Shutdown is graceful and host-driven: DataPress does **not** install
    /// its own OS signal handlers (which would fight CPython's). Instead the
    /// awaitable watches for `Ctrl+C` on the host runtime; on interrupt it
    /// asks the server to stop, waits for in-flight requests to drain, and
    /// then resolves cleanly.
    ///
    /// Returns:
    ///     Awaitable[None]: Completes cleanly on graceful shutdown.
    ///
    /// Raises:
    ///     RuntimeError: If the server thread panics or bind fails.
    fn run<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cfg = clone_app_config(&self.cfg);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // `done` carries the server's exit result back from its thread;
            // `stop` asks the server to begin a graceful shutdown.
            let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<std::io::Result<()>>();
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

            std::thread::spawn(move || {
                let result = actix_web::rt::System::new().block_on(async move {
                    // Resolves when `stop_tx` is sent *or* dropped, so the
                    // server also stops if the host future is cancelled.
                    let shutdown = async move {
                        let _ = stop_rx.await;
                    };
                    match cfg.server.backend {
                        Backend::Duckdb => {
                            datapress_duckdb::serve_with_shutdown(cfg, shutdown).await
                        }
                        Backend::Datafusion => {
                            datapress_datafusion::serve_with_shutdown(cfg, shutdown).await
                        }
                    }
                });
                let _ = done_tx.send(result);
            });

            // Race the server finishing on its own (bind error / panic)
            // against a host interrupt. On Ctrl+C we trigger a graceful
            // stop and then wait for the server to finish draining, so the
            // awaitable resolves *after* a clean shutdown rather than
            // leaving a detached, unstoppable server thread behind.
            let result = tokio::select! {
                res = &mut done_rx => res,
                _ = tokio::signal::ctrl_c() => {
                    let _ = stop_tx.send(());
                    (&mut done_rx).await
                }
            };
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(PyRuntimeError::new_err(e.to_string())),
                Err(_) => Err(PyRuntimeError::new_err("DataPress server thread panicked")),
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
            listen: cfg.server.listen,
            port: cfg.server.port,
            workers: cfg.server.workers,
            prefix: cfg.server.prefix.clone(),
            compress: cfg.server.compress,
            max_body_bytes: cfg.server.max_body_bytes,
            max_page_size: cfg.server.max_page_size,
            request_timeout_ms: cfg.server.request_timeout_ms,
            shutdown_timeout_secs: cfg.server.shutdown_timeout_secs,
            quack: cfg.server.quack.clone(),
        },
        docs: cfg.docs.clone(),
        swagger: cfg.swagger.clone(),
        metrics: cfg.metrics.clone(),
        explorer: cfg.explorer.clone(),
        auth: cfg.auth.clone(),
        sql: cfg.sql.clone(),
        datasets: cfg.datasets.clone(),
    }
}

// ---------------------------------------------------------------------------
// Module entry point
// ---------------------------------------------------------------------------

#[pymodule]
fn datapress(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Best-effort init of env_logger so RUST_LOG=info works from Python.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    m.add_class::<PyHMACKeyPair>()?;
    m.add_class::<PyS3Config>()?;
    m.add_class::<PyDatasetConfig>()?;
    m.add_class::<PyDataPressConfig>()?;
    m.add_class::<PyAuthConfig>()?;
    m.add_class::<PyDataPress>()?;
    Ok(())
}
