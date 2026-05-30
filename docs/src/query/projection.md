# Projection & pagination

## `columns`

Return only the columns you need. Cheaper on wide schemas — the JSON
payload shrinks and the in-memory `take` path materialises only the
projected Arrow arrays.

```json
{
  "columns": ["id", "state", "severity", "start_time"],
  "page_size": 50
}
```

- `columns: []` (or omitted) returns all columns.
- Names are matched **case-insensitively** against the inferred schema,
  and identifiers like `Temperature(F)` are quoted automatically.
- Unknown column names produce `400 Unknown column: <name>`.

## `page` / `page_size`

```json
{ "page": 1, "page_size": 1000 }
```

- `page` is 1-based. `page = 1` returns rows `[0, page_size)`.
  Values `< 1` are treated as `1`.
- `page_size` is clamped to `[1, server.max_page_size]` server-side.
  The default `max_page_size` is `100_000`.

There is no row count in the response. To detect "is there more?" use
one of these patterns:

- Ask for `page_size + 1` and check whether you got the extra row.
- Stop when a page returns fewer rows than `page_size`.
- Hit [`/count`](count.md) separately.

## `limit`

`limit` caps the **total** number of rows returnable across all pages —
not the page size. Useful for previews / dashboards that should never
scan beyond N rows regardless of `page` / `page_size`.

```json
{
  "order_by": [{ "col": "severity", "dir": "desc" }],
  "limit":    100,
  "page_size": 25
}
```

With `limit = 100` and `page_size = 25` you get four full pages of 25;
asking for `page = 5` returns an empty result. Setting `limit` disables
the in-memory fast paths — see [Sorting & limit](sorting.md).
