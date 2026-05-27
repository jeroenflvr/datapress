# Querying

The query endpoint is:

```
POST /api/v1/datasets/{name}/query
Content-Type: application/json
```

The legacy un-versioned alias `POST /api/datasets/{name}/query` is also
mounted and behaves identically.

## Pages

- [Request body](request-body.md) — every field, in one table.
- [Projection & pagination](projection.md) — `columns`, `page`,
  `page_size`, `limit`.
- [Predicates](predicates.md) — all eleven operators.
- [Sorting & limit](sorting.md) — `order_by`, hard caps.
- [Grouping & aggregation](grouping.md) — `group_by` + `aggregations`.
- [Distinct](distinct.md) — `distinct: true`.
- [Arrow IPC vs JSON](arrow-ipc.md) — response format trade-offs.
- [Counting](count.md) — `POST /count` shape.
- [Recipes](recipes.md) — end-to-end queries combining every feature.
