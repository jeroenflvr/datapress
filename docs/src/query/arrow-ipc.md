# Arrow IPC vs JSON

`/query` can return its result set in two wire formats. Same body,
same predicates, same pagination — only the response encoding differs.

## Comparison

| Aspect              | JSON (default)                                       | Arrow IPC stream                                                                 |
|---------------------|------------------------------------------------------|----------------------------------------------------------------------------------|
| Content-Type        | `application/json`                                   | `application/vnd.apache.arrow.stream`                                            |
| How to ask          | nothing — it's the default                           | `Accept: application/vnd.apache.arrow.stream` **or** `?format=arrow` on the URL  |
| Shape               | `{ "data": [{...}, ...], "page": N, "page_size": M }` | Self-describing stream: 1 schema message + N `RecordBatch` messages + EOS        |
| Layout              | Row-oriented; column names repeated on every row     | Columnar; one contiguous buffer per column per batch                             |
| Types preserved     | JSON scalars only (`int`/`float`/`bool`/`string`); temporals stringified to ISO-8601 | Native Arrow types — `Int32`, `Timestamp(ns)`, `Decimal128`, dictionary, etc. retained end-to-end |
| Page metadata       | In the body                                          | In headers: `X-Page`, `X-Page-Size`                                              |
| Empty result        | `{ "data": [], "page": ..., "page_size": ... }`      | Valid stream with the schema message only, zero batches                          |
| Compression         | Big win — JSON is text                               | Smaller starting point; gzip/zstd still help on wide / repetitive cols, brotli usually skipped |
| Client cost         | `json.loads` + per-row dict construction             | `pyarrow.ipc.open_stream(...).read_all()` → zero-copy `pyarrow.Table`            |
| Best for            | Small responses, browsers, ad-hoc `curl`, dashboards | Bulk data into Polars / pandas / DuckDB-on-the-client, ML feature pipelines      |

## When to pick which

- **JSON** when the consumer is JavaScript, the response is small
  (≲ 10 k rows), or you're poking at the API by hand.
- **Arrow IPC** when you're moving result pages into a dataframe
  library, the schema has non-string types you want preserved, or
  page sizes are large enough that JSON parse time shows up in
  profiles.

## How to ask for Arrow

=== "Accept header"

    ```bash
    curl -X POST http://localhost:8080/api/v1/datasets/accidents/query \
      -H 'Content-Type: application/json' \
      -H 'Accept: application/vnd.apache.arrow.stream' \
      --output result.arrow \
      -d '{ "predicates": [{ "col": "State", "op": "eq", "val": "TX" }] }'
    ```

=== "Query string"

    ```bash
    curl -X POST 'http://localhost:8080/api/v1/datasets/accidents/query?format=arrow' \
      -H 'Content-Type: application/json' \
      --output result.arrow \
      -d '{ "predicates": [{ "col": "State", "op": "eq", "val": "TX" }] }'
    ```

## Reading Arrow IPC in Python

```python
import requests, pyarrow.ipc as ipc, polars as pl

r = requests.post(
    "http://localhost:8080/api/v1/datasets/accidents/query",
    json={"columns": ["ID","State"], "page_size": 1000},
    headers={"Accept": "application/vnd.apache.arrow.stream"},
)
table = ipc.open_stream(r.content).read_all()     # → pyarrow.Table
df    = pl.from_arrow(table)                      # zero-copy → Polars
page, size = int(r.headers["X-Page"]), int(r.headers["X-Page-Size"])
```

## Backend support

Both backends support Arrow IPC:

- **DuckDB** streams batches out via its native `query_arrow` API.
- **DataFusion** uses its Arrow plan directly.

Empty results still produce a valid stream (schema message only).
`Compress` middleware applies normally. `count`, `schema`, and the
dataset-listing endpoints are JSON-only.
