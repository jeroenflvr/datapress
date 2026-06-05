"""Type stubs for the `datap_rs.datapress` extension module."""

from collections.abc import Awaitable, Callable
from typing import Optional


class HMACKeyPair:
    """A resolved HMAC access-key / secret-key pair.

    Returned by a :attr:`S3Config.credentials_provider` callable to supply
    object-store credentials at construction time.
    """

    access_key: str
    secret_key: str

    def __init__(self, access_key: str, secret_key: str) -> None:
        """Build an :class:`HMACKeyPair`.

        Args:
            access_key: The access-key id.
            secret_key: The secret access key. Redacted in ``repr``.
        """
        ...


class S3Config:
    """S3 / S3-compatible object-store credentials and endpoint config.

    Attached to a :class:`DatasetConfig` whose ``source`` is an ``s3://`` URI.
    All fields are optional — anything left unset falls back to the standard
    AWS environment variables (``AWS_REGION``, ``AWS_ACCESS_KEY_ID``, …).
    """

    region: Optional[str]
    endpoint: Optional[str]
    addressing_style: str
    allow_http: bool
    access_key_id: Optional[str]
    secret_access_key: Optional[str]
    session_token: Optional[str]
    partitioning: str
    endpoint_bucket_in_host: str
    credentials_provider: Optional[Callable[[], HMACKeyPair]]

    def __init__(
        self,
        region: Optional[str] = None,
        endpoint: Optional[str] = None,
        addressing_style: str = "virtual",
        allow_http: bool = False,
        access_key_id: Optional[str] = None,
        secret_access_key: Optional[str] = None,
        session_token: Optional[str] = None,
        partitioning: str = "auto",
        endpoint_bucket_in_host: str = "auto",
        credentials_provider: Optional[Callable[[], HMACKeyPair]] = None,
    ) -> None:
        """Build an :class:`S3Config`.

        Args:
            region: AWS region, e.g. ``"us-east-1"``.
            endpoint: Custom S3-compatible endpoint URL.
            addressing_style: ``"virtual"`` (default) or ``"path"``.
            allow_http: Allow plain-HTTP endpoints. Defaults to ``False``.
            access_key_id: Static access-key override.
            secret_access_key: Static secret-key override.
            session_token: Temporary STS session token.
            partitioning: Hive partition discovery: ``"auto"`` (default),
                ``"hive"``, or ``"none"``.
            endpoint_bucket_in_host: Fold the bucket into the endpoint host:
                ``"auto"`` (default, follows ``addressing_style``), ``"true"``,
                or ``"false"``.
            credentials_provider: Optional zero-argument callable returning an
                :class:`HMACKeyPair`. When supplied it takes precedence over
                ``access_key_id`` / ``secret_access_key`` (the static HMAC
                credentials are ignored). The callable is invoked once when the
                owning :class:`DataPress` is constructed and the resolved keys
                are cached indefinitely.
        """
        ...


