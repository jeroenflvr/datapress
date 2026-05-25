# Querying datasets

`datap-rs` exposes one query endpoint per dataset:

```
POST /api/datasets/{name}/query
Content-Type: application/json
```

The body is a JSON object with these optional fields:

| Field        | Type            | Default | Meaning                                                                  |
| ------------ | --------------- | ------- | ------------------------------------------------------------------------ |
| `columns`    | `list[str]`     | `[]`    | Columns to return. Empty = all columns.                                  |
| `predicates` | `list[object]`  | `[]`    | Row filters, ANDed together.                                             |
| `order_by`   | `list[object]`  | `[]`    | Sort keys: `{ "col": str, "dir": "asc"\|"desc" }`. `dir` defaults to `asc`. |
| `group_by`     | `list[str]`    | `[]`    | Group-by columns. When set, `columns` is ignored.                        |
| `aggregations` | `list[object]` | `[]`    | `{ col?, op, alias? }`; ops: `count\|sum\|avg\|min\|max`. Requires `group_by`. |
| `limit`      | `int` or null   | `null`  | Hard cap on total rows across all pages. `null` = unlimited.             |
| `page`       | `int` (≥ 1)     | `1`     | 1-based page number.                                                     |
| `page_size`  | `int` (1–1_000_000)  | `1000`   | Rows per page. Hard cap is 1,000,000.                                         |

The response is a JSON array of row objects. There is no envelope and no
total-count — pagination is offset/limit only.

Every example below uses the `accidents` dataset from `data/us_accidents/`.

---

## 1. Empty body — first page of everything

```bash
curl -s -X POST http://localhost:8000/api/datasets/accidents/query \
     -H 'content-type: application/json' \
     -d '{}'
```

Returns the first 100 rows, all columns.

---

## 2. Column projection

Return only the columns you actually need. Massively cheaper on wide schemas:
the JSON payload shrinks, and the in-memory `take` path materialises only the
projected Arrow arrays.

```json
{
  "columns": ["id", "state", "severity", "start_time"],
  "page_size": 50
}
```

Unknown column names produce a `400 Unknown column: <name>`.

---

## 3. Predicates — operator reference

Each predicate is an object:

```json
{ "col": "<column-name>", "op": "<operator>", "val": <json-value> }
```

`col` is matched case-insensitively against the dataset schema. The set of
operators is closed — anything else returns `400 Unknown operator: <op>`.

| Operator      | Meaning                | `val` shape                        |
| ------------- | ---------------------- | ---------------------------------- |
| `eq`          | `col = val`            | scalar (string, number, bool)      |
| `neq`         | `col != val`           | scalar                             |
| `gt`          | `col >  val`           | scalar                             |
| `gte`         | `col >= val`           | scalar                             |
| `lt`          | `col <  val`           | scalar                             |
| `lte`         | `col <= val`           | scalar                             |
| `like`        | `col LIKE val`         | string (use `%` / `_` wildcards)   |
| `ilike`       | `col ILIKE val`        | string (case-insensitive LIKE)     |
| `in`          | `col IN (v1, v2, …)`   | non-empty array of scalars         |
| `is_null`     | `col IS NULL`          | omit `val` (or set to `null`)      |
| `is_not_null` | `col IS NOT NULL`      | omit `val` (or set to `null`)      |

Multiple predicates are ANDed. For OR, use `in` for the same column, or
issue separate queries client-side.

---

## 4. Equality and inequality

```json
{
  "predicates": [
    { "col": "state",    "op": "eq",  "val": "CA" },
    { "col": "severity", "op": "neq", "val": 1 }
  ]
}
```

Boolean values work too:

```json
{ "predicates": [ { "col": "amenity", "op": "eq", "val": true } ] }
```

---

## 5. Ranges

Numeric:

```json
{
  "predicates": [
    { "col": "severity",   "op": "gte", "val": 3 },
    { "col": "distance_mi","op": "lt",  "val": 5.0 }
  ]
}
```

Lexicographic on strings (uses SQL ordering):

```json
{ "predicates": [ { "col": "state", "op": "gte", "val": "M" } ] }
```

Temporal columns are kept native in RAM and compared as strings in their
ISO-8601 textual form on the wire — pass an ISO string:

```json
{
  "predicates": [
    { "col": "start_time", "op": "gte", "val": "2023-01-01" },
    { "col": "start_time", "op": "lt",  "val": "2023-07-01" }
  ]
}
```

---

## 6. `IN` — multi-value membership

```json
{
  "predicates": [
    { "col": "state", "op": "in", "val": ["CA", "TX", "FL", "NY"] }
  ]
}
```

`val` must be a non-empty array. `[]` returns `400`.

A single `in` against an indexed column hits the equality-index fast path —
no SQL engine involvement, just a merged sort of the per-value row-id lists.

---

## 7. `LIKE` and `ILIKE`

SQL wildcards:

* `%` — zero or more characters
* `_` — exactly one character

