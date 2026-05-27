# Distinct rows

`distinct: true` deduplicates the projected rows. With `columns` set
you get distinct values over that subset; without `columns` it acts as
`SELECT DISTINCT *`.

```json
{
  "columns":  ["state"],
  "distinct": true,
  "order_by": [{ "col": "state" }],
  "page_size": 100
}
```

Combine with predicates / `limit` / pagination as usual:

```json
{
  "columns":   ["city", "state"],
  "predicates": [{ "col": "severity", "op": "gte", "val": 3 }],
  "distinct":  true,
  "order_by":  [{ "col": "state" }, { "col": "city" }],
  "limit":     5000,
  "page":      1,
  "page_size": 100
}
```

## Rules

- `distinct` is mutually exclusive with `group_by` / `aggregations` —
  combining them returns `400`. Use `group_by` (with no aggregations)
  when you also want counts per distinct combination.
- `distinct` bypasses the in-memory fast paths and always goes through
  the SQL engine.
- On DuckDB the dedup happens on the raw column values before each row
  is formatted as JSON, not on the JSON string itself.