class DatasetConfig:
    """Declarative description of a single queryable dataset.

    A :class:`DataPress` instance is constructed from a list of these.
    The ``name`` becomes the URL slug (``/api/datasets/<name>/…``).
    """

    name: str
    source: str
    format: str
    mode: str
    description: Optional[str]
    s3: Optional[S3Config]
    columns: Optional[list[str]]
    dict_encode: bool
    index_columns: Optional[list[str]]
    index_max_cardinality: Optional[int]
    lazy: bool

    def __init__(
        self,
        name: str,
        source: str,
        format: str = "parquet",
        mode: str = "auto",
        description: Optional[str] = None,
        s3: Optional[S3Config] = None,
        columns: Optional[list[str]] = None,
        dict_encode: bool = True,
        index_columns: Optional[list[str]] = None,
        index_max_cardinality: Optional[int] = None,
        lazy: bool = False,
    ) -> None:
        """Build a :class:`DatasetConfig`.

        Args:
            name: URL-safe identifier; matches ``[A-Za-z0-9_.\\-]+``.
            source: Local path, glob pattern (``data/*.parquet``,
                ``data/year=*/*.parquet``) or ``s3://bucket/prefix`` URI.
            format: ``"parquet"`` (default) or ``"delta"``.
            mode: Index mode — ``"auto"`` (default), ``"none"`` or ``"list"``.
            description: Free-text shown in the listing endpoint.
            s3: Required when ``source`` starts with ``s3://``.
            columns: Read only these columns from the source. ``None``
                (default) = read all columns.
            dict_encode: Keep dictionary-encoded Utf8 columns as Arrow
                ``Dictionary(Int32, Utf8)``. Defaults to ``True``.
            index_columns: Columns to build an index over when ``mode="list"``.
            index_max_cardinality: Upper bound on distinct values per
                indexed column.
            lazy: Stream from disk instead of loading into RAM.
                DataFusion backend / local parquet only. Defaults to ``False``.
        """
        ...


