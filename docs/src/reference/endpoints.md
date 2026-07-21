# Endpoints

Every route mounted by the server. All paths are relative to the configured
`server.prefix` (empty by default). When `prefix = "/dp"`, add `/dp`
before every path shown here.

## Versioned API (`{prefix}/api/v1`)

| Method | Path                                              | Body            | Purpose                                                              |
|--------|---------------------------------------------------|-----------------|----------------------------------------------------------------------|
| GET    | `/api/v1/datasets`                                | —               | List all datasets with full status entries (state, kind, refresh fields). |
| POST   | `/api/v1/datasets`                                | [Dataset config](../configuration/datasets.md) | Register a dataset at runtime. Requires `X-Admin-Token`. |
| POST   | `/api/v1/datasets/persist`                        | [Dataset config](../configuration/datasets.md) | Append a dataset to the on-disk config. Requires `X-Admin-Token`. |
| GET    | `/api/v1/datasets/{name}/schema`                  | —               | Inferred schema + one sample row.                                    |
| GET    | `/api/v1/datasets/{name}/status`                  | —               | Full status for one dataset: state, kind, residency, refresh observability fields. |
| POST   | `/api/v1/datasets/{name}/query`                   | [Query body](../query/request-body.md) | Filter / project / sort / paginate. Responds with `X-Dataset-Refreshed-At` header. |
| POST   | `/api/v1/sql`                                     | [SQL body](../query/sql.md) | Raw read-only SQL over one dataset. Off unless `[sql].enabled`. |
| POST   | `/api/v1/datasets/{name}/query/stream`            | [Arrow IPC](../query/arrow-ipc.md) | Stream all matching rows as Arrow IPC.             |
| POST   | `/api/v1/datasets/{name}/count`                   | `{ predicates? }` | Total or filtered row count. Responds with `X-Dataset-Refreshed-At` header. |
| GET    | `/api/v1/datasets/{name}/parquet`                 | —               | Whole dataset as a Parquet file (HTTP range + `HEAD`).               |
| GET    | `/api/v1/datasets/{name}/all.parquet`             | —               | Alias of `/parquet` whose URL ends in `.parquet` (bare `FROM '…'`).  |
| POST   | `/api/v1/datasets/{name}/reload`                  | —               | Atomic dataset reload. Requires `X-Admin-Token`.                     |
| POST   | `/api/v1/datasets/reload-all`                     | —               | Enqueue every reloadable dataset in topological order. `202` with `{"enqueued":[...],"skipped":[...]}`. Requires `X-Admin-Token`. |
| POST   | `/api/v1/config/reload`                           | —               | Re-read `datasets.toml`; register newly-added datasets. Requires `X-Admin-Token`. |
| POST   | `/api/v1/queries`                                 | [Create query body](../operations/saved-queries.md) | Create a runtime dataset (`temp` or persisted `query`). Requires `datasets:manage` / `X-Admin-Token`. |
| GET    | `/api/v1/queries`                                 | —               | List all runtime-created datasets with their definitions and state. Requires `datasets:manage` / `X-Admin-Token`. |
| DELETE | `/api/v1/queries/{name}`                          | —               | Unregister a runtime-created dataset and wipe its storage. `403` for config-file datasets; `409` if dependents exist. Requires `datasets:manage` / `X-Admin-Token`. |
| GET    | `{prefix}/health`                                 | —               | Liveness, prefix-aware.                                              |

## Observability headers

`GET /api/v1/datasets/{name}/query` and `POST /api/v1/datasets/{name}/count`
include the following response header when a publish timestamp is available:

| Header                   | Type      | Description |
|--------------------------|-----------|-------------|
| `X-Dataset-Refreshed-At` | RFC-3339  | Publish timestamp of the current generation. |

## Dataset status fields

`GET /api/v1/datasets` and `GET /api/v1/datasets/{name}/status` return
`DatasetStatusEntry` objects with these fields:

| Field                      | Type             | Description |
|----------------------------|------------------|-------------|
| `name`                     | string           | Dataset identifier. |
| `state`                    | string enum      | `pending`, `building`, `published`, `failed`. |
| `kind`                     | string enum      | `parquet`, `delta`, `query`. |
| `residency`                | string enum      | `memory` or `lazy`. |
| `storage_bytes`            | integer?         | Bytes of the storage-backed generation. `null` for in-memory. |
| `generation_id`            | string?          | ULID of the storage generation. `null` for in-memory. |
| `last_refresh_at`          | RFC-3339?        | Timestamp of the last successful publish. |
| `last_refresh_duration_ms` | integer?         | Build duration of the last publish in ms. |
| `next_refresh_at`          | RFC-3339?        | Next scheduled refresh. `null` for non-scheduled. |
| `refresh_source`           | string enum?     | `startup`, `manual`, `schedule`, `cascade`. |
| `consecutive_failures`     | integer          | Scheduler failures since last success. |
| `last_error`               | string?          | Last build error, truncated to 500 characters. |
| `rows`                     | integer          | Row count (`0` when not yet published). |
| `columns`                  | integer          | Column count (`0` when not yet published). |
| `lazy`                     | boolean          | Whether the current generation is lazy/storage-backed. |
| `depends_on`               | array of strings | Upstream dataset names (`query` kind only). |

