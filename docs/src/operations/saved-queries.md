---
description: >-
  Register, list, and delete datasets at runtime via POST /api/v1/queries.
  Supports ephemeral temp datasets and persisted saved queries that survive
  restart.
---

# Saved queries

The saved-queries API lets you register a query dataset at runtime without
editing `datasets.toml`. Useful for ad-hoc analysis, temporary datasets with
a TTL, or gradually promoting exploratory queries to config-file permanence.

All three routes are **admin-gated**: they require the `X-Admin-Token` header
(same token as `POST .../reload`), or — when the `auth` feature is enabled —
a bearer token that carries the `datasets:manage` scope.

---

## Routes

### `POST /api/v1/queries` — create

```
POST /api/v1/queries
X-Admin-Token: $ADMIN_TOKEN
Content-Type: application/json
```

**Request body**

```json
{
  "name":    "tx_severe",
  "sql":     "SELECT * FROM accidents WHERE state = 'TX' AND severity >= 3",
  "kind":    "temp",
  "ttl":     "2h",
  "refresh": { "interval": "15m", "on_upstream_reload": true },
  "materialize": { "residency": "auto", "sort_by": ["state"] },
  "index":   { "mode": "auto" }
}
```

| Field         | Required | Default  | Notes |
|---------------|----------|----------|-------|
| `name`        | yes      | —        | Dataset name. Must be unique across all registered datasets. `409` on conflict. The name `"reload-all"` is reserved and is rejected. |
| `sql`         | yes      | —        | A single read-only `SELECT` (or `WITH … SELECT`). The same validator as `POST /api/v1/sql` applies. |
| `kind`        | no       | `"temp"` | `"temp"` (ephemeral) or `"query"` (persisted). |
| `ttl`         | no       | *(unset)*| Humantime duration. Temp datasets only. After expiry the dataset is automatically unregistered. Absent = lives until restart or explicit delete. |
| `refresh`     | no       | *(unset)*| Same schema as `[dataset.refresh]` in `datasets.toml`. Accepted for both kinds. |
| `materialize` | no       | *(unset)*| Same schema as `[dataset.materialize]`. |
| `index`       | no       | *(unset)*| DataFusion equality-index policy. Rejected if `residency = "lazy"`. |

`depends_on` is **inferred** — the server parses the SQL, determines every
referenced dataset, and returns the list in the response. You do not supply it.

**Query parameters**

| Param    | Default | Notes |
|----------|---------|-------|
| `?async` | `false` | When `true`, return `202 Accepted` immediately with the dataset in `building` state rather than waiting for the build to complete. Poll `GET /api/v1/datasets/{name}/status` for the transition to `published`. |

**Success response (sync `200`)**

```json
{
  "name": "tx_severe",
  "kind": "temp",
  "depends_on": ["accidents"],
  "state": "published",
  "rows": 12345
}
```

**Success response (async `202`)**

```json
{
  "name": "tx_severe",
  "kind": "temp",
  "depends_on": ["accidents"],
  "state": "building"
}
```

**Error responses**

| Code | Reason |
|------|--------|
| `400` | Invalid SQL, unknown table reference, or invalid field value. |
| `401` / `403` | Missing or invalid auth token / scope. |
| `409` | Name conflict with an existing dataset. |

---

### `GET /api/v1/queries` — list

Returns all datasets created via the saved-queries API (both `temp` and
`query` kinds).

```bash
curl -s -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/queries | jq
```

**Response**

```json
[
  {
    "name":       "tx_severe",
    "kind":       "temp",
    "depends_on": ["accidents"],
    "state":      "published"
  },
  {
    "name":       "daily_counts",
    "kind":       "query",
    "depends_on": ["accidents"],
    "state":      "published"
  }
]
```

---

### `DELETE /api/v1/queries/{name}` — delete

```bash
curl -s -X DELETE \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/queries/tx_severe
```

| Code | Reason |
|------|--------|
| `200` | Dataset unregistered (and storage generations removed for storage-backed datasets). |
| `403` | The named dataset was not created via `POST /api/v1/queries` — config-file datasets cannot be deleted at runtime. |
| `404` | Dataset not found. |
| `409` | Other registered datasets depend on this one. The body lists the dependents. |

In-flight queries finish against their captured generation snapshot (Arc
semantics for DataFusion; DuckDB `DROP TABLE` runs after the registry entry
is removed). Storage generations are wiped from disk as part of unregister.

---

## `kind = "temp"` — ephemeral datasets

A `temp` dataset lives entirely in memory and is not written to any file.
It disappears on server restart.

