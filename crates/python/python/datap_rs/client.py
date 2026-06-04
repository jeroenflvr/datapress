"""Typed Python client for a running :class:`DataPress` server.

A small, dependency-light HTTP client that wraps the JSON / Arrow IPC
endpoints exposed by DataPress. Use it from notebooks, scripts, and
integration tests when you'd rather not hand-roll ``urllib`` and
schema decoding yourself.

The HTTP transport uses only the standard library
(:mod:`urllib.request`). :mod:`pyarrow` is imported lazily on the first
call to :meth:`DataPressClient.query` and is **only** required when
asking for Arrow IPC responses; the rest of the API works without it.

Example::

    from datap_rs import DataPressClient

    client = DataPressClient("http://127.0.0.1:8000")
    print(client.datasets())                       # -> ["accidents", ...]
    schema = client.schema("accidents")            # -> dict
    n      = client.count("accidents")             # -> int
    table  = client.query("accidents", {           # -> pyarrow.Table
        "columns":   ["State", "Severity"],
        "page_size": 10_000,
    })
"""

from __future__ import annotations

import gzip
import json
import urllib.error
import urllib.request
from typing import Any, Mapping, Optional


__all__ = ["DataPressClient", "DataPressHTTPError"]


class DataPressHTTPError(RuntimeError):
    """Raised when the server returns a non-2xx response.

    Attributes:
        status:  HTTP status code (e.g. ``404``, ``413``, ``504``).
        body:    Raw response body, decoded as UTF-8 best-effort.
        payload: Parsed JSON body if the response was ``application/json``,
                 otherwise ``None``.
    """

    def __init__(self, status: int, body: str, payload: Optional[dict]) -> None:
        msg = f"HTTP {status}: {body[:200]}"
        super().__init__(msg)
        self.status  = status
        self.body    = body
        self.payload = payload


