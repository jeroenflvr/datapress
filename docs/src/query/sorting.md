# Sorting & limit

## `order_by`

```json
{
  "columns":  ["id", "severity", "start_time"],
  "order_by": [
    { "col": "severity",   "dir": "desc" },
    { "col": "start_time" }
  ],
  "page_size": 50
}
```

- Sort keys are applied in declaration order.
- `dir` is optional and defaults to `"asc"`. `"desc"` is the only
  other accepted value (case-insensitive).
- Unknown columns or directions return `400`.
- When `group_by` is set, `order_by` keys **must** reference a group
  column or aggregation alias (see [Grouping](grouping.md)).

!!! info "Performance note"
    Sorted queries always run through the SQL engine — they do not use
    the in-memory Arrow-slice or equality-index fast paths, even when
    `predicates` is empty or hits an indexed column.

## `limit`

`limit` caps the **total** number of rows returnable across all pages,
not the page size.

```json
{
  "order_by": [{ "col": "severity", "dir": "desc" }],
  "limit":    100,
  "page_size": 25
}
```

With `limit = 100` and `page_size = 25` you get four full pages of 25;
asking for `page = 5` returns an empty result.

Like `order_by`, setting `limit` disables the in-memory fast paths and
goes through the SQL engine. Useful for previews / dashboards that
should never scan beyond N rows regardless of `page` / `page_size`.
