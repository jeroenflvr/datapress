---
description: >-
  Configure query-kind datasets in DataPress: SQL-over-registered-datasets
  materialized at startup, with dependency ordering, interval refresh,
  cascade, and optional storage-backed lazy residency.
---

# Materialized datasets

A *materialized dataset* has `kind = "query"` in its `[dataset.source]` block.
Instead of reading parquet or Delta files, DataPress runs your SQL against
other registered datasets, collects the result into memory (or writes it to a
storage backend), and serves it exactly like a file-backed dataset.

- Joins, aggregations, window functions — any `SELECT` the engine supports.
- Automatic dependency ordering at boot: dependencies are built before
  dependents, and independent datasets build concurrently.
- Non-blocking startup: the HTTP listener is ready immediately; every
  dataset build runs in the background.
- Interval-based scheduled refresh and upstream-triggered cascade.
- Optional storage-backed lazy residency for results that are too large
  for RAM.

---

## Minimal example

```toml
[[dataset]]
name = "accidents"

  [dataset.source]
  kind     = "parquet"
  location = "data/accidents.parquet"

[[dataset]]
name = "state_daily_severity"

  [dataset.source]
  kind       = "query"
  sql        = """
    SELECT state, CAST(start_time AS DATE) AS day, AVG(severity) AS avg_sev
    FROM accidents
    GROUP BY state, day
  """
  depends_on = ["accidents"]   # required: must list every dataset the SQL references

on_start = "eager"             # "eager" (default) | "lazy" | "skip"
```

The `state_daily_severity` dataset is built from the `accidents` dataset.
On `/query` and `/count` it behaves identically to a parquet dataset —
no query-path branching.

---

## `[dataset.source]` — `kind = "query"`

| Field        | Required | Default | Notes |
|--------------|----------|---------|-------|
| `kind`       | yes      | —       | Must be `"query"`. |
| `sql`        | yes      | —       | A single read-only `SELECT` (or `WITH … SELECT`). The same denylist as `POST /api/v1/sql` applies: no file functions, no DDL, no DML. |
| `depends_on` | yes      | —       | Array of dataset names referenced by the SQL. **Exact-match both ways**: every name in `depends_on` must appear in the SQL, and every table reference in the SQL must be listed. |

`depends_on` is validated at startup: unknown names, missing names, and
self-references are all startup errors.

### Cross-dataset joins

A `query` dataset may reference any combination of registered datasets —
including other `query` datasets (chained materialization). The SQL is
validated with the same read-only validator as `POST /api/v1/sql`.

```toml
[[dataset]]
name = "regions"

  [dataset.source]
  kind     = "parquet"
  location = "data/regions.parquet"

[[dataset]]
name = "accidents_enriched"

  [dataset.source]
  kind       = "query"
  sql        = "SELECT a.*, r.timezone FROM accidents a JOIN regions r ON a.state = r.state"
  depends_on = ["accidents", "regions"]
```

---

## `on_start` — startup policy

`on_start` is a top-level field on `[[dataset]]` (not inside `[dataset.source]`).
It controls how the dataset is treated during startup.

| Value   | Behaviour |
|---------|-----------|
| `eager` | *(default)* Built in the background at boot. `/readyz` waits for it. |
| `lazy`  | Registered as `pending`; built on the first incoming query. The triggering request waits for the build to finish. Not gated by `/readyz`. |
| `skip`  | Registered as `pending`; built only when `POST /api/v1/datasets/{name}/reload` is called. Not gated by `/readyz`. Queries return `503` with `Retry-After: 2` until it is built. |

`on_start` is valid on any dataset kind (parquet, delta, query).

---

## Non-blocking startup

The HTTP listener binds and serves *before* any dataset build begins.
All datasets start in `pending` state; `eager` datasets are then built in
background tasks.

A request to a `pending` or `building` dataset returns:

```
HTTP 503 Service Unavailable
Retry-After: 2
Content-Type: application/json

{"error":"dataset 'state_daily_severity' is not yet ready","state":"building"}
```

Gate your orchestrator (Kubernetes, ECS, HAProxy health check) on `/readyz`
rather than a single `/query` call.

### `/readyz` behaviour

`/readyz` semantics are controlled by `[server.startup] readiness`:

