# `datasets.toml` configuration reference

`datapress` is driven by a single TOML file (conventionally `datasets.toml`)
that declares one server block plus any number of `[[dataset]]` blocks.
This document walks through every option and shows complete, copy-pasteable
examples for the most common shapes.

> All snippets here are real TOML — drop them into `datasets.toml` (or
> compose them with the Python `dp.DatasetConfig(...)` API, which mirrors
> the same fields).

## Top-level structure

```toml
[server]                  # optional; defaults shown below
backend = "datafusion"    # or "duckdb"
listen  = "127.0.0.1"
port    = 8080
workers = 0               # 0 / unset → one worker per CPU
prefix  = ""              # e.g. "/datapress" if behind a reverse proxy
compress           = true     # negotiate gzip/brotli/zstd via Accept-Encoding
max_body_bytes     = 1048576  # max JSON request body in bytes (413 above)
max_page_size      = 100000   # max rows returned by one query page
request_timeout_ms = 30000    # per-request timeout in ms; 0 = disabled
shutdown_timeout_secs = 30    # graceful-shutdown grace period, in seconds

[server.quack]                # DuckDB backend only; experimental
enabled = false
uri = "quack:localhost"       # default port 9494; use literal localhost
# token = "change-me"         # optional; generated and logged if omitted
allow_other_hostname = false  # true for quack:0.0.0.0:9494 behind TLS proxy
read_only = true              # allow reads plus Quack attach handshake

[[dataset]]               # one block per dataset
name = "..."
# source, s3, index, lazy follow
```

## `[[dataset]]` fields

| field     | required | default     | notes                                                                                            |
| --------- | -------- | ----------- | ------------------------------------------------------------------------------------------------ |
| `name`    | yes      | —           | URL slug + SQL table name. Must be unique.                                                       |
| `source`  | yes      | —           | Sub-table: `{ kind = "parquet" \| "delta", location = "..." }`.                                  |
| `s3`      | no       | absent      | Only meaningful when `source.location` starts with `s3://`. Non-secret connection details.       |
| `index`   | no       | `mode="auto"` | Equality-index policy. **Important for wide tables — see below.**                              |
| `lazy`    | no       | `false`     | DataFusion + parquet only (local or S3). Skip materialisation; stream row groups at query time. |

## 1. Local parquet — single file

The simplest case. Materialise the file at startup, auto-index every
column up to 100 k distinct values (the default).

```toml
[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/march_2023.parquet"
```

## 2. Local parquet — directory of files

`location` can be a directory; every `*.parquet` underneath is loaded.

```toml
[[dataset]]
name = "events"

[dataset.source]
kind     = "parquet"
location = "data/events/"
```

## 3. Local parquet — glob pattern

Need only a subset of files, or files spread across siblings? Use a glob.
Supported wildcards: `*`, `?`, `[abc]`.

```toml
[[dataset]]
name = "sales_2024"

[dataset.source]
kind     = "parquet"
location = "data/sales/2024/*/*.parquet"
```

## 4. Lazy mode for very large parquet datasets

When the decompressed Arrow size would not fit in RAM (or the index is
too expensive to build), enable `lazy = true`. The DataFusion backend
registers a `ListingTable` against the source and streams row groups at
query time; column-projection pushdown and parquet row-group skipping
happen automatically.

**Trade-off:** higher per-query latency, no equality index. Always pass
explicit `columns=[...]` in queries to get the most out of projection
pushdown.

### Local files

```toml
[[dataset]]
name = "us_accidents"
lazy = true                  # ← skip the in-RAM materialisation

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/*.parquet"
```

### S3 / S3-compatible

Same shape — the `[dataset.s3]` block is honoured exactly as in the eager
S3 path. DataFusion lists objects under the prefix through the registered
object store on demand.

```toml
[[dataset]]
name = "events"
lazy = true

[dataset.source]
kind     = "parquet"
location = "s3://my-bucket/events/2024/"

[dataset.s3]
region = "eu-west-1"
```

> Lazy mode requires `backend = "datafusion"` and `kind = "parquet"`.
> Lazy delta is rejected at startup (deltalake reads the transaction log
> eagerly to know which files are current, so it can't map onto
> `ListingTable` cleanly).

## 5. Delta table — local

```toml
[[dataset]]
name = "orders"

[dataset.source]
kind     = "delta"
location = "data/orders_delta/"
```

## 6. Parquet on S3 (AWS)

Credentials come from the default AWS provider chain (env, instance
profile, `~/.aws/credentials`). Region is auto-detected when omitted.

```toml
[[dataset]]
name = "logs"

[dataset.source]
kind     = "parquet"
location = "s3://my-bucket/logs/2024/"

[dataset.s3]
region = "eu-west-1"
```

## 7. Parquet on MinIO / R2 / Wasabi (custom endpoint)

Non-AWS providers usually need a custom endpoint and path-style
addressing. Plain-HTTP endpoints (local MinIO) require `allow_http = true`.

```toml
[[dataset]]
name = "warehouse"

[dataset.source]
kind     = "parquet"
location = "s3://warehouse/exports/"

[dataset.s3]
region           = "us-east-1"
endpoint         = "http://minio.local:9000"
addressing_style = "path"     # required by MinIO
allow_http       = true
```

## 8. Delta on S3

Same shape as parquet on S3 — just flip the `kind`.

