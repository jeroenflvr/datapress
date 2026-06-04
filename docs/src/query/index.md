---
description: >-
  Query DataPress over HTTP: projection, pagination, predicates, sorting,
  grouping and aggregation, distinct, counting, raw SQL, and Arrow IPC vs
  JSON output.
---

# Querying

The query endpoint is:

```
POST /api/v1/datasets/{name}/query
Content-Type: application/json
```

For one-request Arrow IPC exports, use:

```
POST /api/v1/datasets/{name}/query/stream
Content-Type: application/json
```

The legacy un-versioned alias `POST /api/datasets/{name}/query` is also
mounted and behaves identically. The stream endpoint is also available
under `POST /api/datasets/{name}/query/stream`.

For full SQL — window functions, `CASE`, sub-selects, string and math
functions — over a single dataset, use the opt-in [raw SQL
endpoint](sql.md):

```
POST /api/v1/sql
Content-Type: application/json
```

## Pages

- [Request body](request-body.md) — every field, in one table.
- [Projection & pagination](projection.md) — `columns`, `page`,
  `page_size`, `limit`.
- [Predicates](predicates.md) — all eleven operators.
- [Sorting & limit](sorting.md) — `order_by`, hard caps.
- [Grouping & aggregation](grouping.md) — `group_by` + `aggregations`.
- [Distinct](distinct.md) — `distinct: true`.
- [Arrow IPC vs JSON](arrow-ipc.md) — paged Arrow responses and full-result streams.
- [Counting](count.md) — `POST /count` shape.
- [Raw SQL](sql.md) — opt-in `POST /api/v1/sql` for full SQL over one dataset.
- [Recipes](recipes.md) — end-to-end queries combining every feature.