```json
{
  "predicates": [
    { "col": "city", "op": "like",  "val": "San %" },
    { "col": "city", "op": "ilike", "val": "%falls%" }
  ]
}
```

`like` is case-sensitive; `ilike` is case-insensitive. Both go through the
SQL fallback path (they cannot use the equality index).

---

## 8. Null / not-null

`val` is ignored — omit it or set it to `null`. Both spellings below are
equivalent:

```json
{ "predicates": [
  { "col": "end_lat", "op": "is_null" },
  { "col": "end_lng", "op": "is_not_null", "val": null }
] }
```

---

## 9. Combining everything

```json
{
  "columns": ["id", "state", "city", "severity", "start_time"],
  "predicates": [
    { "col": "state",        "op": "in",   "val": ["CA", "TX"] },
    { "col": "severity",     "op": "gte",  "val": 3 },
    { "col": "weather_condition", "op": "ilike", "val": "%rain%" },
    { "col": "start_time",   "op": "gte",  "val": "2022-01-01" },
    { "col": "end_lat",      "op": "is_not_null" }
  ],
  "page": 1,
  "page_size": 250
}
```

---

## 10. Pagination

```json
{ "page": 1, "page_size": 1000 }
```

There is **no row count** in the response. To know if more pages exist, ask
for `page_size + 1` and check whether you got the extra row, or stop when a
page returns fewer rows than `page_size`.

`page_size` is clamped to `[1, 1_000_000]` server-side. `page < 1` is treated as
`page = 1`.

Page numbers are 1-based — `page=1` returns rows `[0, page_size)`.

---

## 11. Sorting (`order_by`)

Add one or more sort keys. `dir` is optional and defaults to `"asc"`;
`"desc"` is the only other accepted value (case-insensitive). Unknown
columns or directions return `400`.

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

Sorted queries always run through the SQL engine — they do not use the
in-memory Arrow-slice or equality-index fast paths, even when `predicates`
is empty or hits an indexed column.

---

## 12. Hard row cap (`limit`)

`limit` caps the *total* number of rows returnable across all pages, not
the page size. Useful for previews / dashboards that should never scan
beyond N rows regardless of `page` / `page_size`.

```json
{
  "order_by": [{ "col": "severity", "dir": "desc" }],
  "limit":    100,
  "page_size": 25
}
```

With `limit = 100` and `page_size = 25` you get four full pages of 25;
asking for `page = 5` returns an empty `data` array. Like `order_by`,
setting `limit` disables the in-memory fast paths.

---

## 13. Aggregation (`group_by` + `aggregations`)

Group rows by one or more columns and compute aggregates per group. When
`group_by` is set, `columns` is ignored — the SELECT list is built from
the group columns plus each aggregation's output alias.

Supported `op` values: `count`, `sum`, `avg`, `min`, `max` (case-insensitive).
`col` is required for every op except `count`, where it may be omitted to
mean `COUNT(*)`. `alias` is the JSON output key; when omitted it defaults
to `count` for `COUNT(*)` and `{op}_{col}` otherwise.

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

Returns one row per group with keys `state`, `count`, `avg_sev`, `max_severity`.

If `group_by` is set and `aggregations` is empty, an implicit `COUNT(*) AS count`
is added so each group always has at least one value:

```json
{ "group_by": ["state"], "order_by": [{ "col": "count", "dir": "desc" }] }
```

Notes:

* `aggregations` without `group_by` returns `400`.
* Sort keys in `order_by` must reference a group column or an aggregation
  alias — not arbitrary dataset columns (they are not in scope after
  `GROUP BY`).
* Grouped queries always run through the SQL engine. They do not use the
  in-memory fast paths.

---

## 14. Distinct rows (`distinct`)

`distinct: true` deduplicates the projected rows. With `columns` set you
get distinct values over that subset; without `columns` it acts as
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

Notes:

* `distinct` is mutually exclusive with `group_by` / `aggregations` —
  combining them returns `400`. Use `group_by` (with no aggregations) when
  you also want counts per distinct combination.
* `distinct` bypasses the in-memory fast paths and always goes through
  the SQL engine.
* On DuckDB the dedup happens on the raw column values before each row
  is formatted as JSON, not on the JSON string itself.

---

## 15. Response format — JSON or Arrow IPC

By default `/query` returns the JSON envelope shown throughout this doc.
Clients that want a columnar binary payload can opt into an Arrow IPC
stream — useful for moving large result pages into Polars / pandas /
DuckDB-on-the-client without paying JSON encode/decode.

Two ways to ask:

* HTTP header: `Accept: application/vnd.apache.arrow.stream`
* Query string: append `?format=arrow` to the URL

The response is then `Content-Type: application/vnd.apache.arrow.stream`
— a self-describing Arrow IPC **stream** (one schema message + zero or
more `RecordBatch` messages + EOS). Pagination metadata moves into
response headers: `X-Page` and `X-Page-Size`.

```bash
curl -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/vnd.apache.arrow.stream' \
  --output page.arrow \
  -d '{ "columns": ["ID","State"], "page_size": 1000 }'
```

