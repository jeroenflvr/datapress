![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust)
![DuckDB](https://img.shields.io/badge/backend-DuckDB-yellow?logo=duckdb)
![DataFusion](https://img.shields.io/badge/backend-DataFusion-blue?logo=apache)![actix](https://img.shields.io/badge/backend-actix-orange?logo=actix)

# datap-rs

A Rust **Cargo workspace** that exposes one or more **Parquet / Delta
datasets** over a JSON HTTP API. The same surface area is implemented twice —
once on top of **DuckDB**, once on top of **Apache Arrow + DataFusion** — so
you can A/B the engines under identical workloads. A Python wheel
(`datap-rs`, built with maturin + PyO3) bundles both engines and lets you
configure and launch the server from Python.

- Built on [actix-web](https://actix.rs/) 4
- Datasets declared in a single [`datasets.toml`](datasets.toml) (Rust
  binaries) or programmatically (Python wrapper)
- Dynamic schema inference at startup (no hard-coded columns)
- Identical request/response shapes across both backends

---

## Quick start

For testing, we're using this [kaggle US accidents 2016-2023](https://www.kaggle.com/datasets/sobhanmoosavi/us-accidents) dataset.


```bash
# 1. Put a parquet file somewhere (or point the config at an existing one).
ls data/accidents.parquet

# 2. Edit datasets.toml — see the example shipped in this repo.

# 3. Run a backend.
task run:duckdb        # or: task run:datafusion

# 4. Talk to it.
curl http://localhost:8080/api/datasets
```

`Taskfile.yml` wraps the typical `cargo build --release -p …` invocations;
see [`task --list`](Taskfile.yml) for the full menu.

### From Python

The same server can be configured and launched from Python via the
`datapress` wheel (one wheel, both engines bundled):

```python
import asyncio
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

async def main():
    ds = DatasetConfig(
        name="accidents",
        source="data/accidents.parquet",
        format="parquet",   # or "delta"
        mode="auto",        # index policy: "auto" | "none" | "list"
    )
    cfg = DataPressConfig(backend="duckdb", listen="0.0.0.0", port=8000, workers=8)
    server = DataPress(cfg, datasets=[ds])
    await server.run()      # blocks until SIGINT

asyncio.run(main())
```

Build the wheel with `task py:develop` (uses `uv` + `maturin`).

---

## The two backends

| Aspect              | `datapress-duckdb`                             | `datapress-datafusion`                               |
|---------------------|------------------------------------------------|------------------------------------------------------|
| Engine              | DuckDB (embedded C++)                          | Arrow compute + DataFusion (pure Rust)               |
| Storage             | DuckDB in-memory table per dataset             | One contiguous `RecordBatch` per dataset             |
| Concurrency model   | Connection pool, blocking → `web::block`       | Async-native, multi-threaded `MemTable` partitions   |
| Predicate execution | DuckDB optimiser + parallel hash/vector ops    | Equality index → SIMD scan → DataFusion SQL          |
| Indexes             | Native DuckDB internals (zone maps, etc.)      | Per-dataset eq-index built at startup (configurable) |
| Memory profile      | DuckDB's own buffer manager                    | Whole dataset resident in RAM                        |
| Binary size         | Bundled DuckDB ≈ tens of MB                    | Lean — pure Rust                                     |
| Startup time        | Fast (just `read_parquet`)                     | Slower — reads all rows + builds eq-index            |
| Best at             | Heterogeneous SQL, joins, aggregations         | Dense filter scans, low-latency point lookups        |

### When to pick which

- **DuckDB** is the right default. It handles arbitrary SQL well, has a
  battle-tested optimiser, manages memory itself, and starts up in
  milliseconds because it lazily reads parquet pages on demand.
- **DataFusion** shines when:
  - the dataset fits comfortably in RAM,
  - you query the same columns repeatedly with equality/`IN` predicates
    (the in-process equality index turns those into O(1) lookups), and
  - you want a single static binary without a vendored C++ runtime.

The HTTP API is identical, so the practical comparison is "throughput and
p99 on your queries" — see [`TEST_Q.md`](TEST_Q.md) for a benchmark suite.

---

## Configuration: `datasets.toml`

Every instance reads this file at startup. One `[server]` block plus one
`[[dataset]]` entry per table you want to expose.

```toml
[server]
backend = "datafusion"   # "datafusion" (default) | "duckdb"
listen  = "127.0.0.1"    # default; set to "0.0.0.0" to expose
port    = 8080
# workers = 8            # omit for one worker per CPU
# compress = true        # negotiate gzip/brotli/zstd via Accept-Encoding (default)
# max_body_bytes     = 1048576  # 413 above this; default 1 MiB
# request_timeout_ms = 30000    # 504 above this; 0 disables; default 30s
# shutdown_timeout_secs = 30    # SIGTERM/SIGINT grace period, in seconds

[[dataset]]
name = "accidents"                    # used in the URL: /api/datasets/accidents/...

  [dataset.source]
  kind     = "parquet"                # "parquet" | "delta"
  location = "data/accidents.parquet" # file, directory of *.parquet, or s3://…

  # Optional — DataFusion only. DuckDB ignores this block.
  [dataset.index]
  mode             = "auto"           # "auto" | "none" | "list"
  columns          = []               # required when mode = "list"
  max_cardinality  = 100000           # used by "auto" to skip wide cols
```

### Server

| Field     | Default       | Notes                                                                                          |
|-----------|---------------|------------------------------------------------------------------------------------------------|
| `backend` | `datafusion`  | Informational hint; logged at startup. Each binary always runs as its own backend regardless of this value. |
| `listen`  | `127.0.0.1`   | Loopback by default — the service is **not** exposed on a network interface unless you opt in. |
| `port`    | `8080`        |                                                                                                |
| `workers` | *(unset)*     | Actix worker threads. Unset = one per CPU.                                                     |
| `prefix`  | `""`          | URL path prefix mounted in front of every route (e.g. `"/datapress"`) — useful behind a reverse proxy that passes the path through unchanged. Must start with `/` and not end with `/`. |
| `compress`           | `true`     | Negotiate response compression via `Accept-Encoding` (gzip / brotli / zstd). Disable when sitting behind a proxy that compresses for you. |
| `max_body_bytes`     | `1048576`  | Maximum accepted JSON request body, in bytes. Bigger bodies are rejected with `413 Payload Too Large`. |
| `request_timeout_ms` | `30000`    | Per-request handler timeout, in milliseconds. Long-running handlers are cancelled and the client gets `504 Gateway Timeout`. `0` disables the timeout. |
| `shutdown_timeout_secs` | `30`     | Grace period for in-flight requests after the process receives `SIGTERM` / `SIGINT`, in seconds. The listening socket is closed immediately; existing connections then have up to this many seconds to finish before workers are force-stopped. |

The server exposes three probe endpoints. `/healthz` and `/readyz` are
mounted at the bare host root (regardless of `prefix`) so orchestrators
don't need to know how the service is exposed. `/health` lives under
`prefix` and is intended for in-app health checks.

| Route      | Status                                                                 | Body                                                                       |
|------------|------------------------------------------------------------------------|----------------------------------------------------------------------------|
| `/healthz` | Liveness — always `200` while the process is running.                  | `{"status":"ok"}`                                                          |
| `/readyz`  | Readiness — `200` once at least one dataset is registered, `503` otherwise. | `{"status":"ready","datasets":N}` / `{"status":"not ready","reason":"no datasets registered"}` |
| `/version` | Build / version metadata — always `200`.                              | `{"name":"datapress-core","version":"x.y.z","backend":"DuckDB\|DataFusion","profile":"debug\|release", ...}` |
| `{prefix}/health` | App-level liveness — always `200`.                             | `{"status":"ok"}`                                                          |

`/healthz` does not touch the backend, so it stays `200` even while the
dataset registry is still loading at startup. Use `/readyz` to gate
traffic until the server is actually able to serve queries.

`/version` also includes optional fields populated from build-time env
vars when set: `git_sha` (`DATAPRESS_GIT_SHA`), `build_time`
(`DATAPRESS_BUILD_TIME`, ISO-8601), and `target`
(`DATAPRESS_TARGET`, e.g. `aarch64-apple-darwin`). Unset vars are
omitted from the JSON. Example:

```bash
DATAPRESS_GIT_SHA=$(git rev-parse --short HEAD) \
DATAPRESS_BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ) \
DATAPRESS_TARGET=$(rustc -vV | awk '/host:/ {print $2}') \
  cargo build --release -p datapress-duckdb
```

### Source

`[dataset.source]` is a tagged enum.

| `kind`    | `location`                                          | Notes                                                                                  |
|-----------|-----------------------------------------------------|----------------------------------------------------------------------------------------|
| `parquet` | a `.parquet` file                                   | Read as-is.                                                                            |
| `parquet` | a directory                                         | Every `*.parquet` inside (sorted, non-recursive). No glob patterns.                    |
| `parquet` | `s3://bucket/key.parquet` or `s3://bucket/prefix/`  | Requires a `[dataset.s3]` block. DuckDB autoloads `httpfs`.                            |
| `delta`   | a local directory                                   | Pointed at the table root (the dir containing `_delta_log/`).                          |
| `delta`   | `s3://bucket/path/to/table`                         | Requires `[dataset.s3]`. DuckDB autoloads `delta`; DataFusion uses the `deltalake` crate. |

#### S3 / S3-compatible storage

```toml
[[dataset]]
name = "events"

  [dataset.source]
  kind     = "parquet"           # or "delta"
  location = "s3://events/2025/*.parquet"

  [dataset.s3]
  region            = "us-east-1"
  endpoint          = "http://localhost:9000"  # omit for AWS
  addressing_style  = "path"                   # "virtual" (default) | "path"
  allow_http        = true                     # only for non-https endpoints
```

| Field              | Default       | Notes                                                                          |
|--------------------|---------------|--------------------------------------------------------------------------------|
| `region`           | `us-east-1`   | Falls back to `AWS_REGION` env, then `us-east-1`.                              |
| `endpoint`         | *(unset)*     | Custom S3 endpoint (MinIO, R2, Wasabi, Backblaze, …).                          |
| `addressing_style` | `virtual`     | `virtual` = `https://bucket.host`, `path` = `https://host/bucket` (MinIO).     |
| `allow_http`       | `false`       | Must be `true` if `endpoint` is `http://…`.                                    |
| `access_key_id`, `secret_access_key`, `session_token` | *(unset)* | Inline creds. Discouraged for prod — use env vars instead. |

**Credential precedence** (highest → lowest):

1. Per-dataset env vars: `${PREFIX}_AWS_ACCESS_KEY_ID`, `${PREFIX}_AWS_SECRET_ACCESS_KEY`, `${PREFIX}_AWS_SESSION_TOKEN`, `${PREFIX}_AWS_REGION`.
   `PREFIX` is the dataset name uppercased with every non-alphanumeric character mapped to `_` (e.g. `accidents` → `ACCIDENTS_AWS_…`, `my-bucket` → `MY_BUCKET_AWS_…`).
2. Inline `[dataset.s3]` keys.
3. Plain `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_REGION`.
4. The backend's default credential chain (`~/.aws/credentials`, IMDS, etc.).

> When `kind = "delta"` and `location` is an `s3://…` URL, both backends fully materialise the table at startup. There is no incremental scan path — switch to `parquet` if you need on-demand page reads.

### Equality-index policy (DataFusion only)

The DataFusion backend builds an in-memory `value -> [row ids]` map at
startup so that `eq` / `in` predicates resolve in O(1).

| `mode`   | Behaviour                                                              |
|----------|------------------------------------------------------------------------|
| `auto`   | Index every column whose distinct count stays below `max_cardinality`. |
| `none`   | Skip the index entirely — every query goes through DataFusion SQL.     |
| `list`   | Index only the named `columns`. Useful for huge datasets.              |

Override the config path with `DATASETS_CONFIG=/path/to/file.toml`.

## HTTP API

Four routes, both backends:

### API versioning

The canonical paths live under `/api/v1/...`. The un-versioned
`/api/...` paths shown in every example below continue to work as a
**legacy alias** for v1, so existing clients keep running. To upgrade,
replace `/api/` with `/api/v1/` in your URLs — nothing else changes.

```text
POST /api/v1/datasets/accidents/query      # canonical (recommended)
POST /api/datasets/accidents/query         # legacy alias, still v1
```

When a breaking schema change is introduced, it will ship as `/api/v2`
in a sibling module ([crates/core/src/handlers/v1.rs](crates/core/src/handlers/v1.rs))
and v1 will stay mounted alongside it for a deprecation window.

### `GET /api/datasets`

```json
{ "datasets": [ { "name": "accidents", "columns": 47 } ] }
```

### `GET /api/datasets/{name}/schema`

Returns the inferred columns plus a sample row so a client can see what
values look like without issuing a query.

```json
{
  "name": "accidents",
  "columns": [
    { "name": "ID",       "logical": "utf8", "sql_type": "VARCHAR",   "nullable": false },
    { "name": "Severity", "logical": "int",  "sql_type": "INTEGER",   "nullable": true  },
    { "name": "Start_Time", "logical": "temporal", "sql_type": "TIMESTAMP", "nullable": true }
  ],
  "sample": { "ID": "A-1", "Severity": 2, "Start_Time": "2016-02-08 05:46:00", ... }
}
```

`logical` values: `bool | int | float | utf8 | temporal | other`. Temporal
columns are returned as strings.

### `POST /api/datasets/{name}/query`

```json
{
  "columns":   ["ID", "City", "State", "Severity"],
  "predicates": [
    { "col": "State",    "op": "eq",  "val": "TX" },
    { "col": "Severity", "op": "gte", "val": 3   }
  ],
  "order_by": [
    { "col": "Severity", "dir": "desc" },
    { "col": "ID" }
  ],
  "limit":     1000,
  "page":      1,
  "page_size": 50
}
```

Response:

```json
{ "data": [ { ... }, ... ], "page": 1, "page_size": 50 }
```

#### Request fields

| Field        | Type                | Default | Notes                                  |
|--------------|---------------------|---------|----------------------------------------|
| `columns`    | `string[]`          | `[]`    | Empty = all columns.                   |
| `predicates` | `Predicate[]`       | `[]`    | ANDed together.                        |
| `order_by`   | `OrderBy[]`         | `[]`    | `{ col, dir? }`; `dir` is `asc` (default) or `desc`, case-insensitive. When `group_by` is set, `col` must be a group column or aggregation alias. |
| `group_by`   | `string[]`          | `[]`    | Columns to group by. When set, `columns` is ignored. Empty `aggregations` implies `[{ op: "count" }]`. |
| `aggregations` | `Aggregation[]`   | `[]`    | `{ col?, op, alias? }`; `op` is `count\|sum\|avg\|min\|max`. `col` may be omitted only for `count` (= `COUNT(*)`). Requires `group_by`. |
| `distinct`   | `bool`              | `false` | Dedup the projected columns. Mutually exclusive with `group_by` / `aggregations`. |
| `limit`      | `int >= 0` or null  | `null`  | Hard cap on total rows across all pages. `null` = unlimited. |
| `page`       | `int >= 1`          | `1`     | 1-based.                               |
| `page_size`  | `int 1..=1_000_000`      | `1000`   | Clamped to the inclusive range.        |

#### Predicate shape

```json
{ "col": "<column>", "op": "<operator>", "val": <json value | array | omitted> }
```

| `op`           | `val`                  | Meaning                              |
|----------------|------------------------|--------------------------------------|
| `eq`           | scalar                 | `col = val`                          |
| `neq`          | scalar                 | `col <> val`                         |
| `gt` / `gte`   | number / string        | `col > val` / `col >= val`           |
| `lt` / `lte`   | number / string        | `col < val` / `col <= val`           |
| `like`         | string with `%` / `_`  | SQL `LIKE`                           |
| `ilike`        | string with `%` / `_`  | Case-insensitive `LIKE`              |
| `in`           | non-empty array        | `col IN (v1, v2, …)`                 |
| `is_null`      | omit                   | `col IS NULL`                        |
| `is_not_null`  | omit                   | `col IS NOT NULL`                    |

Column names are looked up case-insensitively against the inferred schema
and quoted automatically, so `Temperature(F)` and similar identifiers work.

#### Response format — JSON or Arrow IPC

`/query` can return its result set in two wire formats. Same body, same
predicates, same pagination — only the response encoding differs.

| Aspect              | JSON (default)                                       | Arrow IPC stream                                                                 |
|---------------------|------------------------------------------------------|----------------------------------------------------------------------------------|
| Content-Type        | `application/json`                                   | `application/vnd.apache.arrow.stream`                                            |
| How to ask          | nothing — it's the default                           | `Accept: application/vnd.apache.arrow.stream` **or** `?format=arrow` on the URL  |
| Shape               | Array of row objects (`[{...}, {...}, ...]`)         | Self-describing stream: 1 schema message + N `RecordBatch` messages + EOS        |
| Layout              | Row-oriented; column names repeated on every row     | Columnar; one contiguous buffer per column per batch                             |
| Types preserved     | Scalars become JSON (`int`/`float`/`bool`/`string`); temporals stringified to ISO-8601 | Native Arrow types — `Int32`, `Timestamp(ns)`, `Decimal128`, dictionary, etc. retained end-to-end |
| Page metadata       | In the body (just the rows, no envelope)             | In headers: `X-Page`, `X-Page-Size`                                              |
| Empty result        | `[]`                                                 | Valid stream with the schema message only, zero batches                          |
| Compression         | Big win — JSON is text                               | Smaller starting point; gzip/zstd still help on wide / repetitive cols, brotli usually skipped |
| Client cost         | `json.loads` + per-row dict construction             | `pyarrow.ipc.open_stream(...).read_all()` → zero-copy `pyarrow.Table`            |
| Best for            | Small responses, browsers, ad-hoc `curl`, dashboards | Bulk data into Polars / pandas / DuckDB-on-the-client, ML feature pipelines      |

**When to pick which.** Use JSON when the consumer is JavaScript, the
response is small (<~10k rows), or you're poking at the API by hand.
Use Arrow IPC when you're moving result pages into a dataframe library,
the schema has non-string types you want preserved, or page sizes are
large enough that JSON parse time shows up in profiles.

```bash
# JSON (default)
curl -X POST http://localhost:8080/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{ "predicates": [{ "col": "State", "op": "eq", "val": "TX" }] }'

# Arrow IPC — via Accept header
curl -X POST http://localhost:8080/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/vnd.apache.arrow.stream' \
  --output result.arrow \
  -d '{ "predicates": [{ "col": "State", "op": "eq", "val": "TX" }] }'

# Arrow IPC — via query string (handy when you can't set headers)
curl -X POST 'http://localhost:8080/api/v1/datasets/accidents/query?format=arrow' \
  -H 'Content-Type: application/json' \
  --output result.arrow \
  -d '{ "predicates": [{ "col": "State", "op": "eq", "val": "TX" }] }'
```

```python
import requests, pyarrow.ipc as ipc
r = requests.post(url, json=req, headers={"Accept": "application/vnd.apache.arrow.stream"})
table = ipc.open_stream(r.content).read_all()  # → pyarrow.Table
page  = int(r.headers["X-Page"])
size  = int(r.headers["X-Page-Size"])
```

Supported on **both** backends — DuckDB streams batches out via its
native `query_arrow` API, DataFusion uses its Arrow plan directly.
The `Compress` middleware still applies. `count`, `schema`, and the
dataset-listing endpoints are JSON-only.

#### Grouping / aggregation

When `group_by` is non-empty the SELECT list is derived from the group
columns plus each aggregation's output alias — the top-level `columns`
field is ignored. Supported ops: `count`, `sum`, `avg`, `min`, `max`
(case-insensitive). `col` may be omitted only for `count` (= `COUNT(*)`).
If `aggregations` is omitted an implicit `COUNT(*) AS count` is added.

```bash
curl -X POST http://localhost:8080/api/datasets/accidents/query \
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
# → { "data": [ { "State": "CA", "count": 1741433, "avg_sev": 2.21 }, ... ], ... }
```

`aggregations` without `group_by` returns `400`. `order_by` keys must
reference a group column or an aggregation alias (no arbitrary dataset
columns — they are not in scope after `GROUP BY`). Grouped queries always
go through the SQL engine; no in-memory fast path applies.

#### Distinct rows

`distinct: true` deduplicates on the projected columns. Useful for
building dropdowns / facet lists.

```bash
curl -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns":  ["State"],
    "distinct": true,
    "order_by": [{ "col": "State" }],
    "page_size": 100
  }'
# → { "data": [ { "State": "AL" }, { "State": "AR" }, ... ], ... }
```

Mutually exclusive with `group_by` / `aggregations` (returns `400` if
combined). Also bypasses the in-memory fast paths.

### `POST /api/datasets/{name}/count`

Returns the number of rows matching `predicates`. Same predicate shape as
`/query`; only the `predicates` field is read. Empty body counts every row.

```bash
curl -s -X POST http://localhost:8080/api/datasets/accidents/count \
  -H 'Content-Type: application/json' -d '{}'
# → { "count": 7728394 }

curl -s -X POST http://localhost:8080/api/datasets/accidents/count \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3   }
    ]
  }'
# → { "count": 187423 }
```

On materialised DataFusion datasets the no-predicate path is O(1) (uses the
resident chunk metadata, no scan); indexable predicates short-circuit
through the equality index. Otherwise it runs `SELECT COUNT(*) … WHERE …`
through the engine.

### `POST /api/datasets/{name}/reload` *(admin)*

Rebuilds the dataset from its configured `source` and atomically swaps it
in. Running queries finish against the old snapshot; the next query hits
the new data. The old in-memory copy is dropped once the last in-flight
request releases its reference.

Requires `X-Admin-Token: $ADMIN_TOKEN`. **If `ADMIN_TOKEN` is unset the
endpoint is disabled** — the secure default. The comparison is
constant-time.

```bash
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/datasets/accidents/reload
# { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```

| Status | Body                                          | Meaning                                              |
|--------|-----------------------------------------------|------------------------------------------------------|
| `200`  | `{ dataset, rows, elapsed_ms }`               | New data live.                                       |
| `403`  | `{ "error": "forbidden: …" }`                 | Token missing/wrong, or `ADMIN_TOKEN` not set.       |
| `404`  | `{ "error": "not found: dataset: …" }`        | No such dataset in `datasets.toml`.                  |
| `500`  | `{ "error": "internal error: …" }`            | Parquet read failed — old data stays live.           |

Concurrent reloads of the **same** dataset are serialised (per-name mutex);
reloads of **different** datasets run in parallel. Peak memory roughly
doubles during a reload because old and new copies coexist briefly.

#### How it works — double-buffered, lock-free swap

Hot-reload uses a classic **double-buffering** pattern. There is never a
"loading…" window where queries fail or block:

1. **Build off to the side.** The reload handler re-reads the dataset's
   `source` and constructs a brand-new `DatasetState` (parquet/Delta
   decode, equality-index build, partitioning) entirely off the hot path.
   Live queries keep hitting the current snapshot the whole time.
2. **Atomic pointer swap.** When the new state is ready, a single
   `ArcSwap::store` replaces the pointer in the shared dataset map. This
   is a single atomic word-write — no readers are paused, no locks held.
3. **Old copy retires lazily.** Queries that grabbed the old `Arc` before
   the swap keep using it; once the last in-flight request releases its
   reference, the old `Arc`'s strong count hits zero and the buffers are
   freed. No GC pause, no stop-the-world.
4. **Per-name serialisation.** A small per-dataset async mutex prevents two
   concurrent reloads of the *same* dataset from both rebuilding — they
   queue. Reloads of *different* datasets run fully in parallel.
5. **Failure is safe.** If the rebuild errors out (bad parquet, S3 timeout,
   …), the swap never happens. The old snapshot stays live and the
   handler returns `500` with the underlying error. Clients never observe
   a partially-loaded dataset.

Trade-off: peak RSS roughly doubles for the dataset being reloaded while
both buffers are resident. Reloads of different datasets don't compound
this — only the one being rebuilt is held twice.

---


## Examples

```bash
# Discovery
curl -s http://localhost:8080/api/datasets | jq
curl -s http://localhost:8080/api/datasets/accidents/schema | jq

# Equality + range
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID","Severity","City","State","Start_Time"],
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3 }
    ],
    "page": 1, "page_size": 5
  }' | jq

# Substring + numeric range
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "Description",    "op": "ilike", "val": "%fog%" },
      { "col": "Temperature(F)", "op": "lt",    "val": 32 }
    ],
    "page_size": 10
  }' | jq

# IN list
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "State", "op": "in", "val": ["NY","NJ","CT"] }
    ]
  }' | jq
```

For a deeper benchmark catalogue (light load + CPU/memory stress tests), see
[`TEST_Q.md`](TEST_Q.md).

---

## Project layout

```
Cargo.toml                          # workspace manifest
pyproject.toml                      # maturin / PyO3 build
crates/
├── core/                           # datapress-core: config, schema, errors, admin
│   └── src/
│       ├── admin.rs                # X-Admin-Token verification (constant-time)
│       ├── config.rs               # datasets.toml parsing + validation
│       ├── schema.rs               # backend-agnostic schema model
│       ├── models.rs               # Predicate / QueryRequest
│       └── errors.rs               # AppError + actix ResponseError
├── duckdb/                         # datapress-duckdb
│   └── src/
│       ├── lib.rs                  # pub async fn serve(cfg) -> io::Result<()>
│       ├── db.rs                   # Registry: pool + schemas + reload
│       ├── repository.rs           # DatasetRepository (SQL builder)
│       ├── handlers.rs             # actix routes
│       └── bin/datapress-duckdb.rs # entrypoint binary
├── datafusion/                     # datapress-datafusion
│   └── src/
│       ├── lib.rs                  # pub async fn serve(cfg) -> io::Result<()>
│       ├── store.rs                # Store: RecordBatch + eq-index + reload
│       ├── handlers.rs             # actix routes
│       └── bin/datapress-datafusion.rs
└── python/                         # datapress (Python wheel, cdylib)
    └── src/lib.rs                  # PyO3 bindings — DataPress, DataPressConfig, ...
```

Core re-exports compile without any backend; each backend crate adds the
feature flag it needs on `datapress-core`. The Python crate depends on both
backends, so the wheel can dispatch between them at runtime based on
`DataPressConfig(backend=...)`.

---

## Build flags

```bash
# DuckDB only
cargo build --release -p datapress-duckdb

# DataFusion only
cargo build --release -p datapress-datafusion

# Both Rust binaries
task build

# Python wheel (compiles both backends into one extension)
task py:develop     # editable install into ./.venv (uses uv + maturin)
task py:build       # release wheel into ./target/wheels/
```

Release builds use LTO + `codegen-units = 1` (see `[profile.release]` in
`Cargo.toml`). Expect noticeably longer link times in exchange for tighter
inner loops.

---

## Environment variables

| Variable          | Default          | Purpose                                                                          |
|-------------------|------------------|----------------------------------------------------------------------------------|
| `DATASETS_CONFIG` | `datasets.toml`  | Path to the dataset registry file.                                               |
| `ADMIN_TOKEN`     | *(unset)*        | Enables `POST /api/datasets/{name}/reload`. Unset = admin endpoints disabled.    |
| `DB_POOL_SIZE`    | `num_cpus`       | DuckDB connection pool size (DuckDB only).                                       |
| `RUST_LOG`        | `info`           | Standard `env_logger` filter.                                                    |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | *(unset)* | Fallback S3 credentials used by any dataset that doesn't override them. |
| `AWS_REGION`      | `us-east-1`      | Fallback S3 region.                                                              |
| `${PREFIX}_AWS_*` | *(unset)*        | Per-dataset overrides for the four `AWS_*` vars above. See "Credential precedence" under `[dataset.s3]`. |

Bind address, port, worker count and backend selection live in `[server]`
in `datasets.toml`, not in env vars.

---

## Status / non-goals

- No authentication or rate-limiting on query routes — put this behind your
  own gateway. The `reload` admin route is gated by a shared-secret header
  (`X-Admin-Token`) and disabled unless `ADMIN_TOKEN` is set.
- No write path: parquet sources are read-only. The only mutation is
  reloading a dataset from disk via the admin route.
- No cursor pagination — pagination is plain `OFFSET / LIMIT`, so deep
  pages get expensive (see `H5` in `TEST_Q.md`). `ORDER BY` is supported via
  the `order_by` field, but sorted queries always go through the SQL engine
  (no in-memory fast path).
- DataFusion backend keeps the whole dataset in memory. DuckDB does not.
