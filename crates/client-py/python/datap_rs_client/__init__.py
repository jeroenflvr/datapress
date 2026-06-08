"""Python client for a DataPress dataset server.

A thin, ergonomic wrapper over the native Rust client
(:mod:`datap_rs_client._datapress_client`). Requests are passed as Python
dicts and responses come back as dicts; structured queries can optionally
be decoded into a :class:`pyarrow.Table` when ``pyarrow`` is installed
(``pip install datap-rs-client[arrow]``).

Example
-------
>>> from datap_rs_client import DataPressClient
>>> client = DataPressClient("http://127.0.0.1:8000")
>>> client.datasets()
['accidents']
>>> client.count("accidents", predicates=[{"col": "Severity", "op": "gte", "val": 3}])
123456
>>> rows = client.query("accidents", columns=["State", "Severity"], page_size=1000)
>>> rows["page"], len(rows["data"])
(1, 1000)
"""

from __future__ import annotations

import json
from typing import Any, Mapping, Sequence

from ._datapress_client import Client as _Client
from ._datapress_client import DataPressError

__all__ = ["DataPressClient", "DataPressError"]


class DataPressClient:
    """Synchronous client for a running DataPress server.

    Parameters
    ----------
    base_url:
        Server base URL, e.g. ``http://127.0.0.1:8000``. Include any
        configured server prefix (e.g. ``/datapress``) here.
    api_base:
        Versioned API mount path. Defaults to ``/api/v1`` on the Rust
        side; pass ``/api`` to target the legacy unversioned alias.
    admin_token:
        Token sent as ``X-Admin-Token`` on mutating endpoints
        (:meth:`reload`).
    bearer_token:
        OAuth2 bearer token, attached as ``Authorization: Bearer …`` to
        every request (for servers with ``auth`` enabled).
    timeout:
        Per-request timeout in seconds.
    """

    def __init__(
        self,
        base_url: str,
        *,
        api_base: str | None = None,
        admin_token: str | None = None,
        bearer_token: str | None = None,
        timeout: float | None = None,
    ) -> None:
        self._client = _Client(
            base_url,
            api_base=api_base,
            admin_token=admin_token,
            bearer_token=bearer_token,
            timeout_secs=timeout,
        )

    # -- probes ---------------------------------------------------------

    def healthz(self) -> Any:
        """Liveness probe (``GET /healthz``)."""
        return json.loads(self._client.healthz())

    def readyz(self) -> Any:
        """Readiness probe (``GET /readyz``). Raises while still loading."""
        return json.loads(self._client.readyz())

    # -- metadata -------------------------------------------------------

    def datasets(self) -> list[str]:
        """List registered dataset names."""
        return self._client.datasets()

    def schema(self, dataset: str) -> Any:
        """Fetch the schema description for ``dataset``."""
        return json.loads(self._client.schema(dataset))

    def count(
        self,
        dataset: str,
        predicates: Sequence[Mapping[str, Any]] | None = None,
    ) -> int:
        """Count matching rows. ``predicates`` is a list of predicate dicts."""
        payload = json.dumps(list(predicates)) if predicates else None
        return self._client.count(dataset, payload)

    # -- queries --------------------------------------------------------

    def query(
        self,
        dataset: str,
        *,
        columns: Sequence[str] | None = None,
        predicates: Sequence[Mapping[str, Any]] | None = None,
        group_by: Sequence[str] | None = None,
        aggregations: Sequence[Mapping[str, Any]] | None = None,
        having: Sequence[Mapping[str, Any]] | None = None,
        distinct: bool | None = None,
        order_by: Sequence[Mapping[str, Any]] | None = None,
        limit: int | None = None,
        page: int | None = None,
        page_size: int | None = None,
        request: Mapping[str, Any] | None = None,
    ) -> Any:
        """Run a structured query, returning the JSON response envelope.

        Pass a full ``request`` dict, or build one from keyword arguments
        (mutually exclusive with ``request``).
        """
        body = dict(request) if request is not None else self._build_request(
            columns=columns,
            predicates=predicates,
            group_by=group_by,
            aggregations=aggregations,
            having=having,
            distinct=distinct,
            order_by=order_by,
            limit=limit,
            page=page,
            page_size=page_size,
        )
        return json.loads(self._client.query_json(dataset, json.dumps(body)))

    def query_arrow(
        self,
        dataset: str,
        *,
        request: Mapping[str, Any] | None = None,
        **kwargs: Any,
    ) -> "pyarrow.Table":  # type: ignore[name-defined]  # noqa: F821
        """Run a structured query and decode the Arrow IPC response.

        Requires ``pyarrow`` (``pip install datap-rs-client[arrow]``).
        Accepts the same keyword arguments as :meth:`query`.
        """
        try:
            import pyarrow as pa
        except ImportError as exc:  # pragma: no cover - import guard
            raise RuntimeError(
                "query_arrow requires pyarrow; install datap-rs-client[arrow]"
            ) from exc
        body = dict(request) if request is not None else self._build_request(**kwargs)
        raw = self._client.query_arrow(dataset, json.dumps(body))
        with pa.ipc.open_stream(raw) as reader:
            return reader.read_all()

    def sql(self, sql: str, *, max_rows: int | None = None) -> Any:
        """Run a read-only SQL statement (``POST /sql``)."""
        return json.loads(self._client.sql(sql, max_rows))

    # -- admin ----------------------------------------------------------

    def reload(self, dataset: str) -> Any:
        """Trigger an in-place reload of ``dataset``."""
        return json.loads(self._client.reload(dataset))

    # -- helpers --------------------------------------------------------

    @staticmethod
    def _build_request(
        *,
        columns: Sequence[str] | None = None,
        predicates: Sequence[Mapping[str, Any]] | None = None,
        group_by: Sequence[str] | None = None,
        aggregations: Sequence[Mapping[str, Any]] | None = None,
        having: Sequence[Mapping[str, Any]] | None = None,
        distinct: bool | None = None,
        order_by: Sequence[Mapping[str, Any]] | None = None,
        limit: int | None = None,
        page: int | None = None,
        page_size: int | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {}
        if columns is not None:
            body["columns"] = list(columns)
        if predicates is not None:
            body["predicates"] = list(predicates)
        if group_by is not None:
            body["group_by"] = list(group_by)
        if aggregations is not None:
            body["aggregations"] = list(aggregations)
        if having is not None:
            body["having"] = list(having)
        if distinct is not None:
            body["distinct"] = distinct
        if order_by is not None:
            body["order_by"] = list(order_by)
        if limit is not None:
            body["limit"] = limit
        if page is not None:
            body["page"] = page
        if page_size is not None:
            body["page_size"] = page_size
        return body