class DataPressConfig:
    """Server-side configuration for a :class:`DataPress` instance.

    Selects the query engine and controls how the HTTP server binds.
    """

    backend: str
    listen: str
    port: int
    workers: Optional[int]
    prefix: str
    compress: bool
    max_body_bytes: int
    max_page_size: int
    request_timeout_ms: int
    shutdown_timeout_secs: int
    quack_enabled: bool
    quack_uri: str
    quack_token: Optional[str]
    quack_allow_other_hostname: bool
    quack_read_only: bool
    metrics_enabled: bool
    metrics_path: str
    swagger_enabled: bool
    swagger_path: str
    swagger_oauth2_issuer: str
    swagger_oauth2_client_id: str
    swagger_oauth2_scopes: list[str]
    swagger_oauth2_pkce: bool
    explorer_enabled: bool
    explorer_path: str
    admin_token: Optional[str]
    sql_enabled: bool
    sql_max_rows: int

    def __init__(
        self,
        backend: str = "duckdb",
        listen: str = "127.0.0.1",
        port: int = 8000,
        workers: Optional[int] = None,
        prefix: str = "",
        compress: bool = True,
        max_body_bytes: int = 1_048_576,
        max_page_size: int = 100_000,
        request_timeout_ms: int = 30_000,
        shutdown_timeout_secs: int = 30,
        quack_enabled: bool = False,
        quack_uri: str = "quack:localhost",
        quack_token: Optional[str] = None,
        quack_allow_other_hostname: bool = False,
        quack_read_only: bool = True,
        metrics_enabled: bool = False,
        metrics_path: str = "/metrics",
        swagger_enabled: bool = True,
        swagger_path: str = "/docs",
        swagger_oauth2_issuer: str = "",
        swagger_oauth2_client_id: str = "",
        swagger_oauth2_scopes: Optional[list[str]] = None,
        swagger_oauth2_pkce: bool = True,
        explorer_enabled: bool = True,
        explorer_path: str = "/explore",
        admin_token: Optional[str] = None,
        sql_enabled: bool = False,
        sql_max_rows: int = 100_000,
    ) -> None:
        """Build a :class:`DataPressConfig`.

        Args:
            backend: ``"duckdb"`` (default) or ``"datafusion"``. Both are
                compiled into the wheel; selection is purely runtime.
            listen: Bind address. Default ``"127.0.0.1"`` — use ``"0.0.0.0"``
                to expose the port outside localhost.
            port: TCP port. Default ``8000``.
            workers: Number of actix worker threads. ``None`` (default)
                means one per CPU.
            prefix: URL path prefix mounted in front of every route, e.g.
                ``"/datapress"`` when running behind a reverse proxy that
                passes the path through unchanged. Must start with ``/``
                and not end with ``/``. Empty string (default) = root.
            compress: Enable response compression negotiated via
                ``Accept-Encoding`` (gzip / brotli / zstd). Default ``True``.
            max_body_bytes: Maximum accepted JSON request body, in bytes.
                Larger bodies are rejected with ``413``. Default ``1_048_576``.
            max_page_size: Maximum rows returned by one query page. Larger
                ``page_size`` values are clamped. Default ``100_000``.
            request_timeout_ms: Per-request handler timeout, in ms.
                ``0`` disables the timeout. Default ``30_000``.
            shutdown_timeout_secs: Grace period for in-flight requests after
                the server receives ``SIGTERM``/``SIGINT``, in seconds.
                Default ``30``.
            quack_enabled: Enable DuckDB's experimental Quack remote protocol
                server. DuckDB backend only. Default ``False``.
            quack_uri: Quack listen URI. Default ``"quack:localhost"``.
            quack_token: Optional explicit Quack auth token. If unset, Quack
                generates one and DataPress logs it at startup.
            quack_allow_other_hostname: Allow non-local bind addresses. Use
                only behind a TLS-terminating reverse proxy. Default ``False``.
            quack_read_only: Install a read-only Quack authorization hook.
                Default ``True``.
            metrics_enabled: Expose a Prometheus metrics endpoint. Requires
                the wheel to be built with the ``metrics`` Cargo feature.
                Default ``False``.
            metrics_path: Path the metrics endpoint is served on. Must start
                with ``/`` and not end with ``/``. The endpoint is
                unauthenticated — isolate it at the network layer. Default
                ``"/metrics"``.
            swagger_enabled: Serve the embedded Swagger UI at
                ``swagger_path``. Requires a wheel built with the ``swagger``
                feature. Default ``True``.
            swagger_path: Path the Swagger UI is served on. Default
                ``"/docs"``.
            swagger_oauth2_issuer: OIDC issuer used by Swagger UI's
                Authorize button. Empty disables UI OAuth2 login.
            swagger_oauth2_client_id: Public OAuth2 client id registered for
                Swagger UI.
            swagger_oauth2_scopes: Scopes requested by default in Swagger UI.
            swagger_oauth2_pkce: Use PKCE for the authorization-code flow.
                Default ``True``.
            explorer_enabled: Serve the embedded dataset explorer UI
                (Discovery + DuckDB console) at ``explorer_path``. Requires a
                wheel built with the ``explorer`` feature. Default ``True``.
            explorer_path: Path the explorer UI is served on. Must start with
                ``/`` and not end with ``/``. Default ``"/explore"``.
            admin_token: Admin token accepted by ``POST …/reload`` via the
                ``X-Admin-Token`` header. Equivalent to setting the
                ``ADMIN_TOKEN`` environment variable — use whichever is more
                convenient. When both are provided, the value passed here wins
                (it is applied before the env var is read). ``None`` (default)
                keeps admin endpoints disabled unless ``ADMIN_TOKEN`` is set.
            sql_enabled: Enable the raw-SQL endpoint ``POST /api/v1/sql``.
                Disabled by default. Default ``False``.
            sql_max_rows: Hard cap on rows returned by one raw-SQL query.
                Default ``100_000``.
        """
        ...


