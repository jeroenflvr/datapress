# TODO

Scope reminder: the service stays **read-only**. No append / upsert /
delete / replace endpoints. Dataset reload via "load new, switch pointer,
drop old" is fine since it preserves read-only semantics from the
client's perspective.

## Pre-existing items

- pydantic_settings on the Python side.
- Add endpoint to reload data (load data into new area, switch pointer,
  delete old area). Requires the dataset to fit twice in memory.
- Add API versioning `/api/v1/...` and mirror that in the project
  structure (`handlers/v1`).
- Allow running behind a reverse proxy with a path prefix
  (e.g. `/fast-api` → `/fast-api/api/datasets/{name}/query`).
- Python config surface:
  - number of workers (all cores if `None`)
  - dataset location
  - duckdb vs datafusion
  - index mode `auto` or explicit list (+ define list)
  - port
  - listen address (`127.0.0.1` by default; don't expose by default)

---

## Functional gaps

### Query DSL

- `having` clause for filtering aggregates.
- Extra predicate ops: `between`, `not_in`, `regex` / `~`,
  `starts_with`, `ends_with`.
- `NULLS FIRST` / `NULLS LAST` on `order_by`.
- Richer aggregations: `median`, `stddev`, `var`, percentiles,
  `string_agg`, `first` / `last`.
- Window functions.
- Raw-SQL escape hatch endpoint (`POST /sql`) — opt-in via config, off
  by default. Stays read-only (reject anything that's not `SELECT` /
  `WITH`).
- Joins across datasets, CTEs / subqueries (likely via the raw-SQL
  endpoint rather than DSL).

### Endpoints / metadata

- Dataset reload endpoint (see pre-existing item) — still read-only.
- `/schema` enriched with: row count, per-column min/max,
  null-count, distinct-count estimate, which indices exist.
- `/healthz` and `/readyz`.
- `/metrics` (Prometheus).
- `/version` / build-info endpoint.

### Backends / formats

- DuckDB `query_arrow` implementation (currently inherits the default
  `400`; clients silently fall back to JSON).
- Native Parquet / NDJSON / Arrow file ingestion exposed at config
  level (DataFusion supports them already).
- Remote storage: S3 / GCS / Azure Blob.
- Partitioned / multi-file datasets (folder of parquets, hive-style).
- More response export formats: CSV, NDJSON, Parquet
  (in addition to JSON + Arrow IPC).

### Auth & multi-tenant

- Authentication: API keys and/or JWT bearer.
- Per-dataset ACLs and/or row-level filters.
- Rate limiting / per-client quotas.
- CORS configuration (`actix-cors`).
- TLS termination (optional; reverse proxy stays the default).

---

## Non-functional gaps

### Reliability / ops

- Request timeout, max body size, max predicate count,
  max JSON depth — DoS hardening.
- Per-query memory cap / row-scan cap (beyond `page_size`).
- Graceful shutdown on SIGTERM.
- Backpressure / connection-cap config.
- Query result cache; ETag / `If-None-Match` support.

### Observability

- Structured logging config (level, JSON vs text).
- Tracing (`tracing` + OTLP).
- Per-request access log line: method, path, dataset, ms, rows, bytes.

### Testing & CI

- Integration tests for handlers, the full predicate matrix, Arrow IPC
  round-trip, error paths.
- Criterion benchmarks committed so perf claims are reproducible.
- CI matrix: `clippy` + `fmt` + `test` + `cargo audit` + semver checks.
- Fuzz target for the DSL parser.

### Python wrapper polish

- `.pyi` type stubs for IDE autocomplete / mypy.
- Pure-Python client class (`DataPressClient`) with typed
  `query()` / `count()` returning a pyarrow Table; saves users from
  hand-rolling `requests` + dicts.
- Wheel matrix on PyPI: manylinux + musllinux + macOS arm64/x86_64
  + windows (maturin + cibuildwheel).

### Docs / API contract

- OpenAPI / JSON-schema spec for the request DSL — keeps clients and
  docs in sync automatically.
- Versioned API prefix `/v1/...` (see pre-existing item).
- `CHANGELOG.md` discipline.

### Security

- Fuzz pass on weird column names and predicate values to confirm no
  SQL escape is possible.
- Secret-handling story for future remote-storage credentials.
