# datap-rs

```
██████╗  █████╗ ████████╗ █████╗ ██████╗       ██████╗ ███████╗
██╔══██╗██╔══██╗╚══██╔══╝██╔══██╗██╔══██╗      ██╔══██╗██╔════╝
██║  ██║███████║   ██║   ███████║██████╔╝█████╗██████╔╝███████╗
██║  ██║██╔══██║   ██║   ██╔══██║██╔═══╝ ╚════╝██╔══██╗╚════██║
██████╔╝██║  ██║   ██║   ██║  ██║██║           ██║  ██║███████║
╚═════╝ ╚═╝  ╚═╝   ╚═╝   ╚═╝  ╚═╝╚═╝           ╚═╝  ╚═╝╚══════╝
```
                                                               

[![PyPI](https://img.shields.io/pypi/v/datap-rs.svg)](https://pypi.org/project/datap-rs/)
[![Python](https://img.shields.io/pypi/pyversions/datap-rs.svg)](https://pypi.org/project/datap-rs/)![PyPI - Downloads](https://img.shields.io/pypi/dm/datap-rs)![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust)
![DuckDB](https://img.shields.io/badge/backend-DuckDB-yellow?logo=duckdb)
![DataFusion](https://img.shields.io/badge/backend-DataFusion-blue)




**A fast multi-backend dataset HTTP server, built in Rust and driven from Python.**

`datap-rs` (datapress) exposes one or more **Parquet** or **Delta** datasets over a small
JSON HTTP API. It ships with two pluggable engines bundled into a single
wheel — pick one at runtime:

- **DuckDB** — battle-tested SQL, lazy parquet reads, low startup.
- **DataFusion** — pure-Rust, in-memory `RecordBatch` + equality index for
  low-latency point lookups.

Identical request/response shapes across both, so you can A/B them under your
real workload.

---

## Install

```bash
pip install datap-rs
# or
uv pip install datap-rs
```

Wheels are published for Linux (x86_64/aarch64), macOS (arm64), and Windows
(x86_64) against CPython 3.9+ (abi3).

---

## Quick start

For testing, we're using this [kaggle US accidents 2016-2023](https://www.kaggle.com/datasets/sobhanmoosavi/us-accidents) dataset.


```python
import asyncio
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

async def main() -> None:
    ds = DatasetConfig(
        name="accidents",
        source="data/accidents.parquet",
        format="parquet",          # or "delta"
        mode="auto",               # eq-index policy: "auto" | "none" | "list"
        description="US accidents 2016-2023",
    )
    cfg = DataPressConfig(
        backend="datafusion",      # or "duckdb"
        listen="0.0.0.0",
        port=8000,
        workers=8,
    )
    server = DataPress(cfg, datasets=[ds])
    await server.run()              # blocks until SIGINT

if __name__ == "__main__":
    asyncio.run(main())
```

Hit it:

```bash
curl http://localhost:8000/api/v1/datasets
curl http://localhost:8000/api/v1/datasets/accidents/schema
curl -X POST http://localhost:8000/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID","Severity","City","State"],
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3   }
    ],
    "page": 1, "page_size": 50
  }'
```

---

## API surface

Six public classes, no module-level state:

| Class             | Purpose                                                              |
|-------------------|----------------------------------------------------------------------|
| `DataPressConfig` | Server tuning: `backend`, `listen`, `port`, `workers`, `prefix`, `compress`, `max_body_bytes`, `request_timeout_ms`, `shutdown_timeout_secs`, `metrics_enabled`, `metrics_path`. |
| `DatasetConfig`   | One dataset: `name`, `source`, `format`, `mode`, optional S3 + index.|
| `S3Config`        | S3 / S3-compatible credentials and endpoint config.                  |
| `DataPress`       | Built from a `DataPressConfig` + list of `DatasetConfig` + optional `AuthConfig`. `await .run()`. |
| `AuthConfig`      | OIDC / OAuth2 bearer enforcement (requires the `auth` feature in the wheel). |
| `DataPressClient` | Sync HTTP client for talking to a running server (stdlib + lazy pyarrow). |

Hover any of them in your IDE for full kwarg docs.

### S3 / S3-compatible sources

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig, S3Config

s3 = S3Config(
    region="us-east-1",
    endpoint="http://localhost:9000",   # MinIO / R2 / Wasabi / Backblaze
    addressing_style="path",            # or "virtual"
    allow_http=True,                    # only for non-https endpoints
)

ds = DatasetConfig(
    name="events",
    source="s3://events/2025/",
    format="parquet",                    # or "delta"
    s3=s3,
)
```

Credentials fall back to the standard AWS env vars
(`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
`AWS_REGION`) when not set inline.

### Behind a reverse proxy

Set `prefix` to mount every route under a URL path — handy when nginx /
Traefik / Caddy forwards the prefix verbatim:

```python
DataPressConfig(backend="datafusion", port=8000, prefix="/datapress")
# → GET /datapress/api/v1/datasets, GET /datapress/health, ...
```

`prefix` must start with `/` and not end with `/`. Empty string (default)
mounts at the root.

### Response compression

Compression is on by default and negotiated per request via the
`Accept-Encoding` header (gzip, brotli, zstd). Clients that want raw JSON
send `Accept-Encoding: identity` or omit the header. Turn it off at the
server when sitting behind a proxy that already compresses, or to save
CPU on a trusted LAN:

```python
DataPressConfig(backend="datafusion", port=8000, compress=False)
```

### Request limits & timeouts

Two server-side guardrails are on by default:

```python
DataPressConfig(
    backend="datafusion",
    port=8000,
    max_body_bytes=1_048_576,    # 413 above this; default 1 MiB
    request_timeout_ms=30_000,   # 504 above this; 0 disables; default 30s
    shutdown_timeout_secs=30,    # SIGTERM/SIGINT grace period, in seconds
)
```

Bodies larger than `max_body_bytes` are rejected with `413 Payload Too
Large`. Handlers that take longer than `request_timeout_ms` are cancelled
and the client sees `504 Gateway Timeout`. Set the timeout to `0` to
disable it entirely (useful behind a proxy that already enforces one).

### Graceful shutdown

On `SIGTERM` or `SIGINT` (Ctrl+C) the server stops accepting new
connections, then waits up to `shutdown_timeout_secs` seconds for
in-flight requests to finish before stopping workers. Set it lower for
faster restarts, higher for long-running query handlers.

### Client

A small sync client is bundled for talking to a running server:

```python
from datap_rs import DataPressClient

c = DataPressClient("http://127.0.0.1:8000")
c.healthz()                                  # -> {"status": "ok"}
c.readyz()                                   # -> {"status": "ready", "datasets": N}
c.datasets()                                 # -> ["accidents", ...]
c.schema("accidents")                        # -> dict
c.count("accidents")                         # -> int
table = c.query("accidents", {               # -> pyarrow.Table
    "columns":   ["State", "Severity"],
    "page_size": 10_000,
})
```

`query()` requests Arrow IPC and returns a `pyarrow.Table` (pyarrow is
imported lazily). For the JSON envelope verbatim, use `query_json()`.
On non-2xx responses a `DataPressHTTPError` is raised with `.status`,
`.body` and `.payload`.

### Equality-index policy (DataFusion only)

```python
DatasetConfig(
    name="big",
    source="data/big.parquet",
    mode="list",                                  # "auto" | "none" | "list"
    index_columns=["State", "Severity"],          # required for "list"
    index_max_cardinality=100_000,                # used by "auto"
)
```

- `auto` — index every column whose distinct count stays below `index_max_cardinality`.
- `none` — skip the index; every query goes through DataFusion SQL.
- `list` — index only `index_columns`. Best for very wide datasets.

DuckDB ignores this block.

---

## HTTP API

Same five routes for both backends.

| Method | Path                                  | Purpose                                    |
|--------|---------------------------------------|--------------------------------------------|
| GET    | `/health`                             | Liveness probe.                            |
| GET    | `/api/v1/datasets`                    | List configured datasets.                  |
| GET    | `/api/v1/datasets/{name}/schema`      | Inferred columns + sample row.             |
| POST   | `/api/v1/datasets/{name}/query`       | Filter + paginate.                         |
| POST   | `/api/v1/datasets/{name}/count`       | Total or filtered row count.               |
| POST   | `/api/v1/datasets/{name}/reload`      | Atomic dataset reload (requires admin token). |

### Query body

```json
{
  "columns":   ["ID","City","State","Severity"],
  "predicates": [
    { "col": "State",    "op": "eq",  "val": "TX" },
    { "col": "Severity", "op": "gte", "val": 3   }
  ],
  "order_by": [ { "col": "Severity", "dir": "desc" } ],
  "limit":     1000,
  "page":      1,
  "page_size": 50
}
```

| Field        | Type            | Default | Notes                       |
|--------------|-----------------|---------|-----------------------------|
| `columns`    | `string[]`      | `[]`    | Empty = all columns.        |
| `predicates` | `Predicate[]`   | `[]`    | ANDed together.             |
| `order_by`   | `OrderBy[]`     | `[]`    | `{ col, dir? }`; `dir` is `asc` (default) or `desc`. |
| `group_by`     | `string[]`     | `[]`    | Group-by columns; when set, `columns` is ignored. |
| `aggregations` | `Aggregation[]` | `[]`   | `{ col?, op, alias? }`; ops: `count\|sum\|avg\|min\|max`. Requires `group_by`. |
| `distinct`   | `bool`          | `false` | Dedup the projected columns. Mutually exclusive with `group_by` / `aggregations`. |
| `limit`      | `int` or null   | `null`  | Hard cap on total rows across pages. |
| `page`       | `int >= 1`      | `1`     | 1-based.                    |
| `page_size`  | `int 1..=1_000_000`  | `1000`   | Clamped.                    |

### Predicate operators

| `op`          | `val`                 | Meaning                       |
|---------------|-----------------------|-------------------------------|
| `eq`          | scalar                | `col = val`                   |
| `neq`         | scalar                | `col <> val`                  |
| `gt` / `gte`  | number / string       | `col > val` / `col >= val`    |
| `lt` / `lte`  | number / string       | `col < val` / `col <= val`    |
| `like`        | string with `%`/`_`   | SQL `LIKE`                    |
| `ilike`       | string with `%`/`_`   | Case-insensitive `LIKE`       |
| `in`          | non-empty array       | `col IN (v1, v2, …)`          |
| `is_null`     | omit                  | `col IS NULL`                 |
| `is_not_null` | omit                  | `col IS NOT NULL`             |

### Grouping / aggregation

```bash
curl -X POST http://localhost:8000/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "group_by": ["State"],
    "aggregations": [
      { "op":  "count" },
      { "col": "Severity", "op": "avg", "alias": "avg_sev" }
    ],
    "order_by": [{ "col": "count", "dir": "desc" }],
    "page_size": 10
  }'
```

When `group_by` is non-empty the SELECT list is derived from the group
columns plus each aggregation's alias; the top-level `columns` field is
ignored. `aggregations` without `group_by` returns `400`. `order_by` keys
must be a group column or aggregation alias.

### Distinct

```bash
curl -X POST http://localhost:8000/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{ "columns": ["State"], "distinct": true, "order_by": [{"col":"State"}] }'
```

Mutually exclusive with `group_by` / `aggregations`.

### Arrow IPC responses

Opt in per-request with the `Accept` header (or `?format=arrow`) to skip
the JSON envelope and receive an Arrow IPC stream instead:

```python
import requests, pyarrow.ipc as ipc, polars as pl

r = requests.post(
    "http://localhost:8000/api/v1/datasets/accidents/query",
    json={"columns": ["ID","State"], "page_size": 1000},
    headers={"Accept": "application/vnd.apache.arrow.stream"},
)
table = ipc.open_stream(r.content).read_all()   # pyarrow.Table
df    = pl.from_arrow(table)                    # zero-copy → Polars
page, page_size = r.headers["X-Page"], r.headers["X-Page-Size"]
```

To read the complete result set into Polars, walk pages until the server
returns fewer rows than requested:

```python
import pyarrow as pa
import pyarrow.ipc as ipc
import polars as pl
import requests

ARROW = "application/vnd.apache.arrow.stream"


def query_all_polars(
    base_url: str,
    dataset: str,
    body: dict,
    page_size: int = 100_000,
) -> pl.DataFrame:
    tables: list[pa.Table] = []
    page = 1

    with requests.Session() as session:
        while True:
            response = session.post(
                f"{base_url.rstrip('/')}/api/v1/datasets/{dataset}/query",
                json={**body, "page": page, "page_size": page_size},
                headers={"Accept": ARROW},
            )
            response.raise_for_status()

            table = ipc.open_stream(response.content).read_all()
            tables.append(table)

            if table.num_rows < page_size:
                break
            page += 1

    table = tables[0] if len(tables) == 1 else pa.concat_tables(tables)
    return pl.from_arrow(table)
```

Use a deterministic `order_by` for full exports from datasets that may be
reloaded while you page through results. Arrow IPC is supported by both
backends.

### Count body

Same predicate shape, no projection or pagination:

```json
{ "predicates": [ { "col": "State", "op": "eq", "val": "TX" } ] }
```

Response: `{ "count": <int> }`. Empty body (`{}`) counts every row. On
materialised DataFusion datasets, the no-predicate case is O(1) and indexed
`eq` / `in` predicates short-circuit through the equality index.

```bash
curl -X POST http://localhost:8000/api/v1/datasets/accidents/count \
  -H 'Content-Type: application/json' -d '{}'
# → { "count": 7728394 }
```

### Admin reload

`POST /api/v1/datasets/{name}/reload` rebuilds a dataset from its source and
atomically swaps it in. Requires the `X-Admin-Token` header to match the
`ADMIN_TOKEN` env var. **Endpoint is disabled when `ADMIN_TOKEN` is unset**
(secure default).

```python
import os
os.environ["ADMIN_TOKEN"] = "supersecret"     # before constructing DataPress
```

```bash
curl -X POST -H "X-Admin-Token: supersecret" \
  http://localhost:8000/api/v1/datasets/accidents/reload
# → { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```

Reload publication is backend-specific. DataFusion uses a service-level
double buffer: it builds a new `DatasetState` off to the side, then
publishes it with an `ArcSwap` snapshot update. In-flight queries keep
using the old Arrow buffers; later queries see the new state. Peak RSS can
approach roughly twice the materialised dataset size during reload.

DuckDB delegates the heavy publication step to the engine with
`CREATE OR REPLACE TABLE ... AS SELECT ...`. DuckDB handles that as an
ACID transaction over the table/catalog replacement: failures leave the
existing table live, and successful reloads become visible atomically to
later queries while in-flight queries continue against their starting
snapshot. DataPress then refreshes its small cached schema and row-count
metadata. Per-dataset reloads are serialised by an async mutex; reloads
of different datasets run in parallel.

---

## Authentication (OIDC / OAuth2)

Optional bearer-token enforcement against any OpenID Connect issuer
(Keycloak, Auth0, Entra ID, Okta, Zitadel, …). Requires a wheel built
with the `auth` Cargo feature:

```bash
maturin build --release --features auth
```

Pre-built PyPI wheels include it by default.

```python
from datap_rs.datapress import (
    DataPress, DataPressConfig, DatasetConfig, AuthConfig,
)

auth = AuthConfig(
    enabled=True,
    issuer="http://localhost:8080/realms/datapress",
    audience="datapress-api",
    read_scopes=["datasets:read"],
    reload_scopes=["datasets:reload"],
    # anonymous_read=False,
    # algorithms=["RS256"],
    # leeway_secs=60,
    # jwks_refresh_secs=3600,
    # tenant_claim="/tenant_id",
    # allowed_tenants=["acme"],
    # admin_token_fallback=True,    # honour legacy X-Admin-Token
    # start_degraded=True,          # boot even if JWKS fetch fails
)

server = DataPress(cfg, datasets=[ds], auth=auth)
await server.run()
```

When `enabled=False` (default) all other fields are ignored and the
server behaves exactly as before. Validation errors (missing issuer,
malformed `tenant_claim`, …) raise `ValueError` at construction time.

Call any endpoint with `Authorization: Bearer <jwt>`. Reload endpoints
require `reload_scopes`; read endpoints require `read_scopes` unless
`anonymous_read=True`.

### Try it locally

The repo ships a one-command Keycloak stack at
[`examples/keycloak/`](https://github.com/jeroenflvr/fast-api/tree/main/examples/keycloak)
with a pre-provisioned realm, service-account client, scopes and a test
user. `docker compose up -d` and point `issuer` at
`http://localhost:8080/realms/datapress`.

---

## Choosing a backend

- **DuckDB** — the safe default. Handles arbitrary SQL well, manages its own
  buffer pool, starts up in milliseconds because it lazily reads parquet
  pages on demand.
- **DataFusion** — pick when the data fits in RAM and you repeatedly query
  the same columns with equality / `IN` predicates; the eq-index turns
  those into O(1) lookups. Also produces a leaner static binary (no
  vendored C++).

Both engines are compiled into the same wheel — switching is one keyword
argument away.

---

## Logging

`datapress` initialises `env_logger` on import. Control verbosity with the
standard `RUST_LOG` variable:

```bash
RUST_LOG=info  python example.py
RUST_LOG=debug python example.py
```

---

## License

MIT. See [LICENSE](LICENSE) in the source repo.

Source, issue tracker and Rust crates: <https://github.com/jeroenflvr/fast-api>
