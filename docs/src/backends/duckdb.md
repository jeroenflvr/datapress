# DuckDB

**Crate:** `crates/duckdb` &nbsp;·&nbsp; **Binary:** `datapress-duckdb`

DataPress wraps the bundled [DuckDB](https://duckdb.org/) library
(`libduckdb-sys`) and exposes its query engine over the standard
HTTP API.

## Highlights

- **Battle-tested SQL.** Full SQL surface, mature optimiser, robust
  type coercion, well-understood NULL semantics.
- **Lazy parquet reads everywhere.** Both local files and S3 URLs are
  scanned on demand via DuckDB's parquet reader. No materialisation
  step at startup — the server is up and serving within milliseconds.
- **httpfs + delta.** DuckDB autoloads `httpfs` and `delta` extensions
  when the dataset URL requires them.
- **Arrow IPC.** Paged `/query?format=arrow` responses and full
  `/query/stream` exports write DuckDB's native `query_arrow` batches
  into the HTTP response stream; no JSON round-trip on the server side.
- **Experimental Quack server.** Opt into DuckDB's Quack remote protocol
  with `[server.quack]` to let DuckDB clients attach to the same in-process
  database over `quack:localhost`.
- **Transactional reload.** Dataset reload uses DuckDB's ACID transaction
  path (`CREATE OR REPLACE TABLE ... AS SELECT ...`), so failed reloads
  leave the existing table live. See [Operations › Dataset reload](../operations/reload.md).
- **Small binary.** No DataFusion plan trees, no in-memory chunk
  store, no equality index — just DuckDB.

## Trade-offs

- No equality index. Every `eq` / `in` predicate runs through DuckDB's
  SQL optimiser. That's still fast (zone maps, parallel hash join),
  but the in-memory `O(1)` row-id lookup the DataFusion backend offers
  is not available here.
- `[dataset.index]` is ignored. The DataFusion-specific block in
  `datasets.toml` doesn't apply.
- `lazy = true` is meaningful but redundant — DuckDB always reads
  parquet on demand.

## S3 reads

For S3 parquet and Delta datasets, DataPress loads DuckDB's `httpfs`
extension and creates a temporary DuckDB S3 secret for each dataset.
The secret is scoped to the dataset bucket, for example `s3://bucket`.
That scope reliably matches single files, globs, partitioned paths, and
reloads without leaking credentials across buckets.

When no inline `access_key_id` / `secret_access_key` pair is configured,
DataPress asks DuckDB to use the AWS environment/profile chain
(`env;config`). This avoids accidentally probing instance metadata from
local or S3-compatible deployments, which can otherwise show up as a
confusing `503` from `read_parquet`.

For S3-compatible endpoints such as MinIO, R2, or Wasabi, set
`endpoint`, `addressing_style`, and `allow_http` the same way as the
DataFusion backend:

DataPress config uses `addressing_style = "virtual" | "path"`. For DuckDB,
DataPress translates `virtual` to DuckDB's `URL_STYLE 'vhost'` and `path`
to `URL_STYLE 'path'`.

```toml
[[dataset]]
name = "warehouse"
source.kind = "parquet"
source.location = "s3://warehouse/exports/*.parquet"

[dataset.s3]
region = "us-east-1"
endpoint = "http://minio.local:9000"
addressing_style = "path"
allow_http = true
```

## When to pick DuckDB

- You want **SQL semantics you trust** and rich type coverage.
- You need **fast startup** on huge datasets — no full scan at boot.
- You query datasets that **don't fit in RAM**.
- You want DuckDB-native clients, such as the DuckDB CLI, to attach to
  the running DataPress process via Quack.
- You don't need sub-millisecond point lookups on indexed columns.

## Quack remote protocol

Quack is DuckDB's experimental remote protocol. DataPress starts it only
when explicitly configured:

```toml
[server]
backend = "duckdb"

[server.quack]
enabled = true
uri = "quack:localhost"
token = "analytics-token"
read_only = true
```

The Quack server starts after DataPress registers datasets, so remote
clients can query the same tables as the HTTP API. By default DataPress
keeps Quack on localhost and installs a read-only authorization hook.
For non-local exposure, set `allow_other_hostname = true` and place a
TLS-terminating reverse proxy in front of the Quack port.

DuckDB CLI example:

```sql
INSTALL quack;
LOAD quack;

ATTACH 'quack:localhost' AS datapress (TOKEN 'analytics-token');
FROM datapress.accidents LIMIT 10;
```

For any host other than `localhost`, Quack defaults to **HTTPS**. When the
server is reached over plain HTTP (development, or before a TLS proxy is in
place), add `DISABLE_SSL true`:

```sql
ATTACH 'quack:remote_ip' AS remote_db (TOKEN 'analytics-token', DISABLE_SSL true);
FROM remote_db.accidents LIMIT 10;
```

## When to skip DuckDB

- You need sub-millisecond `eq` / `in` lookups on indexed columns.
- You want zero-copy Arrow access into the resident chunks from
  in-process Rust (DataFusion backend uses native `RecordBatch`).
