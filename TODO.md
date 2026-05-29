# TODO

Scope reminder: the service stays **read-only**. No append / upsert /
delete / replace endpoints. Dataset reload via "load new, switch pointer,
drop old" is fine since it preserves read-only semantics from the
client's perspective.

## Recently closed (kept here for one release as a changelog hint)

- Versioned API prefix `/v1/...` (+ legacy `/api/...` alias).
- OpenAPI / Swagger UI at `/docs` (feature-gated, default on).
- MkDocs Material site embedded at `/mkdocs` (feature-gated, default on).
- Graceful shutdown on SIGINT / SIGTERM with configurable grace period.
- Reverse-proxy path prefix (`server.prefix`).
- Dataset reload endpoint (`POST .../reload`, admin-token guarded).
- Request timeout, max body size, response compression toggle.
- Per-request access log (method, path, status, bytes, ms).
- Arrow IPC stream response (content-negotiated via `Accept` or
  `?format=arrow`).
- Python config surface: backend, listen address, port, workers,
  prefix, compression, body/timeout caps, datasets list, index policy.
- Wheel matrix in CI: linux + macos + windows (manylinux via maturin).
- `/schema` enriched with `rows` + `indexed` (column list).
- OIDC / OAuth2 bearer auth (feature-gated `--features auth`): JWKS
  cache w/ background refresh, scope + tenant claim enforcement,
  Swagger UI SSO via OpenID Connect, admin-token kept as fallback.
- S3 / S3-compatible object storage source (`s3://`, custom endpoint
  for MinIO / R2 / Wasabi, virtual/path addressing, env credential
  chain).
- Delta Lake source kind (`kind = "delta"`).
- Multi-file / glob parquet datasets (`data/*.parquet`, folder of
  parquets) + optional `lazy` on-disk streaming on the DataFusion
  backend.
- Hive-style partitioned datasets (`city=NYC/part.parquet`): partition
  keys are folded into the schema and are queryable on both backends
  (DuckDB natively; DataFusion via constant columns in eager mode and
  `ListingTable` partition columns in `lazy` mode).

---

## Pre-existing items

- `pydantic_settings` on the Python side (load `DataPressConfig` from
  env vars / `.env`).