| Mode  | When `/readyz` returns `200` |
|-------|------------------------------|
| `all` | *(default)* Every `eager` dataset has published successfully. A failed `eager` dataset keeps `/readyz` at `503`. |
| `any` | At least one `eager` dataset has published. |

`lazy` and `skip` datasets never gate readiness — they start in `pending`
without blocking the ready signal.

---

## `[server.startup]` — startup concurrency

```toml
[server.startup]
max_concurrent = 4        # independent datasets built concurrently; default 4
readiness      = "all"    # "all" | "any"; default "all"
```

| Field           | Default | Notes |
|-----------------|---------|-------|
| `max_concurrent` | `4`    | Maximum datasets built in parallel during startup. Dependencies are always ordered before dependents regardless of this cap. |
| `readiness`     | `"all"` | See above. |

---

## `[dataset.refresh]` — scheduled refresh

Add a `[dataset.refresh]` block to schedule periodic re-materialization.
Only valid on `kind = "query"` datasets; setting it on a file-backed dataset
is a startup error.

```toml
[[dataset]]
name = "state_daily_severity"

  [dataset.source]
  kind       = "query"
  sql        = "..."
  depends_on = ["accidents"]

  [dataset.refresh]
  interval           = "15m"    # humantime: "30s", "15m", "2h", "1d"
  on_upstream_reload = true     # re-build when "accidents" publishes
  timeout            = "10m"    # build timeout; default "10m"
  jitter             = true     # ±10% uniform jitter; default true
  debounce           = "5s"     # cascade debounce window; default "5s"
```

| Field               | Default  | Notes |
|---------------------|----------|-------|
| `interval`          | *(unset)*| Polling interval. Absent = no scheduled refresh. |
| `on_upstream_reload`| `false`  | Re-build when any dependency publishes a new generation. |
| `timeout`           | `"10m"`  | Per-build timeout. The build is cancelled on expiry; the previous generation stays live. |
| `jitter`            | `true`   | Apply ±10% uniform jitter to every scheduled fire to avoid thundering herds. |
| `debounce`          | `"5s"`   | When `on_upstream_reload = true`, multiple upstream publishes within this window coalesce into one refresh. |

### Concurrency rules

- Scheduled refresh and manual `POST .../reload` share the same per-dataset
  mutex. A concurrent manual reload coalesces the scheduled tick: the tick is
  skipped and the next fire is scheduled from `now + interval`.
- The global semaphore in `[server.refresh]` governs how many dataset builds
  may run simultaneously across all scheduled refreshes.

```toml
[server.refresh]
max_concurrent = 1    # default 1
```

### Failure backoff

On consecutive build failures, the scheduler backs off exponentially:
`base_interval × 2^min(failures, 3)`, capped at `8 × base_interval`.
The counter resets on success or on a coalesced-tick.

---

## Snapshot consistency

Every materialization captures a snapshot of each dependency's published
state **once, before planning**. Mid-build upstreams do not bleed into the
result:

- **DataFusion**: one `Arc` clone of each dependency's `DatasetState` is
  taken before the query is planned. Readers of the old generation keep their
  captured arcs; the new generation is published atomically via ArcSwap.
- **DuckDB**: the `CREATE OR REPLACE TABLE … AS SELECT` runs inside a single
  transaction; DuckDB's MVCC provides a consistent snapshot.

**Keep-last-good**: a failed build never publishes. The previously published
generation stays live and queryable.

---

## `[dataset.materialize]` — residency

By default, materialized results live in memory. Use `[dataset.materialize]`
to control this.

```toml
[dataset.materialize]
residency      = "auto"      # "auto" (default) | "memory" | "lazy"
sort_by        = ["state"]   # ORDER BY applied when writing parquet files
reuse_on_start = false       # reuse an existing generation on restart
```

| Field           | Default    | Notes |
|-----------------|------------|-------|
| `residency`     | `"auto"`   | See below. |
| `sort_by`       | `[]`       | Column names. Applied as `ORDER BY` so parquet row-group min/max stats enable effective predicate pruning. |
| `reuse_on_start`| `false`    | On boot, find the newest complete generation whose SQL-hash and schema-hash match current config and register it without rebuilding. Default `false` — predictable freshness. |

