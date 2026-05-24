# datapress

[![PyPI](https://img.shields.io/pypi/v/datapress.svg)](https://pypi.org/project/datapress/)
[![Python](https://img.shields.io/pypi/pyversions/datapress.svg)](https://pypi.org/project/datapress/)

**A fast multi-backend dataset HTTP server, built in Rust and driven from Python.**

`datapress` exposes one or more **Parquet** or **Delta** datasets over a small
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
pip install datapress
# or
uv pip install datapress
```

Wheels are published for macOS (arm64/x86_64), Linux (x86_64/aarch64) and
Windows (x86_64) against CPython 3.9+ (abi3).

---

## Quick start

```python
import asyncio
from datapress import DataPress, DataPressConfig, DatasetConfig

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
curl http://localhost:8000/api/datasets
curl http://localhost:8000/api/datasets/accidents/schema
curl -X POST http://localhost:8000/api/datasets/accidents/query \
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

Four classes, no module-level state:

| Class             | Purpose                                                              |
|-------------------|----------------------------------------------------------------------|
| `DataPressConfig` | Server tuning: `backend`, `listen`, `port`, `workers`, `prefix`.     |
| `DatasetConfig`   | One dataset: `name`, `source`, `format`, `mode`, optional S3 + index.|
| `S3Config`        | S3 / S3-compatible credentials and endpoint config.                  |
| `DataPress`       | Built from a `DataPressConfig` + list of `DatasetConfig`. `await .run()`. |

Hover any of them in your IDE for full kwarg docs.

### S3 / S3-compatible sources

```python
from datapress import DataPress, DataPressConfig, DatasetConfig, S3Config

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
# → GET /datapress/api/datasets, GET /datapress/health, ...
```

`prefix` must start with `/` and not end with `/`. Empty string (default)
mounts at the root.

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
| GET    | `/api/datasets`                       | List configured datasets.                  |
| GET    | `/api/datasets/{name}/schema`         | Inferred columns + sample row.             |
| POST   | `/api/datasets/{name}/query`          | Filter + paginate.                         |
| POST   | `/api/datasets/{name}/count`          | Total or filtered row count.               |
| POST   | `/api/datasets/{name}/reload`         | Atomic dataset reload (requires admin token). |

### Query body

```json
{
  "columns":   ["ID","City","State","Severity"],
  "predicates": [
    { "col": "State",    "op": "eq",  "val": "TX" },
    { "col": "Severity", "op": "gte", "val": 3   }
  ],
  "page":      1,
  "page_size": 50
}
```

| Field        | Type            | Default | Notes                       |
|--------------|-----------------|---------|-----------------------------|
| `columns`    | `string[]`      | `[]`    | Empty = all columns.        |
| `predicates` | `Predicate[]`   | `[]`    | ANDed together.             |
| `page`       | `int >= 1`      | `1`     | 1-based.                    |
| `page_size`  | `int 1..=1000`  | `100`   | Clamped.                    |

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

### Count body

Same predicate shape, no projection or pagination:

```json
{ "predicates": [ { "col": "State", "op": "eq", "val": "TX" } ] }
```

Response: `{ "count": <int> }`. Empty body (`{}`) counts every row. On
materialised DataFusion datasets, the no-predicate case is O(1) and indexed
`eq` / `in` predicates short-circuit through the equality index.

```bash
curl -X POST http://localhost:8000/api/datasets/accidents/count \
  -H 'Content-Type: application/json' -d '{}'
# → { "count": 7728394 }
```

### Admin reload

`POST /api/datasets/{name}/reload` rebuilds a dataset from its source and
atomically swaps it in. Requires the `X-Admin-Token` header to match the
`ADMIN_TOKEN` env var. **Endpoint is disabled when `ADMIN_TOKEN` is unset**
(secure default).

```python
import os
os.environ["ADMIN_TOKEN"] = "supersecret"     # before constructing DataPress
```

```bash
curl -X POST -H "X-Admin-Token: supersecret" \
  http://localhost:8000/api/datasets/accidents/reload
# → { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```

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