- Python config surface: still missing — dataset *location* helper
  (currently every dataset must be specified explicitly; a "point at
  this folder, auto-register every parquet" shortcut would be nice).

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

- `/schema` further enrichment: per-column min/max, null-count,
  distinct-count estimate. (Row count + indexed columns are now in.)
- `/metrics` (Prometheus).

### Backends / formats

- NDJSON / Arrow *file* ingestion exposed at config level (Parquet and
  Delta are already in; DataFusion supports the rest).
- Remote storage: GCS / Azure Blob. (S3 / S3-compatible is done.)
- More response export formats: CSV, NDJSON, Parquet
  (in addition to JSON + Arrow IPC).

### Auth & multi-tenant

- Authentication: integration test against a fake JWKS issuer
  (currently covered by unit tests on scope/tenant/parse helpers only).
- Per-operation security requirements in the OpenAPI spec (scopes are
  enforced server-side; surfacing them per-route in the spec would let
  Swagger UI request only the minimum needed scopes).
- Per-dataset ACLs and/or row-level filters.
- Rate limiting / per-client quotas.
- CORS configuration (`actix-cors`).
- TLS termination (optional; reverse proxy stays the default).

---

## Non-functional gaps

### Reliability / ops

- Max predicate count, max JSON depth — DoS hardening beyond the
  existing body-size cap.
- Per-query memory cap / row-scan cap (beyond `page_size`).
- Backpressure / connection-cap config.
- Query result cache; ETag / `If-None-Match` support.

### Observability

- Structured logging config (level, JSON vs text).
- Tracing (`tracing` + OTLP).

### Testing & CI

- Done: handler integration tests, DuckDB end-to-end with the full
  predicate matrix, Arrow IPC round-trip, and `.github/workflows/ci.yml`
  (clippy + workspace test, `docs,swagger` features on). `cargo fmt
  --check` still left out because the codebase relies on hand-aligned
  formatting that conflicts with rustfmt's default rules — pick this
  up when a `rustfmt.toml` is in.
- Criterion benchmarks committed so perf claims are reproducible.
- `cargo audit` + semver checks in CI.
- Fuzz target for the DSL parser.

### DuckDB GROUP BY: `ORDER BY <alias>` rejection (regression risk)

The DuckDB JSON path emits `SELECT json_object('city', "city", 'total',
SUM("score"), …) FROM … GROUP BY "city" ORDER BY "total"` — but
`"total"` is only a key inside `json_object`, never exposed to the
outer SQL scope, so DuckDB rejects the `ORDER BY`. Either:

- wrap the projection in a subquery so the aliases are visible to
  `ORDER BY`, or
- rewrite `ORDER BY <alias>` to the corresponding aggregation
  expression at plan time.

The integration test
`crates/duckdb/tests/end_to_end.rs::group_by_with_default_count_and_named_aggs`
currently asserts the aggregation values without an `ORDER BY` to
work around this.

### Python wrapper polish

- `.pyi` type stubs for IDE autocomplete / mypy.
- Pure-Python client class (`DataPressClient`) with typed
  `query()` / `count()` returning a pyarrow Table; saves users from
  hand-rolling `requests` + dicts.
- Wheel matrix on PyPI: musllinux still missing (manylinux + macOS
  arm64/x86_64 + windows are wired in `.github/workflows/publish.yml`).

### Docs / API contract

- `CHANGELOG.md` discipline. (OpenAPI spec + versioned `/v1` prefix
  are now in — see "Recently closed".)

### Security

- Fuzz pass on weird column names and predicate values to confirm no
  SQL escape is possible.
- Secret-handling story for future remote-storage credentials.

## Audit findings (2025)

Recorded from a code review of the workspace. `cargo clippy
--workspace --all-targets --all-features` is clean; these are
findings clippy does not catch. Ordered by severity.

### Correctness — bugs

- ~~**JWKS URL is hardcoded, breaks standard OIDC discovery.**~~
  *(Fixed.)* `JwksCache` now runs OIDC discovery against
  `{issuer}/.well-known/openid-configuration` and uses the advertised
  `jwks_uri` (caching it), with a one-shot fallback to the legacy
  `{issuer}/.well-known/jwks.json` path only when discovery is
  unreachable. Works with Keycloak / Azure AD / Auth0 / Okta out of the
  box. See `crates/core/src/auth.rs`.

### Security — hardening

- **Internal error messages leak to clients.**
  `crates/core/src/errors.rs` renders `AppError::Internal(_)` straight
  into the HTTP body via `json!({ "error": self.to_string() })`. The
  `Internal` variant wraps raw `duckdb` / `datafusion` / `arrow` /
  `parquet` error strings, which can disclose SQL fragments, column
  names, local file paths, and S3 URLs to unauthenticated callers.
  Fix: log the detail (already done) but return a generic
  `"internal error"` body for the 500 case.

- **DataFusion predicate path string-interpolates literals.**
  `crates/datafusion/src/store.rs::json_to_sql_lit` builds WHERE-clause
  values by inlining them into the SQL text (escaping `'` → `''`),
  whereas the DuckDB backend (`crates/duckdb/src/repository.rs`) uses
  real bound parameters (`params_from_iter`). The escaping looks
  correct for the standard SQL dialect DataFusion uses, but it is a
  lower-assurance approach and the two backends are inconsistent.
  This is the concrete target for the existing "fuzz pass" item above;
  prefer DataFusion's prepared-statement / `LogicalPlanBuilder` API so
  values never touch the SQL string.

- **No cap on predicate count or `in`-list length.**
  Request bodies are bounded by `server.max_body_bytes` (default 1 MiB)
  but nothing caps the number of predicates or the size of an `in`
  array within that budget. A single request can therefore force a
  very large `WHERE … AND … AND …` / `IN (…)` against a lazy dataset.
  Consider an explicit per-request predicate/in-list limit.

### Process

- **No dependency vulnerability scanning.** `cargo audit` is not
  installed locally and is not run in CI. Add `cargo install
  cargo-audit` + a `cargo audit` (and ideally `cargo deny`) step to
  `.github/workflows/ci.yml` so advisory regressions are caught. (This
  overlaps the existing "cargo audit + semver checks in CI" item under
  Testing & CI.)