class DataPressClient:
    """Sync HTTP client for the DataPress server.

    The client is stateless and thread-safe — every method opens its
    own short-lived connection. There is no connection pool; for
    high-concurrency workloads create one client per thread or wrap
    the lower-level methods in your own pool.

    Args:
        base_url: Server base URL, e.g. ``"http://127.0.0.1:8000"``.
            If the server is mounted under a prefix
            (``ServerConfig.prefix = "/datapress"``), include it here:
            ``"http://host:8000/datapress"``. Trailing slashes are stripped.
        admin_token: Optional admin token sent as ``X-Admin-Token`` on
            mutating endpoints (currently only :meth:`reload`).
        timeout: Socket timeout in seconds for every request. Default 60.
        accept_compression: Send ``Accept-Encoding: gzip``. Default ``True``.
    """

    def __init__(
        self,
        base_url:           str,
        admin_token:        Optional[str] = None,
        timeout:            float         = 60.0,
        accept_compression: bool          = True,
    ) -> None:
        self.base_url           = base_url.rstrip("/")
        self.admin_token        = admin_token
        self.timeout            = timeout
        self.accept_compression = accept_compression

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def healthz(self) -> dict:
        """Liveness probe — hits ``GET /healthz`` (always at root).

        Returns the parsed JSON body, e.g. ``{"status": "ok"}``.
        """
        return self._request_json("GET", self._root_url("/healthz"))

    def readyz(self) -> dict:
        """Readiness probe — hits ``GET /readyz`` (always at root).

        Returns the parsed JSON body. Raises :class:`DataPressHTTPError`
        with ``status == 503`` while the server is still loading.
        """
        return self._request_json("GET", self._root_url("/readyz"))

    def datasets(self) -> list[str]:
        """List dataset names registered by the server."""
        body = self._request_json("GET", self._url("/api/datasets"))
        # The list endpoint returns either ["a","b"] or [{"name":"a",...}]
        # depending on backend; normalise to a list[str].
        if body and isinstance(body[0], dict):
            return [row["name"] for row in body]
        return list(body)

    def schema(self, dataset: str) -> dict:
        """Fetch the Arrow schema description for ``dataset``."""
        return self._request_json("GET", self._url(f"/api/datasets/{dataset}/schema"))

    def count(
        self,
        dataset:    str,
        predicates: Optional[list[dict]] = None,
    ) -> int:
        """Count matching rows.

        Args:
            dataset:    Dataset name.
            predicates: Optional list of predicate dicts in the same
                shape accepted by :meth:`query`. ``None`` = unfiltered.

        Returns:
            Row count as an ``int``.
        """
        body: dict[str, Any] = {}
        if predicates:
            body["predicates"] = predicates
        out = self._request_json(
            "POST",
            self._url(f"/api/datasets/{dataset}/count"),
            json_body=body,
        )
        return int(out["count"])

    def query_json(self, dataset: str, request: Mapping[str, Any]) -> dict:
        """Run a query and return the JSON envelope verbatim.

        Args:
            dataset: Dataset name.
            request: Query body — see the QUERY.md reference.

        Returns:
            The decoded JSON envelope: ``{"rows": [...], "next_cursor": ...}``.
        """
        return self._request_json(
            "POST",
            self._url(f"/api/datasets/{dataset}/query"),
            json_body=dict(request),
        )

    def query(self, dataset: str, request: Mapping[str, Any]) -> "Any":
        """Run a query and return the result as a :class:`pyarrow.Table`.

        Sends ``Accept: application/vnd.apache.arrow.stream`` and decodes
        the Arrow IPC stream response. Backends that don't support Arrow
        IPC silently fall back to JSON; this method detects that and
        re-encodes the JSON rows into a Table (slower path, kept so
        calling code doesn't need to branch on backend).

        Args:
            dataset: Dataset name.
            request: Query body — see the QUERY.md reference.

        Returns:
            A ``pyarrow.Table``.

        Raises:
            ImportError: If ``pyarrow`` is not installed.
            DataPressHTTPError: On a non-2xx response.
        """
        try:
            import pyarrow as pa  # noqa: F401  (validated availability)
            import pyarrow.ipc as ipc
        except ImportError as e:
            raise ImportError(
                "pyarrow is required for DataPressClient.query(); "
                "install it with `pip install pyarrow` or use query_json()."
            ) from e

        status, headers, body = self._request(
            "POST",
            self._url(f"/api/datasets/{dataset}/query"),
            json_body=dict(request),
            extra_headers={"Accept": "application/vnd.apache.arrow.stream"},
        )
        ctype = headers.get("Content-Type", "").lower()
        if "arrow" in ctype:
            reader = ipc.RecordBatchStreamReader(pa.BufferReader(body))
            return reader.read_all()
        # JSON fallback — server didn't support Arrow IPC for this dataset.
        envelope = json.loads(body.decode("utf-8"))
        return pa.Table.from_pylist(envelope.get("rows", []))

    def sql(
        self,
        sql:      str,
        max_rows: Optional[int] = None,
    ) -> list[dict]:
        """Run a raw read-only SQL statement via ``POST /api/v1/sql``.

        The endpoint must be enabled server-side (``[sql].enabled = true``);
        otherwise the server responds ``404`` and this raises
        :class:`DataPressHTTPError`. The statement must be a single
        read-only ``SELECT`` referencing a single registered dataset.

        Args:
            sql:      The SQL statement to execute.
            max_rows: Optional client row cap. Clamped server-side into
                ``[1, [sql].max_rows]``; it can never raise the server cap.
                ``None`` uses the configured server cap.

        Returns:
            The result set as a list of row dicts (the ``data`` array of
            the response envelope).

        Raises:
            DataPressHTTPError: On a non-2xx response — e.g. ``404`` when
                the endpoint is disabled, or ``400`` when the statement is
                rejected by the validation gate.
        """
        body: dict[str, Any] = {"sql": sql}
        if max_rows is not None:
            body["max_rows"] = max_rows
        out = self._request_json("POST", self._url("/api/sql"), json_body=body)
        return out["data"]

    def reload(self, dataset: str) -> dict:
        """Trigger an in-place reload of ``dataset``.

        Requires ``admin_token`` to have been set on the client.
        """
        return self._request_json(
            "POST",
            self._url(f"/api/datasets/{dataset}/reload"),
        )

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _root_url(self, path: str) -> str:
        # /healthz and /readyz are mounted at the bare host root, outside
        # any configured prefix. Strip whatever prefix the user gave us.
        from urllib.parse import urlsplit, urlunsplit
        parts = urlsplit(self.base_url)
        return urlunsplit((parts.scheme, parts.netloc, path, "", ""))

    def _url(self, path: str) -> str:
        return f"{self.base_url}{path}"

    def _request_json(
        self,
        method:    str,
        url:       str,
        json_body: Optional[dict] = None,
    ) -> Any:
        _, _, body = self._request(method, url, json_body=json_body)
        if not body:
            return None
        return json.loads(body.decode("utf-8"))

    def _request(
        self,
        method:        str,
        url:           str,
        json_body:     Optional[dict]      = None,
        extra_headers: Optional[dict]      = None,
    ) -> tuple[int, dict, bytes]:
        data: Optional[bytes] = None
        headers: dict[str, str] = {"Accept": "application/json"}
        if json_body is not None:
            data = json.dumps(json_body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self.accept_compression:
            headers["Accept-Encoding"] = "gzip"
        if self.admin_token:
            headers["X-Admin-Token"] = self.admin_token
        if extra_headers:
            headers.update(extra_headers)

        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                status = resp.status
                resp_headers = {k: v for k, v in resp.headers.items()}
                raw = resp.read()
        except urllib.error.HTTPError as e:
            raw = e.read() or b""
            resp_headers = {k: v for k, v in (e.headers or {}).items()}
            status = e.code
            text = self._maybe_decompress(raw, resp_headers).decode("utf-8", "replace")
            payload: Optional[dict] = None
            if "application/json" in resp_headers.get("Content-Type", "").lower():
                try:
                    payload = json.loads(text)
                except json.JSONDecodeError:
                    payload = None
            raise DataPressHTTPError(status, text, payload) from None

        return status, resp_headers, self._maybe_decompress(raw, resp_headers)

    @staticmethod
    def _maybe_decompress(body: bytes, headers: Mapping[str, str]) -> bytes:
        enc = headers.get("Content-Encoding", "").lower()
        if enc == "gzip":
            return gzip.decompress(body)
        # brotli / zstd: urlopen doesn't negotiate them since we only ask
        # for gzip, so we shouldn't see them here. Pass through untouched
        # if a future change widens Accept-Encoding.
        return body