```python
import requests, pyarrow.ipc as ipc, polars as pl
r = requests.post(url, json=req,
                  headers={"Accept": "application/vnd.apache.arrow.stream"})
table = ipc.open_stream(r.content).read_all()   # pyarrow.Table
df    = pl.from_arrow(table)                    # zero-copy → Polars
```

Notes:

* Currently implemented on the **DataFusion** backend only. DuckDB
  returns `400 invalid value: Arrow IPC response format is not supported
  by this backend` — falls back to JSON on the client side trivially.
* Empty results still produce a valid stream (schema only, no batches).
* `Compress` middleware applies normally; gzip / zstd over the binary
  payload still wins on wide / repetitive columns.
* `count`, `schema`, and the dataset-listing endpoints are unaffected —
  they remain JSON.

---

## How predicates are executed

For materialised (non-`lazy`) datasets the backend picks the cheapest
applicable path:

1. **Empty predicates** → direct Arrow slice over the resident chunks.
   `O(page_size)`, no engine overhead.
2. **All predicates are `eq` / `in` on indexed columns** → equality-index
   path. Each per-value row-id list is merged in sorted order; the page is
   materialised with a single `arrow::compute::take`. `O(predicate_matches)`.
3. **Anything else** (ranges, `LIKE`, `ILIKE`, `is_null`, mixed) →
   DataFusion SQL. Multi-threaded vectorised scan over the dataset, then
   pagination.

For `lazy` datasets every query goes through DataFusion directly against the
`ListingTable` registered on the parquet files — column projection and
predicate pushdown are handled by DataFusion's parquet reader.

Index-eligible types are: `Utf8` (including dictionary-encoded), `Boolean`,
and the signed integer family (`Int8/16/32/64`). Floats, temporals and
binary columns always go through SQL.

---

## Counting rows

```
POST /api/datasets/{name}/count
Content-Type: application/json
```

Same predicate shape as `/query`, but only the `predicates` field is read —
`columns`, `page`, `page_size` are ignored. An empty body (or `{}`) counts
every row in the dataset.

Response:

```json
{ "count": 7728394 }
```

Examples:

```bash
# Total row count — O(1) on materialised datasets (no scan).
curl -s -X POST http://localhost:8000/api/datasets/accidents/count \
     -H 'content-type: application/json' -d '{}'
# → {"count":7728394}

# Filtered count — same operators as /query.
curl -s -X POST http://localhost:8000/api/datasets/accidents/count \
     -H 'content-type: application/json' \
     -d '{
       "predicates": [
         { "col": "state",    "op": "in",  "val": ["CA","TX"] },
         { "col": "severity", "op": "gte", "val": 3 }
       ]
     }'
# → {"count":418217}
```

Execution paths (DataFusion backend):

1. **No predicates** on a materialised dataset → resident `num_rows()`. No scan.
2. **All `eq` / `in` on indexed columns** → length of the merged row-id
   list from the equality index. No scan.
3. **Anything else, or lazy datasets** → `SELECT COUNT(*) FROM … WHERE …`
   through DataFusion / DuckDB.

---

## Python — querying from a client

`datap-rs` ships a server, not a Python client. Use any HTTP library —
`httpx`, `requests`, `aiohttp`. The body is plain JSON:

```python
import httpx

resp = httpx.post(
    "http://localhost:8000/api/datasets/accidents/query",
    json={
        "columns": ["id", "state", "severity", "start_time"],
        "predicates": [
            {"col": "state",             "op": "in",    "val": ["CA", "TX"]},
            {"col": "severity",          "op": "gte",   "val": 3},
            {"col": "weather_condition", "op": "ilike", "val": "%rain%"},
            {"col": "start_time",        "op": "gte",   "val": "2022-01-01"},
            {"col": "start_time",        "op": "lt",    "val": "2023-01-01"},
            {"col": "end_lat",           "op": "is_not_null"},
        ],
        "page": 1,
        "page_size": 250,
    },
    timeout=30.0,
)
resp.raise_for_status()
rows = resp.json()  # list[dict]
```

Counting from Python is the same shape — POST to `/count` with just
`predicates`:

```python
import httpx

total = httpx.post(
    "http://localhost:8000/api/datasets/accidents/count",
    json={},
    timeout=30.0,
).raise_for_status().json()["count"]

filtered = httpx.post(
    "http://localhost:8000/api/datasets/accidents/count",
    json={"predicates": [
        {"col": "state",    "op": "in",  "val": ["CA", "TX"]},
        {"col": "severity", "op": "gte", "val": 3},
    ]},
    timeout=30.0,
).raise_for_status().json()["count"]
```

To iterate all matching rows without holding them in memory:

```python
def iter_pages(url, body, page_size=1000):
    page = 1
    while True:
        body = {**body, "page": page, "page_size": page_size}
        rows = httpx.post(url, json=body, timeout=60.0).raise_for_status().json()
        if not rows:
            return
        yield from rows
        if len(rows) < page_size:
            return
        page += 1
```

