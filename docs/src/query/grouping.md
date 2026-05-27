# Grouping & aggregation

Group rows by one or more columns and compute aggregates per group.

```json
{
  "group_by":   ["state"],
  "aggregations": [
    { "op": "count" },
    { "col": "severity", "op": "avg", "alias": "avg_sev" },
    { "col": "severity", "op": "max" }
  ],
  "order_by":  [{ "col": "count", "dir": "desc" }],
  "page_size": 10
}
```

Returns one row per group with keys `state`, `count`, `avg_sev`,
`max_severity`.

## Rules

- When `group_by` is set, the top-level `columns` field is **ignored**.
  The SELECT list is built from the group columns plus each
  aggregation's output alias.
- Supported `op` values (case-insensitive): `count`, `sum`, `avg`,
  `min`, `max`.
- `col` is **required** for every op except `count`, where it may be
  omitted to mean `COUNT(*)`.
- `alias` is the JSON output key. Defaults: `count` for `COUNT(*)`,
  `{op}_{col}` otherwise.
- `aggregations` without `group_by` returns `400`.
- `order_by` keys must reference a **group column** or an
  **aggregation alias** — arbitrary dataset columns are not in scope
  after `GROUP BY`.
- Grouped queries always run through the SQL engine; no in-memory
  fast path applies.

## Implicit `COUNT(*)`

If `aggregations` is omitted (or empty) an implicit
`COUNT(*) AS count` is added so each group always has at least one
value:

```json
{
  "group_by": ["state"],
  "order_by": [{ "col": "count", "dir": "desc" }]
}
```

## Multi-key grouping

```json
{
  "group_by": ["state", "city"],
  "aggregations": [
    { "op": "count" },
    { "col": "severity", "op": "avg", "alias": "avg_sev" }
  ],
  "order_by":  [{ "col": "count", "dir": "desc" }],
  "page_size": 50
}
```

## All five ops with explicit aliases

```json
{
  "group_by": ["state"],
  "aggregations": [
    { "op": "count",                       "alias": "n_rows"   },
    { "col": "severity",    "op": "avg",   "alias": "sev_avg"  },
    { "col": "severity",    "op": "min",   "alias": "sev_min"  },
    { "col": "severity",    "op": "max",   "alias": "sev_max"  },
    { "col": "distance_mi", "op": "sum",   "alias": "miles"    }
  ],
  "order_by": [{ "col": "n_rows", "dir": "desc" }],
  "page_size": 20
}
```
