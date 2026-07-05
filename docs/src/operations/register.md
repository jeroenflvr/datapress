---
description: >-
  Add datasets to a running DataPress server without a restart — register a
  single dataset over the API, persist it back to datasets.toml, or hot-reload
  the config file to pick up newly-added datasets.
---

# Register datasets at runtime

DataPress can add new datasets to a **running** server without a restart.
Three admin-only endpoints cover the workflow:

| Endpoint | Purpose |
|----------|---------|
| `POST /api/v1/datasets` | Register one new dataset from a JSON body. |
| `POST /api/v1/datasets/persist` | Append a dataset's `[[dataset]]` block to `datasets.toml` so it survives a restart. |
| `POST /api/v1/config/reload` | Re-read `datasets.toml` and register every newly-added `[[dataset]]`. |

All three require the same permission as [dataset reload](reload.md): an
`X-Admin-Token` header matching `ADMIN_TOKEN`, or a bearer token carrying the
configured reload scope when [OIDC auth](auth.md) is enabled.

Newly-registered datasets are held **in memory only**. They answer queries
immediately, but disappear on restart unless you also persist them (or add
them to `datasets.toml` and hot-reload).

## Register a single dataset

`POST /api/v1/datasets` takes a JSON body with the same shape as a
`[[dataset]]` block (see [Configuration › Datasets](../configuration/datasets.md)).
The backend validates the config, opens the source, and makes the dataset
queryable. It returns the fresh dataset summary.

=== "Local Parquet"

    ```bash
    curl -s -X POST \
      -H "X-Admin-Token: $ADMIN_TOKEN" \
      -H "Content-Type: application/json" \
      -d '{
            "name":   "events",
            "source": { "kind": "parquet", "location": "/data/events/*.parquet" }
          }' \
      http://localhost:8080/api/v1/datasets | jq
    # { "name": "events", "rows": 1240333, "columns": 12 }
    ```

=== "S3 Parquet"

    ```bash
    curl -s -X POST \
      -H "X-Admin-Token: $ADMIN_TOKEN" \
      -H "Content-Type: application/json" \
      -d '{
            "name":   "orders",
            "lazy":   true,
            "source": { "kind": "parquet", "location": "s3://lake/orders/*.parquet" },
            "s3":     { "region": "us-east-1" }
          }' \
      http://localhost:8080/api/v1/datasets | jq
    ```

=== "Delta + index"

    ```bash
    curl -s -X POST \
      -H "X-Admin-Token: $ADMIN_TOKEN" \
      -H "Content-Type: application/json" \
      -d '{
            "name":   "customers",
            "source": { "kind": "delta", "location": "/data/customers" },
            "index":  { "mode": "list", "columns": ["region", "tier"] }
          }' \
      http://localhost:8080/api/v1/datasets | jq
    ```

=== "Python (requests)"

    ```python
    import os, requests

    resp = requests.post(
        "http://localhost:8080/api/v1/datasets",
        headers={"X-Admin-Token": os.environ["ADMIN_TOKEN"]},
        json={
            "name": "events",
            "source": {"kind": "parquet", "location": "/data/events/*.parquet"},
        },
    )
    resp.raise_for_status()
    print(resp.json())  # {'name': 'events', 'rows': 1240333, 'columns': 12}
    ```

The full body accepts every field a `[[dataset]]` block does:

```json
{
  "name": "events",
  "source": { "kind": "parquet", "location": "s3://lake/events/*.parquet" },
  "columns": ["id", "ts", "kind", "payload"],
  "dict_encode": true,
  "lazy": false,
  "index": { "mode": "auto", "columns": [], "max_cardinality": 100000 },
  "s3": {
    "region": "us-east-1",
    "endpoint": "http://minio.local:9000",
    "addressing_style": "path",
    "allow_http": true,
    "partitioning": "hive"
  }
}
```

A `400` is returned when the name is already registered, the name uses
characters outside `[A-Za-z0-9_.-]`, or the source cannot be opened
(not found, access denied, empty).

## Persist a dataset to the config file

Registration alone is in-memory. To make a dataset survive a restart, append
its `[[dataset]]` block to the `datasets.toml` the server was started from:

```bash
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
        "name":   "events",
        "source": { "kind": "parquet", "location": "/data/events/*.parquet" }
      }' \
  http://localhost:8080/api/v1/datasets/persist | jq
# { "persisted": true, "path": "/etc/datapress/datasets.toml" }
```

The body is the same `DatasetConfig` shape as `POST /datasets`. This only
works when the server was started from a config **file** — a server
configured in-process (for example through the [Python bindings](../python/index.md))
has no file to append to and returns `400`.

!!! tip "Register, then persist"
    A common pattern is to `POST /datasets` first, confirm the dataset loads
    and answers queries, then `POST /datasets/persist` with the same body to
    keep it. The [explorer UI](#explorer-ui) does exactly this from its
    **Register** tab.

## Hot-reload the config file

If you edit `datasets.toml` on disk — for example a data pipeline appends a new
`[[dataset]]` when it publishes fresh files — trigger a hot reload to pick up
the additions without a restart:

```bash
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/config/reload | jq
# {
#   "registered": ["events", "orders"],
#   "skipped":    ["accidents"],
#   "errors":     []
# }
```

The server re-reads and validates the file, then registers every
`[[dataset]]` whose name is **not already registered**:

- `registered` — datasets that were newly added and loaded.
- `skipped` — datasets that were already live (left untouched).
- `errors` — datasets that failed to load, each as `{ "dataset", "error" }`.
  One bad dataset does not abort the others, so the response can report a
  partial success with `200`.

What hot-reload does **not** do:

- It does not rebuild datasets that already exist — use
  [`POST /datasets/{name}/reload`](reload.md) to refresh a dataset whose
  files changed under the same name.
- It does not re-apply server-level settings (`port`, `workers`, `[sql]`,
  `[auth]`, …). Those still require a restart.
- It does not remove datasets that were deleted from the file.

### Automating on new data

Because the trigger is a single authenticated `POST`, any external event loop
can drive it — a Kafka consumer, a cron job, or a pipeline post-publish hook:

```python
import os, requests

def refresh_datapress():
    r = requests.post(
        "http://localhost:8080/api/v1/config/reload",
        headers={"X-Admin-Token": os.environ["ADMIN_TOKEN"]},
    )
    r.raise_for_status()
    added = r.json()["registered"]
    if added:
        print(f"registered new datasets: {added}")
```

## Explorer UI

When the [explorer](../configuration/docs-site.md) is enabled, its **Register**
tab provides a form for the same workflow: fill in the name, source kind,
location, optional projection / index / S3 settings, and submit. On success
the tab shows the live row and column counts plus the exported
`[[dataset]]` TOML, and — when the server was loaded from a config file —
a button to persist the block to it.

The Register (and Persist) actions are gated by the **same** admin/reload
permission as the HTTP API — the explorer is not a back door around it. Enter
your `ADMIN_TOKEN` in the form's **Admin token** field (sent as the
`X-Admin-Token` header, never in the request body), or sign in with the
**Authorize** button on the API Query tab and your bearer token is forwarded
automatically. Without either, the server has no `ADMIN_TOKEN` and no OIDC
reload scope, the action is rejected just like the API.

## Security

These endpoints mutate server state and read from arbitrary local paths or
object-store URLs you supply, so they are gated behind the admin/reload
permission. Keep `ADMIN_TOKEN` secret (or enforce OIDC reload scopes), and put
the server behind TLS before exposing the admin surface beyond localhost. See
[Authentication](auth.md) for the OIDC path.
