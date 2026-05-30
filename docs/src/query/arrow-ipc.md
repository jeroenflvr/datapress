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
| Compression         | Big win — JSON is text                               | Smaller starting point; gzip/brotli/zstd can still help on wide / repetitive cols; clients must decode HTTP compression before handing bytes to Arrow |
| Client cost         | `json.loads` + per-row dict construction             | `pyarrow.ipc.open_stream(...).read_all()` → zero-copy `pyarrow.Table`            |
| Best for            | Small responses, browsers, ad-hoc `curl`, dashboards | Bulk data into Polars / pandas / DuckDB-on-the-client, ML feature pipelines      |

## When to pick which

- **JSON** when the consumer is JavaScript, the response is small
  (≲ 10 k rows), or you're poking at the API by hand.
- **Arrow IPC** when you're moving result pages into a dataframe
  library, the schema has non-string types you want preserved, or
  page sizes are large enough that JSON parse time shows up in
  profiles.

## Response size and `max_body_bytes`

`page_size` is a **row-count** control, not a byte-count control. A
request with `"page_size": 1000` asks the backend for up to 1000 rows
for that page. The number of bytes in the Arrow IPC response depends on
what those rows contain:

- selected columns and their data types
- string/list/binary value lengths
- null bitmaps and offset buffers for variable-width columns
- Arrow stream metadata: schema, record-batch messages, and end marker
- optional HTTP compression when enabled and negotiated

`max_body_bytes` is unrelated to that response size. It limits the
incoming JSON request body, for example the bytes in:

```json
{ "columns": ["ID", "State"], "page_size": 1000 }
```

It does **not** limit, trim, or paginate the Arrow IPC stream returned
by the server. If your configuration says `max_body_bytes = 10_485_760`
and a `page_size = 1000` Arrow IPC query returns exactly 10 MiB, that is
not DataPress applying `max_body_bytes` to the response. It means those
1000 rows, with the selected columns and Arrow encoding overhead, happen
to serialize to about that size. No rows are silently dropped to fit the
request-body limit.

The precedence for `/query` is:

1. DataPress reads the incoming request body and rejects it with `413 Payload Too Large` if it exceeds `max_body_bytes`.
2. The query JSON is parsed.
3. `page_size` is clamped to `[1, server.max_page_size]` and combined with `page` and optional top-level `limit`.
4. The backend returns the selected page of rows.
5. The response encoder writes those rows as JSON or Arrow IPC.

To keep Arrow responses smaller, ask for fewer columns, lower
`page_size`, add predicates, or continue paging with the helper below.
Also check any reverse proxy in front of DataPress: proxies often have
their own request and response buffering limits, independent of
DataPress' `max_body_bytes`.

## HTTP compression

Arrow IPC is already a compact binary format, but DataPress can still
compress the HTTP response when `[server].compress = true` and the
client sends `Accept-Encoding`. For example, a client can ask for
Brotli with:

```http
Accept-Encoding: br
```

That compression is an HTTP transfer encoding around the Arrow IPC
stream. The Arrow stream itself is unchanged, but the bytes on the wire
are compressed. Therefore the client must pass **decompressed** bytes to
`pyarrow.ipc.open_stream()`. If compressed bytes are passed directly to
PyArrow, it will fail because the first bytes no longer look like an
Arrow IPC stream.

With `requests` and `httpx`, `response.content` is decompressed
automatically for supported `Content-Encoding` values. `gzip` and
`deflate` work out of the box. Brotli requires a Brotli decoder package
in the Python environment, such as `brotli` or `brotlicffi`, or an HTTP
client install that includes its Brotli extra. Without one, do not send
`Accept-Encoding: br`; request `gzip` or `identity`, or decompress the
body yourself before calling `ipc.open_stream()`.

When debugging, inspect `response.headers["Content-Encoding"]`. If it
is `br` and `ipc.open_stream(response.content)` throws, the client is
almost certainly still holding compressed bytes.

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

For a single page, read the Arrow IPC stream and pass the resulting
`pyarrow.Table` to Polars. When requesting Brotli with
`Accept-Encoding: br`, make sure your HTTP client has Brotli support so
`response.content` contains decompressed Arrow IPC bytes.

