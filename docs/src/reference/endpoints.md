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
| POST   | `/api/v1/datasets/{name}/query/stream`            | [Arrow IPC](../query/arrow-ipc.md) | Stream all matching rows as Arrow IPC.             |
| POST   | `/api/v1/datasets/{name}/count`                   | `{ predicates? }` | Total or filtered row count.                                       |
| POST   | `/api/v1/datasets/{name}/reload`                  | —               | Atomic dataset reload. Requires `X-Admin-Token`.                     |
| GET    | `{prefix}/health`                                 | —               | Liveness, prefix-aware.                                              |

## Legacy aliases (`/api`)

Same handlers, no `/v1`:

| Method | Path                                       |
|--------|--------------------------------------------|
| GET    | `/api/datasets`                            |
| GET    | `/api/datasets/{name}/schema`              |
| POST   | `/api/datasets/{name}/query`               |
| POST   | `/api/datasets/{name}/query/stream`        |
| POST   | `/api/datasets/{name}/count`               |
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
