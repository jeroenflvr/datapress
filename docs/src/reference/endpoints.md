# Endpoints

Every route mounted by the server. The probe endpoints live at the
bare host root regardless of any configured URL prefix; everything
under `/api/v1/` and `/api/` is shifted by `prefix` when set.

## Versioned API (`/api/v1`)

| Method | Path                                              | Body            | Purpose                                                              |
|--------|---------------------------------------------------|-----------------|----------------------------------------------------------------------|
| GET    | `/api/v1/datasets`                                | —               | List configured datasets and metadata.                               |
| GET    | `/api/v1/datasets/{name}/schema`                  | —               | Inferred schema + one sample row.                                    |
| POST   | `/api/v1/datasets/{name}/query`                   | [Query body](../query/request-body.md) | Filter / project / sort / paginate.                |
| POST   | `/api/v1/sql`                                     | [SQL body](../query/sql.md) | Raw read-only SQL over one dataset. Off unless `[sql].enabled`. |
| POST   | `/api/v1/datasets/{name}/query/stream`            | [Arrow IPC](../query/arrow-ipc.md) | Stream all matching rows as Arrow IPC.             |
| POST   | `/api/v1/datasets/{name}/count`                   | `{ predicates? }` | Total or filtered row count.                                       |
| GET    | `/api/v1/datasets/{name}/parquet`                 | —               | Whole dataset as a Parquet file (HTTP range + `HEAD`).               |
| GET    | `/api/v1/datasets/{name}/all.parquet`             | —               | Alias of `/parquet` whose URL ends in `.parquet` (bare `FROM '…'`).  |
| POST   | `/api/v1/datasets/{name}/reload`                  | —               | Atomic dataset reload. Requires `X-Admin-Token`.                     |
| GET    | `{prefix}/health`                                 | —               | Liveness, prefix-aware.                                              |

## Legacy aliases (`/api`)

Same handlers, no `/v1`:

| Method | Path                                       |
|--------|--------------------------------------------|
| GET    | `/api/datasets`                            |
| GET    | `/api/datasets/{name}/schema`              |
| POST   | `/api/datasets/{name}/query`               |
| POST   | `/api/sql`                                 |
| POST   | `/api/datasets/{name}/query/stream`        |
| POST   | `/api/datasets/{name}/count`               |
| GET    | `/api/datasets/{name}/parquet`             |
| GET    | `/api/datasets/{name}/all.parquet`         |
| POST   | `/api/datasets/{name}/reload`              |

Prefer `/api/v1/...` in new code; the unversioned routes will
eventually be deprecated.

## Probes (unprefixed)

| Method | Path        | Code           | Purpose                                       |
|--------|-------------|----------------|-----------------------------------------------|
| GET    | `/healthz`  | `200`          | Liveness; always OK.                          |
| GET    | `/readyz`   | `200` / `503`  | Ready once at least one dataset is loaded.    |
| GET    | `/version`  | `200`          | Build/version metadata.                       |

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