### Small `requests` example

```python
import pyarrow.ipc as ipc
import polars as pl
import requests

ARROW = "application/vnd.apache.arrow.stream"

response = requests.post(
    "http://localhost:8080/api/v1/datasets/accidents/query",
    json={"columns": ["ID", "State"], "page_size": 1000},
    headers={
        "Accept": ARROW,
        "Accept-Encoding": "br",
    },
)
response.raise_for_status()

table = ipc.open_stream(response.content).read_all()
df = pl.from_arrow(table)
page = int(response.headers["X-Page"])
size = int(response.headers["X-Page-Size"])
```

### Small `httpx` example

```python
import httpx
import pyarrow.ipc as ipc
import polars as pl

ARROW = "application/vnd.apache.arrow.stream"

response = httpx.post(
    "http://localhost:8080/api/v1/datasets/accidents/query",
    json={"columns": ["ID", "State"], "page_size": 1000},
    headers={
        "Accept": ARROW,
        "Accept-Encoding": "br",
    },
    timeout=60.0,
)
response.raise_for_status()

table = ipc.open_stream(response.content).read_all()
df = pl.from_arrow(table)
```

### Async `httpx` with count + gather

For a complete result set, first call `/count` with the same predicates,
compute the page numbers, then fetch those pages with `asyncio.gather`.
This works well for bounded fan-out, such as 30-100 pages.
`asyncio.gather` preserves result order, so concatenating the returned
tables keeps pages in ascending order. Include a deterministic `order_by`
so page boundaries stay stable.

```python
import asyncio
import math

import httpx
import pyarrow as pa
import pyarrow.ipc as ipc
import polars as pl

ARROW = "application/vnd.apache.arrow.stream"


async def query_all_polars_httpx(
    base_url: str,
    dataset: str,
    body: dict,
    *,
    page_size: int,
) -> pl.DataFrame:
    base = base_url.rstrip("/")
    count_body = {"predicates": body.get("predicates", [])}

    async with httpx.AsyncClient(timeout=60.0) as client:
        count_response = await client.post(
            f"{base}/api/v1/datasets/{dataset}/count",
            json=count_body,
        )
        count_response.raise_for_status()

        total_rows = int(count_response.json()["count"])
        if total_rows == 0:
            return pl.DataFrame()

        page_count = math.ceil(total_rows / page_size)

        async def fetch_page(page: int) -> pa.Table:
            response = await client.post(
                f"{base}/api/v1/datasets/{dataset}/query",
                json={**body, "page": page, "page_size": page_size},
                headers={
                    "Accept": ARROW,
                    # Requires httpx Brotli support, for example the
                    # httpx brotli extra, brotli, or brotlicffi.
                    "Accept-Encoding": "br",
                },
            )
            response.raise_for_status()

            # response.content must be decompressed before PyArrow sees it.
            # httpx does this for Brotli only when Brotli support is installed.
            return ipc.open_stream(response.content).read_all()

        tables = await asyncio.gather(
            *(fetch_page(page) for page in range(1, page_count + 1))
        )

    table = tables[0] if len(tables) == 1 else pa.concat_tables(tables)
    return pl.from_arrow(table)


# Fully async version: the /count docs show this predicate at about
# 418k rows, so page_size=10_000 produces roughly 42 Arrow IPC requests.
df_async = asyncio.run(query_all_polars_httpx(
    "http://localhost:8080",
    "accidents",
    {
        "columns": ["ID", "State", "Severity", "Start_Time"],
        "predicates": [
            {"col": "State", "op": "in", "val": ["CA", "TX"]},
            {"col": "Severity", "op": "gte", "val": 3},
        ],
        "order_by": [{"col": "ID"}],
    },
    page_size=10_000,
))
```

## Backend support

Both backends support Arrow IPC:

- **DuckDB** streams batches out via its native `query_arrow` API.
- **DataFusion** uses its Arrow plan directly.

Empty results still produce a valid stream (schema message only).
`Compress` middleware applies normally. `count`, `schema`, and the
dataset-listing endpoints are JSON-only.