### Residency modes

| Mode     | Behaviour |
|----------|-----------|
| `auto`   | *(default)* In memory unless the build crosses `[server.storage] force_lazy_above_mb`; then automatically demoted to storage. With no `[server.storage]` configured, behaves as `memory` with a startup WARN. |
| `memory` | Always in RAM. Crossing the threshold logs a WARN but does not demote. |
| `lazy`   | Always written to the storage backend; served lazily from parquet. Requires `[server.storage]`. |

`lazy` residency disables the DataFusion equality index: combining an
explicit `[dataset.index]` block with `residency = "lazy"` is a startup
error. The auto-index (`mode = "auto"`) is silently skipped for demoted
generations, with a WARN logged.

---

## `[server.storage]` — storage backend

Configure a server-level storage backend to enable `lazy` residency or
auto-demotion for `query` datasets.

```toml
[server.storage]
backend            = "local"                            # "local" | "s3"
root               = "/var/lib/datapress/materialized"  # local path or "s3://bucket/prefix"
force_lazy_above_mb = 512                               # auto-demotion threshold; default 512
# materialization_memory_mb = 1024                       # build-time sort/aggregate pool (MiB)
# materialization_sort_spill_reservation_mb = 64         # merge reservation (MiB); « the pool
```

### Local storage

```toml
[server.storage]
backend = "local"
root    = "/var/lib/datapress/materialized"
force_lazy_above_mb = 512
```

### S3 / S3-compatible storage

```toml
[server.storage]
backend = "s3"
root    = "s3://my-bucket/datapress"

  [server.storage.s3]
  region              = "eu-west-1"
  endpoint            = "http://localhost:9000"    # omit for AWS; MinIO / R2 / etc.
  access_key_id_env   = "DATAPRESS_STORAGE_KEY_ID" # env-var NAME, not the value
  secret_access_key_env = "DATAPRESS_STORAGE_SECRET"
  addressing_style    = "path"    # "virtual" (default) | "path" (MinIO)
  allow_http          = true      # required for http:// endpoints
```

| Field                  | Default     | Notes |
|------------------------|-------------|-------|
| `backend`              | `"local"`   | `"local"` or `"s3"`. |
| `root`                 | *(required)*| Local path or `s3://bucket/prefix`. |
| `force_lazy_above_mb`  | `512`       | Auto-demotion threshold in MiB. `0` disables auto-demotion (only explicit `lazy` datasets use storage). |
| `materialization_memory_mb` | *(unset)* | Explicit memory budget (MiB) for the pool used while **building** a `query` dataset (the external sort / hash-aggregate). Set this when a build fails with *"Not enough memory to continue external sort"*. Unset = derive from `force_lazy_above_mb`. Maps to `datafusion.runtime.memory_limit`. |
| `materialization_sort_spill_reservation_mb` | *(unset)* | Reservation (MiB) held back for the external-sort **merge** phase. **Must be strictly smaller than `materialization_memory_mb`** — it is headroom carved out of the build pool, not a separate budget; setting it ≥ the pool is a startup error. Unset = auto `min(pool / 4, 10 MiB)`. Maps to `datafusion.execution.sort_spill_reservation_bytes`. |
| `s3.region`            | *(unset)*   | AWS region. Omit to use `AWS_REGION` or the SDK default. |
| `s3.endpoint`          | *(unset)*   | Custom endpoint (MinIO, R2, Wasabi…). |
| `s3.access_key_id_env` | *(unset)*   | Name of the env var holding the access key ID. Both env vars must be set together, or both omitted (provider chain). |
| `s3.secret_access_key_env` | *(unset)* | Name of the env var holding the secret key. |
| `s3.addressing_style`  | `"virtual"` | `"virtual"` or `"path"`. MinIO requires `"path"`. |
| `s3.allow_http`        | `false`     | Required for `http://` endpoints. |

!!! warning "Inline credentials are rejected"
    Inline secret values directly in TOML (e.g. `access_key_id = "AKI…"`) are a
    startup error in `[server.storage.s3]`. Use env-var indirection only.

