# Request body

The body is a JSON object. Every field is optional.

| Field          | Type                  | Default | Meaning                                                                                |
|----------------|-----------------------|---------|----------------------------------------------------------------------------------------|
| `columns`      | `string[]`            | `[]`    | Columns to return. Empty = all columns.                                                |
| `predicates`   | `Predicate[]`         | `[]`    | Row filters, ANDed together.                                                           |
| `order_by`     | `OrderBy[]`           | `[]`    | Sort keys: `{ "col": str, "dir": "asc"\|"desc" }`. `dir` defaults to `asc`.            |
| `group_by`     | `string[]`            | `[]`    | Group-by columns. When set, `columns` is ignored.                                      |
| `aggregations` | `Aggregation[]`       | `[]`    | `{ col?, op, alias? }`; ops: `count\|sum\|avg\|min\|max`. Requires `group_by`.         |
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
body is a self-describing Arrow IPC **stream** and pagination metadata
moves into response headers:

```http
Content-Type: application/vnd.apache.arrow.stream
X-Page: 1
X-Page-Size: 50
```

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
