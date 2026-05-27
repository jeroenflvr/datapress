# Predicates

Each predicate is an object:

```json
{ "col": "<column-name>", "op": "<operator>", "val": <json-value> }
```

`col` is matched case-insensitively against the dataset schema. The set
of operators is closed — anything else returns
`400 Unknown operator: <op>`.

Multiple predicates are **ANDed** together. For OR, use `in` for the
same column, or issue separate queries client-side.

## Operator reference

| Operator      | Meaning                | `val` shape                        |
|---------------|------------------------|------------------------------------|
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

## Equality and inequality

```json
{
  "predicates": [
    { "col": "state",    "op": "eq",  "val": "CA" },
    { "col": "severity", "op": "neq", "val": 1    }
  ]
}
```

Boolean values work too:

```json
{ "predicates": [ { "col": "amenity", "op": "eq", "val": true } ] }
```

## Ranges

Numeric:

```json
{
  "predicates": [
    { "col": "severity",    "op": "gte", "val": 3   },
    { "col": "distance_mi", "op": "lt",  "val": 5.0 }
  ]
}
```

Lexicographic on strings:

```json
{ "predicates": [ { "col": "state", "op": "gte", "val": "M" } ] }
```

Temporal columns are kept native in RAM and compared as strings in
their ISO-8601 textual form on the wire:

```json
{
  "predicates": [
    { "col": "start_time", "op": "gte", "val": "2023-01-01" },
    { "col": "start_time", "op": "lt",  "val": "2023-07-01" }
  ]
}
```

## `IN` — multi-value membership

```json
{
  "predicates": [
    { "col": "state", "op": "in", "val": ["CA", "TX", "FL", "NY"] }
  ]
}
```

`val` must be a non-empty array. `[]` returns `400`.

A single `in` against an [indexed column](../configuration/indexing.md)
hits the equality-index fast path — no SQL engine involvement, just a
merged sort of the per-value row-id lists.

## `LIKE` and `ILIKE`

SQL wildcards:

- `%` — zero or more characters
- `_` — exactly one character

```json
{
  "predicates": [
    { "col": "city", "op": "like",  "val": "San %"     },
    { "col": "city", "op": "ilike", "val": "%falls%"   }
  ]
}
```

`like` is case-sensitive; `ilike` is case-insensitive. Both go through
the SQL fallback path (they cannot use the equality index).

## Null / not-null

`val` is ignored — omit it or set it to `null`. Both spellings below
are equivalent:

```json
{ "predicates": [
  { "col": "end_lat", "op": "is_null"                    },
  { "col": "end_lng", "op": "is_not_null", "val": null   }
] }
```

## How predicates are executed

For materialised (non-`lazy`) datasets the backend picks the cheapest
applicable path:

1. **Empty predicates** → direct Arrow slice over the resident chunks.
   `O(page_size)`, no engine overhead.
2. **All predicates are `eq` / `in` on indexed columns** →
   equality-index path. Each per-value row-id list is merged in sorted
   order; the page is materialised with a single `arrow::compute::take`.
   `O(predicate_matches)`.
3. **Anything else** (ranges, `LIKE`, `ILIKE`, `is_null`, mixed) →
   DataFusion SQL. Multi-threaded vectorised scan over the dataset,
   then pagination.

For `lazy` datasets every query goes through DataFusion directly
against the `ListingTable` registered on the parquet files — column
projection and predicate pushdown are handled by DataFusion's parquet
reader.

## Combining everything

```json
{
  "columns": ["id", "state", "city", "severity", "start_time"],
  "predicates": [
    { "col": "state",             "op": "in",          "val": ["CA", "TX"] },
    { "col": "severity",          "op": "gte",         "val": 3            },
    { "col": "weather_condition", "op": "ilike",       "val": "%rain%"     },
    { "col": "start_time",        "op": "gte",         "val": "2022-01-01" },
    { "col": "end_lat",           "op": "is_not_null"                       }
  ],
  "page": 1,
  "page_size": 250
}
```
