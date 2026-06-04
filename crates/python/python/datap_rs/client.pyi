"""Type stubs for :mod:`datap_rs.client`."""

from typing import Any, Mapping, Optional


class DataPressHTTPError(RuntimeError):
    status:  int
    body:    str
    payload: Optional[dict]
    def __init__(self, status: int, body: str, payload: Optional[dict]) -> None: ...


class DataPressClient:
    base_url:           str
    admin_token:        Optional[str]
    timeout:            float
    accept_compression: bool

    def __init__(
        self,
        base_url:           str,
        admin_token:        Optional[str] = ...,
        timeout:            float         = ...,
        accept_compression: bool          = ...,
    ) -> None: ...

    def healthz(self) -> dict: ...
    def readyz(self) -> dict: ...
    def datasets(self) -> list[str]: ...
    def schema(self, dataset: str) -> dict: ...
    def count(
        self,
        dataset:    str,
        predicates: Optional[list[dict]] = ...,
    ) -> int: ...
    def query_json(self, dataset: str, request: Mapping[str, Any]) -> dict: ...
    def query(self, dataset: str, request: Mapping[str, Any]) -> Any: ...
    def sql(self, sql: str, max_rows: Optional[int] = ...) -> list[dict]: ...
    def reload(self, dataset: str) -> dict: ...