!!! tip "Tuning build memory for large sorts"
    A `query` dataset that sorts millions of rows (an `ORDER BY` or a
    `[dataset.materialize] sort_by`) can fail with *"Not enough memory to
    continue external sort … Failed to allocate N MB for ExternalSorterMerge"*.
    Raise `materialization_memory_mb` to give the build a bigger pool. Only set
    `materialization_sort_spill_reservation_mb` if you need to override the
    automatic reservation — and keep it a small fraction of the pool. Setting
    the reservation equal to (or larger than) the pool consumes the entire
    budget and every build fails, so it is rejected at startup.

### Generation layout

Each published generation is written to:

```
<root>/<dataset-name>/<generation-ulid>/data-*.parquet
<root>/<dataset-name>/<generation-ulid>/manifest.json
```

`manifest.json` is written **last** — a generation without a manifest is
treated as incomplete and GC'd at boot. The manifest records the SQL hash,
schema hash, row count, byte size, file list, and creation timestamp.

**N-2 retention**: on each publish, all generations older than the previous
one are deleted. In-flight readers of the prior generation keep their file
handles until they finish; the swap is atomic at the index level (ArcSwap /
view replace), not at the filesystem level.

`reuse_on_start = true` checks the sql-hash and schema-hash in the most
recent complete manifest against the current config before deciding whether
to skip a rebuild.

---

## Cascade refresh

When `on_upstream_reload = true`, a new generation in any upstream dependency
triggers a refresh of this dataset. The cascade follows topological order
across the DAG: a dataset is not refreshed until all its dependencies in the
same wave have published.

A diamond (`d` depends on `b` and `c`, both depending on `a`) refreshes `d`
**exactly once** per settled wave, governed by the `debounce` window.

Cascade refreshes use the same Phase 3 scheduler machinery (global semaphore,
per-dataset mutex, timeout, coalescing). A cascade triggered by a **failed**
upstream build does not fire — nothing was published.

---

## Schema and query paths

A `query` dataset exposes the same routes as a file-backed dataset:

| Route | Notes |
|-------|-------|
| `GET /api/v1/datasets/{name}/schema` | Schema inferred from the materialized result; one sample row. |
| `POST /api/v1/datasets/{name}/query` | Filter / project / sort / paginate. |
| `POST /api/v1/datasets/{name}/count` | Total or filtered row count. |
| `POST /api/v1/datasets/{name}/reload` | Re-execute the SQL and publish (manual refresh). |
| `GET /api/v1/datasets/{name}/status` | State, residency, refresh observability fields. |

The equality index (DataFusion) is built from the materialized result, so
point-lookup predicates work exactly as on a parquet dataset — unless
`residency = "lazy"`, which has no eq-index (see above).

---

## Complete config reference

All new keys introduced by materialized datasets:

```toml
# ---------- per dataset ----------

[[dataset]]
name    = "my_query_dataset"
on_start = "eager"          # "eager" | "lazy" | "skip"; default "eager"

  [dataset.source]
  kind       = "query"
  sql        = "SELECT ..."
  depends_on = ["dep1", "dep2"]

  [dataset.refresh]
  interval           = "15m"   # absent = no scheduled refresh
  on_upstream_reload = false   # default false
  timeout            = "10m"   # default "10m"
  jitter             = true    # default true
  debounce           = "5s"    # default "5s"

  [dataset.materialize]
  residency      = "auto"   # "auto" | "memory" | "lazy"; default "auto"
  sort_by        = []        # default []
  reuse_on_start = false     # default false

# ---------- server ----------

[server.startup]
max_concurrent = 4     # default 4
readiness      = "all" # "all" | "any"; default "all"

[server.refresh]
max_concurrent = 1     # default 1

[server.storage]
backend             = "local"   # "local" | "s3"
root                = "/var/lib/datapress/materialized"
force_lazy_above_mb = 512       # default 512
# materialization_memory_mb = 1024               # build-time sort/aggregate pool (MiB)
# materialization_sort_spill_reservation_mb = 64 # merge reservation (MiB); « the pool

  [server.storage.s3]           # required iff backend = "s3"
  region                = "us-east-1"
  endpoint              = ""
  access_key_id_env     = "DATAPRESS_STORAGE_KEY_ID"
  secret_access_key_env = "DATAPRESS_STORAGE_SECRET"
  addressing_style      = "virtual"  # "virtual" | "path"
  allow_http            = false
```
