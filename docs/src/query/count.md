# Counting rows

```
POST /api/v1/datasets/{name}/count
Content-Type: application/json
```

Same predicate shape as [`/query`](request-body.md), but only the
`predicates` field is read — `columns`, `page`, `page_size`, `limit`
are all ignored. An empty body (or `{}`) counts every row.

## Response

```json
{ "count": 7728394 }
```

## Examples

```bash
# Total row count.
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/count \
     -H 'content-type: application/json' -d '{}'
# → {"count":7728394}

# Filtered count — same operators as /query.
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/count \
     -H 'content-type: application/json' \
     -d '{
       "predicates": [
         { "col": "state",    "op": "in",  "val": ["CA","TX"] },
         { "col": "severity", "op": "gte", "val": 3 }
       ]
     }'
# → {"count":418217}
```

## Execution paths

DataFusion backend:

1. **No predicates** on a materialised dataset → resident `num_rows()`.
   No scan.
2. **All `eq` / `in` on indexed columns** → length of the merged row-id
   list from the equality index. No scan.
3. **Anything else, or lazy datasets** → `SELECT COUNT(*) FROM … WHERE …`
   through DataFusion / DuckDB.

This makes `/count` ideal for "results so far" UI badges that update
as the user adjusts filters.
