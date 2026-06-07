# Request body

The body is a JSON object. Every field is optional.

| Field          | Type                  | Default | Meaning                                                                                |
|----------------|-----------------------|---------|----------------------------------------------------------------------------------------|
| `columns`      | `string[]`            | `[]`    | Columns to return. Empty = all columns.                                                |
| `predicates`   | `Predicate[]`         | `[]`    | Row filters, ANDed together.                                                           |
| `order_by`     | `OrderBy[]`           | `[]`    | Sort keys: `{ "col": str, "dir": "asc"\|"desc" }`. `dir` defaults to `asc`.            |
| `group_by`     | `string[]`            | `[]`    | Group-by columns. When set, `columns` is ignored.                                      |
| `aggregations` | `Aggregation[]`       | `[]`    | `{ col?, op, alias? }`; ops: `count\|sum\|avg\|min\|max`. Requires `group_by`.         |
| `having`       | `Predicate[]`         | `[]`    | Post-aggregation filters, ANDed. `col` is a `group_by` column or aggregation alias. Requires `group_by`. |
| `distinct`     | `bool`                | `false` | Deduplicate the projected rows. Mutually exclusive with `group_by` / `aggregations`.   |
| `limit`        | `int >= 0` or `null`  | `null`  | Hard cap on total rows across all pages. `null` = unlimited.                           |
| `page`         | `int >= 1`            | `1`     | 1-based page number.                                                                   |
| `page_size`    | `int >= 1`            | `1000`  | Rows per page. Clamped to `[1, server.max_page_size]`; default cap is `100_000`.        |

## Response — JSON

```json
{ "data": [ { ... }, ... ], "page": 1, "page_size": 50 }
```

`data` is a plain array of row objects. Column names are emitted
verbatim. There is no total-count — pagination is offset/limit only;
see [Counting](count.md) for a separate count endpoint.

## Response — Arrow IPC

When the client opts in (see [Arrow IPC vs JSON](arrow-ipc.md)), the
`/query` body is a self-describing Arrow IPC **stream** for the selected
page and pagination metadata moves into response headers:

```http
Content-Type: application/vnd.apache.arrow.stream
X-Page: 1
X-Page-Size: 50
```

`POST /query/stream` uses the same request body for filtering,
projection, sorting, grouping, and optional `limit`, but ignores `page`
and `page_size`. Its response is one Arrow IPC stream for all matching
rows and does not include page headers.

## Smallest possible query

```bash
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/query \
     -H 'content-type: application/json' \
     -d '{}'
```

Returns the first 1000 rows, all columns.

## Smallest realistic query

```json
{
  "columns": ["id", "state", "severity"],
  "predicates": [
    { "col": "state", "op": "eq", "val": "TX" }
  ],
  "page_size": 100
}
```

## Filtering groups with `having`

`having` filters rows **after** aggregation, the same way SQL `HAVING`
does. It requires a non-empty `group_by`, and each predicate's `col`
references either a `group_by` column or an aggregation **alias** (the
`alias` you set on an `aggregations` entry, or its default: `count` for
`COUNT(*)`, otherwise `{op}_{col}`). Predicates use the same operator
vocabulary as `predicates` (`eq`, `neq`, `gt`, `gte`, `lt`, `lte`,
`like`, `ilike`, `in`, `is_null`, `is_not_null`) and are ANDed together.

```json
{
  "group_by": ["state"],
  "aggregations": [
    { "op": "count", "alias": "n" },
    { "op": "avg", "col": "severity", "alias": "avg_sev" }
  ],
  "having": [
    { "col": "n", "op": "gt", "val": 100 },
    { "col": "avg_sev", "op": "gte", "val": 2.5 }
  ],
  "order_by": [{ "col": "n", "dir": "desc" }]
}
```

This is equivalent to:

```sql
SELECT state, COUNT(*) AS n, AVG(severity) AS avg_sev
FROM accidents
GROUP BY state
HAVING COUNT(*) > 100 AND AVG(severity) >= 2.5
ORDER BY n DESC
```

!!! note "HAVING can only reference declared aggregations"
    A `having` predicate may only filter on a `group_by` column or an
    aggregation you have listed in `aggregations`. To filter on an
    aggregate, add it to `aggregations` first (give it an `alias` and
    reference that). For expressions the structured API can't model
    — window functions, arbitrary SQL — use the [raw SQL
    endpoint](sql.md).