```toml
[[dataset]]
name = "events_delta"

[dataset.source]
kind     = "delta"
location = "s3://my-bucket/events_delta/"

[dataset.s3]
region = "us-east-1"
```

## 9. S3 credentials via inline keys (discouraged)

Prefer env vars. If you must inline, the keys go in `[dataset.s3]`.

```toml
[[dataset]]
name = "scratch"

[dataset.source]
kind     = "parquet"
location = "s3://scratch-bucket/dump/"

[dataset.s3]
region            = "us-east-1"
access_key_id     = "AKIA..."
secret_access_key = "..."
# session_token   = "..."   # optional, for STS creds
```

## 10. S3 credentials via per-dataset env vars

For multi-tenant setups, scope credentials to one dataset by prefixing
the standard AWS env-var names with `${DATASET_NAME_UPPERCASE}_`.
Non-alphanumeric chars in the name become `_`.

For a dataset named `sales.eu-1` (prefix → `SALES_EU_1`):

```bash
export SALES_EU_1_AWS_ACCESS_KEY_ID=AKIA...
export SALES_EU_1_AWS_SECRET_ACCESS_KEY=...
```

```toml
[[dataset]]
name = "sales.eu-1"

[dataset.source]
kind     = "parquet"
location = "s3://eu-sales/"

[dataset.s3]
region = "eu-west-1"
```

Resolution order: per-dataset env → inline keys in `[dataset.s3]` →
plain `AWS_*` env → default provider chain.

---

## Equality index (`[dataset.index]`)

`datapress` builds a per-column equality index at load time. It backs
the O(1) hot path for `eq` / `in` predicates and only applies to
**eager** datasets (it's skipped entirely when `lazy = true`).

| field             | default     | meaning                                                  |
| ----------------- | ----------- | -------------------------------------------------------- |
| `mode`            | `"auto"`    | `"auto"`, `"none"`, or `"list"`.                         |
| `columns`         | `[]`        | Explicit column list. Required for `mode = "list"`.      |
| `max_cardinality` | `100000`    | Auto mode: stop indexing a column once distinct values exceed this. |

### `mode = "auto"` (default)

Indexes every column whose Arrow type is one of `Utf8`, `Boolean`,
`Int8`/`Int16`/`Int32`/`Int64`. Each column is built in parallel and
abandoned if its distinct-value count exceeds `max_cardinality`.

**Warning for wide schemas (≳ 50 columns):** Auto can blow up memory.
The index keys are heap-allocated `String`s and 320 maps building
concurrently easily run into tens of GB. For wide tables, switch to
`mode = "list"` and name the columns you actually filter on.

```toml
[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents.parquet"

[dataset.index]
mode            = "auto"
max_cardinality = 50_000     # tighten the cap if RAM is tight
```

### `mode = "none"` — no index

All predicates go through the DataFusion / DuckDB SQL fallback (still
vectorised + multi-threaded). Use this when:
- the dataset is wide and you don't have a fixed query pattern,
- startup time matters more than first-query latency,
- you mostly filter on ranges / `LIKE` (index doesn't help those anyway).

```toml
[[dataset]]
name = "raw_logs"

[dataset.source]
kind     = "parquet"
location = "data/raw_logs/"

[dataset.index]
mode = "none"
```

### `mode = "list"` — index a hand-picked set

Best for **wide tables with a known query pattern.** Only the listed
columns get an index; `max_cardinality` is ignored.

```toml
[[dataset]]
name = "us_accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/*.parquet"

[dataset.index]
mode    = "list"
columns = ["state", "severity", "weather_condition", "city"]
```

Use this when the dataset has hundreds of columns but realistically you
only ever filter on a handful — the rest pay for themselves through
projection at query time, not through an index.

### Combining `mode = "list"` with an empty `columns` list — invalid

Caught at startup with a clear error:

```text
dataset 'foo': index.mode = "list" requires a non-empty index.columns
```

---

## Full multi-dataset example

```toml
[server]
backend = "datafusion"
listen  = "0.0.0.0"
port    = 8080
prefix  = "/datapress"

# Small reference table — auto-index every column.
[[dataset]]
name = "states"

[dataset.source]
kind     = "parquet"
location = "data/ref/states.parquet"

# Wide fact table — explicit index, glob over monthly partitions.
[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/accidents/2024/*.parquet"

[dataset.index]
mode    = "list"
columns = ["state", "severity"]

# Huge cold dataset — lazy, stream from disk.
[[dataset]]
name = "raw_telemetry"
lazy = true

[dataset.source]
kind     = "parquet"
location = "data/telemetry/*.parquet"

# Delta table on S3.
[[dataset]]
name = "orders"

[dataset.source]
kind     = "delta"
location = "s3://prod-bucket/orders_delta/"

[dataset.s3]
region = "eu-west-1"
```

---

## Same configuration from Python

Every field above has a counterpart on `dp.DatasetConfig`. The runtime
honours both shapes identically.

```python
from datap_rs import datapress as dp

cfg = dp.AppConfig(
    server=dp.ServerConfig(backend="datafusion", port=8080),
    datasets=[
        dp.DatasetConfig(
            name="accidents",
            source="data/accidents/2024/*.parquet",
            format="parquet",
            mode="list",
            index_columns=["state", "severity"],
        ),
        dp.DatasetConfig(
            name="raw_telemetry",
            source="data/telemetry/*.parquet",
            format="parquet",
            lazy=True,
        ),
    ],
)
```