class AuthConfig:
    """OIDC / OAuth2 bearer-token enforcement for the HTTP API.

    Pass an instance to :class:`DataPress` as the ``auth`` kwarg. Requires
    the wheel to be built with the ``auth`` Cargo feature (the published
    wheels include it). When ``enabled=False`` (default) the entire auth
    layer is a no-op and existing ``X-Admin-Token`` semantics apply.
    """

    enabled: bool
    issuer: str
    audience: str
    read_scopes: list[str]
    reload_scopes: list[str]
    anonymous_read: bool
    algorithms: list[str]
    leeway_secs: int
    jwks_refresh_secs: int
    tenant_claim: str
    allowed_tenants: list[str]
    admin_token_fallback: bool
    start_degraded: bool

    def __init__(
        self,
        enabled: bool = False,
        issuer: str = "",
        audience: str = "",
        read_scopes: Optional[list[str]] = None,
        reload_scopes: Optional[list[str]] = None,
        anonymous_read: bool = False,
        algorithms: Optional[list[str]] = None,
        leeway_secs: int = 60,
        jwks_refresh_secs: int = 3600,
        tenant_claim: str = "",
        allowed_tenants: Optional[list[str]] = None,
        admin_token_fallback: bool = True,
        start_degraded: bool = True,
    ) -> None:
        """Build an :class:`AuthConfig`.

        Args:
            enabled: Master switch. Default ``False``.
            issuer: OIDC issuer URL — must equal the JWT ``iss`` claim.
                Required when ``enabled=True``. Must be ``https://...`` (or
                ``http://localhost...`` for local development).
            audience: Expected JWT ``aud`` claim. Empty disables ``aud``
                validation (not recommended in production).
            read_scopes: Scopes required on every read endpoint. Empty
                (default) = any valid token is enough.
            reload_scopes: Scopes required on ``POST .../reload``.
            anonymous_read: Allow unauthenticated reads. Default ``False``.
            algorithms: Allowed JWS algorithms. Default ``["RS256"]``.
                Only RS/ES/PS variants are accepted.
            leeway_secs: Clock-skew tolerance for ``exp``/``nbf``. Default ``60``.
            jwks_refresh_secs: Background JWKS refresh interval. Default
                ``3600`` (clamped to ≥ 60).
            tenant_claim: JSON-pointer into the JWT claims to extract a
                tenant id (e.g. ``"/tid"`` for Entra ID). Empty disables.
            allowed_tenants: If non-empty, the token's tenant value must be
                in this list. Has no effect without ``tenant_claim``.
            admin_token_fallback: Keep ``X-Admin-Token`` working in parallel
                with OIDC for ``POST .../reload``. Default ``True``.
            start_degraded: If ``True`` (default) the server starts even when
                the IdP is unreachable and serves 503 for authenticated
                requests until JWKS becomes available. If ``False``, an
                unreachable IdP at boot fails startup.
        """
        ...


class DataPress:
    """A configured DataPress HTTP server, ready to :meth:`run`.

    Construct with a :class:`DataPressConfig` and a list of
    :class:`DatasetConfig`. The server is not started until :meth:`run`
    is awaited.

    Example:
        >>> import asyncio
        >>> from datapress import DataPress, DataPressConfig, DatasetConfig
        >>> dp = DataPress(
        ...     DataPressConfig(backend="datafusion", port=8000),
        ...     datasets=[DatasetConfig(name="accidents", source="data/x.parquet")],
        ... )
        >>> asyncio.run(dp.run())
    """

    def __init__(
        self,
        config: DataPressConfig,
        datasets: list[DatasetConfig],
        auth: Optional[AuthConfig] = None,
    ) -> None:
        """Build a :class:`DataPress` instance.

        Args:
            config: Server-side configuration.
            datasets: Datasets to publish. Must be non-empty.
            auth: Optional OIDC/OAuth2 enforcement. Defaults to disabled.

        Raises:
            ValueError: If any field is invalid (bad backend name, bad
                prefix, duplicate dataset name, …).
        """
        ...

    def run(self) -> Awaitable[None]:
        """Start the HTTP server and run until SIGINT (Ctrl-C).

        Returns a Python awaitable that resolves when the server stops.
        The server runs on a dedicated OS thread with its own actix
        runtime, so the caller's asyncio event loop is not blocked.

        Returns:
            An awaitable that completes cleanly on graceful shutdown.

        Raises:
            RuntimeError: If the server thread panics or bind fails.
        """
        ...


__all__ = ["AuthConfig", "DataPress", "DataPressConfig", "DatasetConfig", "HMACKeyPair", "S3Config"]