```bash
# Create a temp dataset that expires in 2 hours
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"recent_accidents","sql":"SELECT * FROM accidents WHERE year = 2023","kind":"temp","ttl":"2h"}' \
  http://localhost:8080/api/v1/queries
```

When `ttl` is set, the scheduler fires a one-shot expiry at the deadline.
Expiry behaves identically to `DELETE /api/v1/queries/{name}`: the dataset
is unregistered and its storage tree (if any) is wiped. A `409` at expiry
time (dependents exist) is logged as a WARN — the dataset survives.

`ttl` is only meaningful for `kind = "temp"`. Setting it on `kind = "query"`
is a `400` error.

---

## `kind = "query"` — persisted datasets

A `query` dataset is written to a TOML file under the managed directory so
it survives server restart.

**Default managed directory**: `<config_dir>/datasets.d/` — a sibling
directory next to the main `datasets.toml`. Override with:

```toml
[server]
saved_queries_dir = "/var/lib/datapress/saved"
```

At startup DataPress loads all `*.toml` files from this directory after
`datasets.toml`. A name collision between the two sources fails startup.

The persisted TOML has the same schema as a `[[dataset]]` entry plus an
internal `managed = true` marker. **The user's `datasets.toml` is never
rewritten.**

```bash
# Create a persisted saved query
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "daily_counts",
    "sql":  "SELECT state, COUNT(*) AS n FROM accidents GROUP BY state",
    "kind": "query",
    "refresh": {"interval":"1h","on_upstream_reload":true}
  }' \
  http://localhost:8080/api/v1/queries | jq
```

---

## Bulk reload — `POST /api/v1/datasets/reload-all`

Enqueues every reloadable dataset for refresh in a single wave:

```bash
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/datasets/reload-all | jq
```

**Response `202 Accepted`**

```json
{
  "enqueued": ["accidents", "state_daily_severity", "daily_counts"],
  "skipped":  ["regions"]
}
```

`skipped` contains datasets that are already `building`. Datasets in
`pending` state with `on_start = "lazy"` or `on_start = "skip"` are excluded
(nothing to reload). Dependents within the wave absorb their upstreams'
cascade trigger via debounce — no dataset builds twice within one wave.

The `reload-all` dataset name is reserved and rejected at config validation.

---

## Auth

| Scenario | Required |
|----------|----------|
| `ADMIN_TOKEN` set, `auth` feature off | `X-Admin-Token: $ADMIN_TOKEN` header |
| `auth` feature on, OIDC configured | Bearer token with `datasets:manage` scope |
| Neither token set, no OIDC | Routes return `404` (not even their existence is revealed) |

The `datasets:manage` scope is separate from `datasets:reload` — users with
read or reload access cannot create or delete runtime datasets.

---

## Explorer integration

When DataPress is built with `--features explorer`, the browser UI gains:

- **Save as dataset** action on any query result — opens a form to create a
  `temp` or `query` dataset from the current SQL. The server re-validates
  before building. The inferred `depends_on` list is shown for confirmation.
- **State badges** in the dataset list showing `pending / building /
  published / failed` in real time (polls `GET .../status`).
- **Per-dataset reload button** calling `POST /api/v1/datasets/{name}/reload`.
- **Reload all button** with a confirmation dialog calling
  `POST /api/v1/datasets/reload-all`.
- **Delete action** on runtime-created (`managed`) datasets, surfacing
  `409` errors verbatim if dependents exist.

The save/delete actions are hidden entirely when the saved-queries routes
return `404` (auth not configured or feature not compiled in).

---

## Interaction with the dependency graph

Runtime datasets participate fully in the dependency graph:

- A `query` dataset created via `POST /api/v1/queries` may list config-file
  datasets in its `depends_on` (inferred automatically).
- Config-file datasets **cannot** `depends_on` a runtime dataset (config is
  validated before runtime state exists).
- `on_upstream_reload = true` in the `refresh` block triggers cascade exactly
  as for a config-file query dataset.

Deleting a dataset that other registered datasets depend on returns `409`
with the list of dependents. The dataset survives.

---

## `[server] saved_queries_dir`

```toml
[server]
saved_queries_dir = "/var/lib/datapress/saved"   # absolute path
# or relative to the config file directory:
saved_queries_dir = "datasets.d"
```

| Value          | Behaviour |
|----------------|-----------|
| *(unset)*      | Default: `<config_dir>/datasets.d/` |
| absolute path  | Used as-is. Directory is created on first write if absent. |
| relative path  | Resolved relative to the directory containing `datasets.toml`. |
