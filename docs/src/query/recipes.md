# Recipes

Realistic combinations of every feature. All bodies target
`POST /api/v1/datasets/accidents/query` unless noted.

## R1. Top-N dashboard tile — worst-hit cities per state

Group by `(state, city)`, count rows, average severity, sort by count
descending, cap at 50 cities total.

```json
{
  "group_by": ["state", "city"],
  "aggregations": [
    { "op": "count" },
    { "col": "severity", "op": "avg", "alias": "avg_sev" }
  ],
  "predicates": [
    { "col": "start_time", "op": "gte", "val": "2022-01-01" },
    { "col": "severity",   "op": "gte", "val": 2            }
  ],
  "order_by":  [{ "col": "count", "dir": "desc" }, { "col": "avg_sev", "dir": "desc" }],
  "limit":     50,
  "page_size": 50
}
```

## R2. Histogram bucket — accidents per state, severity ≥ 3

Pure count-by-key. No `aggregations` block needed — the implicit
`COUNT(*) AS count` kicks in.

```json
{
  "group_by":   ["state"],
  "predicates": [{ "col": "severity", "op": "gte", "val": 3 }],
  "order_by":   [{ "col": "count", "dir": "desc" }]
}
```

## R3. Distinct value list for a filter dropdown

```json
{
  "columns":    ["state"],
  "predicates": [{ "col": "severity", "op": "gte", "val": 3 }],
  "distinct":   true,
  "order_by":   [{ "col": "state" }],
  "page_size":  100
}
```

## R4. Time-range scan with multi-column projection + sort

Wide schema, narrow projection, ISO-string range, secondary sort.

```json
{
  "columns": ["id", "state", "city", "severity", "start_time", "weather_condition"],
  "predicates": [
    { "col": "state",      "op": "in",  "val": ["CA", "TX", "NY", "FL"] },
    { "col": "start_time", "op": "gte", "val": "2023-06-01T00:00:00"    },
    { "col": "start_time", "op": "lt",  "val": "2023-07-01T00:00:00"    }
  ],
  "order_by": [
    { "col": "state" },
    { "col": "start_time", "dir": "desc" }
  ],
  "page":      1,
  "page_size": 500
}
```

## R5. Text search with NULL filter

```json
{
  "columns": ["id", "city", "state", "start_lat", "start_lng", "description"],
  "predicates": [
    { "col": "description", "op": "ilike",       "val": "%black ice%" },
    { "col": "start_lat",   "op": "is_not_null"                       },
    { "col": "start_lng",   "op": "is_not_null"                       }
  ],
  "limit":     2000,
  "page_size": 250
}
```

## R6. Preview pane — first N rows, fully bounded

```json
{
  "columns":   ["id", "state", "city", "severity", "start_time"],
  "order_by":  [{ "col": "id" }],
  "limit":     200,
  "page":      1,
  "page_size": 50
}
```

## R7. Per-group min/max/avg with renamed outputs

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
  "predicates": [{ "col": "start_time", "op": "gte", "val": "2023-01-01" }],
  "order_by":   [{ "col": "n_rows", "dir": "desc" }],
  "page_size":  20
}
```

## R8. Arrow IPC into Polars

```bash
curl -X POST 'http://localhost:8080/api/v1/datasets/accidents/query?format=arrow' \
  -H 'Content-Type: application/json' \
  --output page.arrow \
  -d '{
    "columns": ["id","state","city","severity","start_time"],
    "predicates": [
      { "col": "state",      "op": "in",  "val": ["CA","TX"] },
      { "col": "start_time", "op": "gte", "val": "2023-06-01" }
    ],
    "order_by":  [{ "col": "start_time", "dir": "desc" }],
    "page_size": 10000
  }'
```

```python
import pyarrow.ipc as ipc, polars as pl
with open("page.arrow", "rb") as fh:
    table = ipc.open_stream(fh).read_all()
df = pl.from_arrow(table)
```

## R9. Filtered count for a "results so far" badge

```bash
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/count \
     -H 'content-type: application/json' \
     -d '{
       "predicates": [
         { "col": "state",       "op": "in",    "val": ["CA","TX"] },
         { "col": "severity",    "op": "gte",   "val": 3 },
         { "col": "description", "op": "ilike", "val": "%fog%" }
       ]
     }'
```

## R10. Cursor-style pagination via `page_size + 1` probe

```python
PAGE = 100
body = {
    "columns":   ["id", "state", "severity"],
    "order_by":  [{ "col": "id" }],
    "page_size": PAGE + 1,
}

page = 1
while True:
    body["page"] = page
    rows = httpx.post(url, json=body).raise_for_status().json()["data"]
    has_next = len(rows) > PAGE
    yield from rows[:PAGE]
    if not has_next:
        break
    page += 1
```
