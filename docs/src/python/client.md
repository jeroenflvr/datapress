# Client

`DataPressClient` is a small sync client for talking to a running
DataPress server. It uses only the Python stdlib plus a lazy
`pyarrow` import (only loaded when you call `query()` for Arrow IPC).

!!! tip "Standalone alternative"
    This client ships inside the embedded `datap-rs` wheel. If you only need
    to talk to a server (not run one), the standalone
    [`datap-rs-client`](../clients/python.md) package is lighter and uses a
    native Rust transport.

```python
from datap_rs import DataPressClient

c = DataPressClient("http://127.0.0.1:8000")

c.healthz()                                  # -> {"status": "ok"}
c.readyz()                                   # -> {"status": "ready", "datasets": N}
c.datasets()                                 # -> ["accidents", ...]
c.schema("accidents")                        # -> dict
c.count("accidents")                         # -> int
```

## Querying

`query()` requests Arrow IPC and returns a `pyarrow.Table`:

```python
table = c.query("accidents", {
    "columns":   ["State", "Severity"],
    "predicates": [{ "col": "State", "op": "eq", "val": "TX" }],
    "page_size": 10_000,
})
```

For the JSON envelope verbatim, use `query_json()`:

```python
payload = c.query_json("accidents", { "page_size": 50 })
# -> { "data": [...], "page": 1, "page_size": 50 }
```

## Raw SQL

`sql()` posts a single read-only `SELECT` to `POST /api/v1/sql` and
returns the result as a list of row dicts. The endpoint must be enabled
server-side (`[sql].enabled = true`, or `sql_enabled=True` on
[`DataPressConfig`](config.md)); otherwise the server responds `404` and
the call raises `DataPressHTTPError`.

```python
rows = c.sql(
    "SELECT State, COUNT(*) AS n FROM accidents GROUP BY State ORDER BY n DESC",
    max_rows=10,
)
# -> [{"State": "CA", "n": 1234}, {"State": "TX", "n": 987}, ...]
```

Phase 1 allows **one** registered dataset per statement (no cross-dataset
joins yet). `max_rows` is clamped server-side into `[1, [sql].max_rows]`;
it can never raise the server cap. Omit it to use the configured cap.

Load the rows straight into a DataFrame:

```python
import pandas as pd
df = pd.DataFrame(c.sql("SELECT * FROM accidents WHERE Severity >= 3"))
```

## Filtered counts

```python
n = c.count("accidents", {
    "predicates": [{ "col": "State", "op": "in", "val": ["CA","TX"] }],
})
```

## Errors

Non-2xx responses raise `DataPressHTTPError` with three attributes:

| Attribute | Meaning                                                  |
|-----------|----------------------------------------------------------|
| `status`  | HTTP status code (`int`).                                |
| `body`    | Response body as `str` (may be empty).                   |
| `payload` | Parsed JSON body if the server sent one, else `None`.    |

```python
from datap_rs import DataPressHTTPError

try:
    c.query("missing", {})
except DataPressHTTPError as e:
    print(e.status, e.payload)
```

## With a URL prefix

```python
c = DataPressClient("http://127.0.0.1:8000/datapress")
# Internally calls /datapress/api/v1/datasets, /datapress/healthz, ...
```

## Admin endpoints

`reload()` requires the server's `ADMIN_TOKEN`:

```python
c.reload("accidents", admin_token="...")  # -> {"dataset":..., "rows":..., "elapsed_ms":...}
```
