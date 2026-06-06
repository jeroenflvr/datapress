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

#### DDoS resilience & concurrent-query isolation

Goal: survive request floods *and* keep admitted queries fast (one heavy
query must not starve honest concurrent ones). Two separate problems —
volumetric attacks belong at the edge, app-layer overload belongs
in-process. Prioritized:

1. **Edge first (biggest win, zero app CPU):** document + recommend a
   reverse proxy / CDN (Cloudflare / nginx / Caddy) for TLS, per-IP rate
   limiting, connection caps, SYN-flood protection. Reuses `server.prefix`.
   Volumetric DDoS cannot be solved in-process — don't try to out-CPU a
   flood in Rust.
2. **Concurrency admission limiter (best in-process change):**
   `tokio::sync::Semaphore` around the query + `/sql` handlers, sized by a
   new `server.max_concurrent_queries` knob (0 = unlimited). On saturation
   fail fast with `503` + `Retry-After` instead of piling up heavy work.
   Bounds CPU/RAM contention so each admitted query keeps its perf. Also
   retro-fits DataFusion with the cap DuckDB already gets from its pool
   (`init_pool` in `crates/duckdb/src/db.rs`); DataFusion `collect()`
   currently runs unbounded.
3. **Per-query resource bounds (caps one bad query's blast radius):**
   DataFusion `RuntimeEnv` with a bounded `MemoryPool`
   (`GreedyMemoryPool` / `FairSpillPool`) + sane `target_partitions`;
   DuckDB `PRAGMA memory_limit` per pooled connection (threads already
   split across the pool).
4. **actix connection limits:** expose `max_connections` /
   `max_connection_rate` / backlog on the `HttpServer` builder via config.
5. **Optional in-app per-IP rate limit** (`governor` token bucket) as
   defense-in-depth. Caveat: behind a proxy must trust `X-Forwarded-For`
   correctly or it rate-limits the proxy's single IP — prefer the edge.
6. **Observability:** under the `metrics` feature, add gauges for
   in-flight queries, semaphore queue depth, and `503` rejection count to
   see saturation and tune limits.

Highest value-to-risk first step: #2 + #4 together (no new heavy deps),
defaults preserving current behavior unless configured. (See also the
scattered items above and "Rate limiting / per-client quotas" under
Auth & multi-tenant — fold those in when implementing.)

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
- ~~`cargo audit`~~ + semver checks in CI. *(`cargo audit` added as the
  `audit` job in `.github/workflows/ci.yml`; semver checks still open.)*
- Fuzz target for the DSL parser.

### DuckDB GROUP BY: `ORDER BY <alias>` rejection (regression risk) *(Fixed.)*

The DuckDB JSON path emitted `SELECT json_object('city', "city", 'total',
SUM("score"), …) FROM … GROUP BY "city" ORDER BY "total"` — but
`"total"` was only a key inside `json_object`, never exposed to the
outer SQL scope, so DuckDB rejected the `ORDER BY`.

Fixed by running the aggregation in an inner subquery
(`SELECT "city", SUM("score") AS "total" FROM … GROUP BY "city" ORDER BY
"total" LIMIT … OFFSET …`) so each alias is a real output column visible
to `ORDER BY`, then wrapping it in `json_object` in the outer query
(mirroring the existing `DISTINCT` subquery pattern). The integration
test `crates/duckdb/tests/end_to_end.rs::group_by_with_default_count_and_named_aggs`
now asserts `ORDER BY <alias>` ordering.

### Python wrapper polish

- ~~`.pyi` type stubs for IDE autocomplete / mypy.~~ *(Done — full
  stubs for `datapress` + `client`, kept in sync with the compiled
  classes; `AuthConfig` re-exported at the top level.)*
- ~~Pure-Python client class (`DataPressClient`) with typed
  `query()` / `count()` returning a pyarrow Table.~~ *(Done — see
  `crates/python/python/datap_rs/client.py`.)*
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

- ~~**DataFusion predicate path string-interpolates literals.**~~
  *(Fixed.)* `crates/datafusion/src/store.rs` no longer inlines
  predicate values into the SQL text. The builders emit positional
  placeholders (`$1`, `$2`, …) and collect each value as a typed
  `ScalarValue`; the scalars are bound to the logical plan via
  `DataFrame::with_param_values` before execution, so user input
  reaches the engine as data and can never alter query structure
  (`json_to_sql_lit` replaced by `json_to_scalar` + a `Params`
  accumulator). This brings the DataFusion backend in line with the
  DuckDB backend's bound-parameter approach. Regression tests in
  `crates/datafusion/tests/end_to_end.rs` cover quote-containing
  values, an injection-style literal, `in`-list binding, and numeric
  coercion.

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

