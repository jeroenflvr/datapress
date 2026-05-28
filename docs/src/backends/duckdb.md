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
- **Arrow IPC.** Responses to `/query?format=arrow` stream via
  DuckDB's native `query_arrow` API; no JSON round-trip on the server
  side.
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

## When to pick DuckDB

- You want **SQL semantics you trust** and rich type coverage.
- You need **fast startup** on huge datasets — no full scan at boot.
- You query datasets that **don't fit in RAM**.
- You don't need sub-millisecond point lookups on indexed columns.

## When to skip DuckDB

- You need sub-millisecond `eq` / `in` lookups on indexed columns.
- You want zero-copy Arrow access into the resident chunks from
  in-process Rust (DataFusion backend uses native `RecordBatch`).