## Probes

| Method | Path                 | Code           | Purpose                                       |
|--------|----------------------|----------------|-----------------------------------------------|
| GET    | `{prefix}/healthz`   | `200`          | Liveness; always OK.                          |
| GET    | `{prefix}/readyz`   | `200` / `503`  | Ready once eager datasets have published (configurable via `[server.startup] readiness`). Returns `503` while datasets are still building after a non-blocking boot. |
| GET    | `{prefix}/version`   | `200`          | Build/version metadata.                       |

Full descriptions: [Operations › Probes](../operations/probes.md).

## Documentation (optional)

When built with `--features docs` and `[docs] enabled = true`:

| Method | Path             | Purpose                                            |
|--------|------------------|----------------------------------------------------|
| GET    | `{docs.path}/`   | Embedded MkDocs site root (default `/mkdocs/`).    |
| GET    | `{docs.path}/{*}`| Static assets / inner pages.                       |

See [Configuration › Documentation site](../configuration/docs-site.md).

## Parquet export

`GET /api/v1/datasets/{name}/parquet` encodes the **entire** dataset as a
single self-contained Parquet file and serves it with HTTP range and
`HEAD` support, so external tools can read it straight over HTTP without
downloading the whole file.

The encoded file is cached per dataset and invalidated on
[reload](../operations/reload.md), so the multiple range requests a Parquet
reader issues (a `HEAD` for the size, then ranged `GET`s for the footer and
row-group metadata) all observe identical, stable bytes.

Read it from a DuckDB client with the `httpfs` extension. Use
`read_parquet(...)`, which always works regardless of the URL ending:

```sql
INSTALL httpfs; LOAD httpfs;
SELECT count(*)
FROM read_parquet('http://localhost:8080/api/v1/datasets/accidents/parquet');
-- → 7728394
```

A `count(*)` only fetches the Parquet footer via range requests — not the
whole file. The bare `FROM '…/parquet'` form does **not** auto-detect the
format, because DuckDB sniffs the file type from the URL extension. For the
bare form, use the `.parquet`-suffixed alias instead, which serves the exact
same bytes:

```sql
SELECT count(*)
FROM 'http://localhost:8080/api/v1/datasets/accidents/all.parquet';
-- → 7728394
```

Response headers:

| Header           | Value                                 |
|------------------|---------------------------------------|
| `Content-Type`   | `application/vnd.apache.parquet`      |
| `Accept-Ranges`  | `bytes`                               |
| `Content-Range`  | `bytes {start}-{end}/{total}` (on `206`) |

A satisfiable `Range` request returns `206 Partial Content`; an
out-of-range one returns `416 Range Not Satisfiable`.

## Metrics (optional)

When built with `--features metrics` and `[metrics] enabled = true`:

| Method | Path             | Purpose                                            |
|--------|------------------|----------------------------------------------------|
| GET    | `{metrics.path}` | Prometheus metrics, text format (default `/metrics`). Unprefixed and unauthenticated. |

See [Operations › Prometheus metrics](../operations/metrics.md).

## PostgreSQL wire protocol (optional)

The DataFusion backend, built with `--features pgwire` and enabled under
`[server.pgwire]`, additionally serves datasets over the PostgreSQL wire
protocol on a separate TCP port (default `5432`) — this is **not** an HTTP
route. See [Clients › PostgreSQL](../clients/postgresql.md).

## Admin

`POST .../reload` requires the `ADMIN_TOKEN` environment variable to
be set (otherwise the endpoint returns `403`). The request must carry
the matching token in the `X-Admin-Token` header.

Reload publication is backend-specific: DataFusion uses a service-level
double buffer, while DuckDB uses transactional table replacement inside
DuckDB. See [Operations › Dataset reload](../operations/reload.md).

```bash
curl -s -X POST \
     -H "X-Admin-Token: $ADMIN_TOKEN" \
     http://localhost:8080/api/v1/datasets/accidents/reload | jq
# → { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```
