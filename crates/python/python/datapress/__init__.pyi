"""Type stubs for the `datapress` extension module."""

from collections.abc import Awaitable
from typing import Optional


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

    def __init__(
        self,
        region: Optional[str] = None,
        endpoint: Optional[str] = None,
        addressing_style: str = "virtual",
        allow_http: bool = False,
        access_key_id: Optional[str] = None,
        secret_access_key: Optional[str] = None,
        session_token: Optional[str] = None,
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
            index_columns: Columns to build an index over when ``mode="list"``.
            index_max_cardinality: Upper bound on distinct values per
                indexed column.
            lazy: When ``True`` the dataset is **not** materialised into
                RAM at startup. Queries stream from disk via DataFusion's
                ``ListingTable``, with column-projection and predicate
                pushdown. Essential for wide (hundreds of columns) or
                multi-file parquet datasets. DataFusion backend, local
                parquet only. Defaults to ``False``.
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

    def __init__(
        self,
        backend: str = "duckdb",
        listen: str = "127.0.0.1",
        port: int = 8000,
        workers: Optional[int] = None,
        prefix: str = "",
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
    ) -> None:
        """Build a :class:`DataPress` instance.

        Args:
            config: Server-side configuration.
            datasets: Datasets to publish. Must be non-empty.

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


__all__ = ["DataPress", "DataPressConfig", "DatasetConfig", "S3Config"]
