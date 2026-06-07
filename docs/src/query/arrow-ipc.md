# Arrow IPC vs JSON

DataPress has two Arrow IPC modes:

- **Paged Arrow**: `POST /query` with `Accept: application/vnd.apache.arrow.stream`
    or `?format=arrow`. This returns one requested page as an Arrow IPC
    stream. Clients still make one request per page.
- **Full-result Arrow stream**: `POST /query/stream`. This returns one
    Arrow IPC stream for the full matching result set in a single HTTP
    response. It ignores `page` and `page_size`; use `limit` to cap rows.

Both modes use the same Arrow IPC wire format: one schema message, zero
or more `RecordBatch` messages, then an end marker. The difference is
which rows the server selects before it starts writing the stream.

## What Streaming Means

Arrow IPC is a stream format, and DataPress writes it to the HTTP
response as chunks. This avoids building one complete response buffer in
memory before sending bytes to the client.

That does not always mean the query engine itself is a server-side
cursor:

- DuckDB uses its native Arrow iterator and writes batches directly into
    the HTTP response stream.
- DataFusion writes Arrow batches into the HTTP response stream. For SQL
    fallback paths, DataFusion may still collect execution batches before
    DataPress encodes them, but DataPress no longer concatenates them into
    one giant batch or buffers the full Arrow IPC response.

Client code can usually read either mode the same way:

```python
table = pyarrow.ipc.open_stream(response.content).read_all()
```

For very large results, prefer `/query/stream` with a sensible `limit`,
or consume the HTTP response incrementally with an HTTP client that
supports streaming bytes.

## Comparison

| Aspect              | JSON `/query`                                        | Paged Arrow `/query`                                                             | Full Arrow `/query/stream`                                                      |
|---------------------|------------------------------------------------------|----------------------------------------------------------------------------------|---------------------------------------------------------------------------------|
| Content-Type        | `application/json`                                   | `application/vnd.apache.arrow.stream`                                            | `application/vnd.apache.arrow.stream`                                           |
| How to ask          | Default                                              | `Accept: application/vnd.apache.arrow.stream` or `?format=arrow`                 | Call `/query/stream`                                                            |
| Rows returned       | One page                                             | One page                                                                         | All matching rows, optionally capped by `limit`                                 |
| Uses `page`         | Yes                                                  | Yes                                                                              | No                                                                              |
| Uses `page_size`    | Yes, clamped to `server.max_page_size`               | Yes, clamped to `server.max_page_size`                                           | No                                                                              |
| Uses `limit`        | Caps total rows across pages                         | Caps total rows across pages                                                     | Caps total rows in the single stream                                            |
| Shape               | `{ "data": [{...}], "page": N, "page_size": M }`    | Arrow IPC stream: schema + batches + end marker                                  | Arrow IPC stream: schema + batches + end marker                                 |
| Page metadata       | In the body                                          | Headers `X-Page`, `X-Page-Size`                                                  | None                                                                            |
| Empty result        | `{ "data": [], "page": ..., "page_size": ... }`      | Valid stream with the schema message only, zero batches                          | Valid stream with the schema message only, zero batches                         |
| Best for            | Small UI/API responses                               | Dataframe clients that want explicit paging                                      | Dataframe clients that want one request for a bounded export                    |

## When to pick which

- **JSON** when the consumer is JavaScript, the response is small
  (≲ 10 k rows), or you're poking at the API by hand.
- **Paged Arrow IPC** when you want dataframe-friendly pages, bounded
    memory per request, retryable page fetches, or parallel page downloads.
- **`/query/stream`** when you want one HTTP request for the full
    filtered result set and can consume an Arrow IPC stream on the client.

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
For `/query/stream`, use `limit` to cap the total rows returned by the
single streaming response.
Also check any reverse proxy in front of DataPress: proxies often have
their own request and response buffering limits, independent of
DataPress' `max_body_bytes`.

## Full-result stream

Use `/query/stream` when you want one request that streams the full
matching result set as Arrow IPC:

```bash
curl -X POST http://localhost:8080/api/v1/datasets/accidents/query/stream \
    -H 'Content-Type: application/json' \
    --output result.arrow \
    -d '{
        "columns": ["ID", "State", "Severity", "Start_Time"],
        "predicates": [{ "col": "State", "op": "in", "val": ["CA", "TX"] }],
        "order_by": [{ "col": "ID" }],
        "limit": 100000
    }'
```

`/query/stream` always returns `application/vnd.apache.arrow.stream`.
It does not include `X-Page` or `X-Page-Size`, because there is no
server-side page boundary. The optional top-level `limit` still caps the
total number of rows in the stream.

The request flow is different from paged `/query`:

```text
Paged /query:
request page 1 -> Arrow stream for page 1 -> done
request page 2 -> Arrow stream for page 2 -> done
request page 3 -> Arrow stream for page 3 -> done

Full /query/stream:
one request -> Arrow stream for every matching row -> done
```

Use a deterministic `order_by` for either mode when stable row order
matters. For paged `/query`, it keeps page boundaries stable. For
`/query/stream`, it makes repeated exports easier to compare.

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

For a single page or a full-result stream, read the Arrow IPC stream and
pass the resulting `pyarrow.Table` to Polars. When requesting Brotli with
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

### One-request stream with `httpx`

```python
import httpx
import pyarrow.ipc as ipc
import polars as pl

response = httpx.post(
    "http://localhost:8080/api/v1/datasets/accidents/query/stream",
    json={
        "columns": ["ID", "State", "Severity", "Start_Time"],
        "predicates": [{"col": "State", "op": "in", "val": ["CA", "TX"]}],
        "order_by": [{"col": "ID"}],
        "limit": 100_000,
    },
    timeout=60.0,
)
response.raise_for_status()

table = ipc.open_stream(response.content).read_all()
df = pl.from_arrow(table)
```

That example still buffers the HTTP response in `response.content`
before PyArrow reads it. The server streams the response, but the client
chooses to materialize the bytes. For larger responses, stream bytes into
a file-like buffer first:

```python
import tempfile

import httpx
import pyarrow.ipc as ipc
import polars as pl

with tempfile.SpooledTemporaryFile(max_size=256 * 1024 * 1024) as file:
    with httpx.stream(
        "POST",
        "http://localhost:8080/api/v1/datasets/accidents/query/stream",
        json={
            "columns": ["ID", "State", "Severity", "Start_Time"],
            "predicates": [{"col": "State", "op": "in", "val": ["CA", "TX"]}],
            "order_by": [{"col": "ID"}],
            "limit": 100_000,
        },
        timeout=60.0,
    ) as response:
        response.raise_for_status()
        for chunk in response.iter_bytes():
            file.write(chunk)

    file.seek(0)
    table = ipc.open_stream(file).read_all()

df = pl.from_arrow(table)
```

`SpooledTemporaryFile` keeps small responses in memory and spills larger
ones to disk. HTTP compression is still decoded by `httpx` before chunks
are yielded, provided the needed decoder is installed.

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

- **DuckDB** streams batches out via its native `query_arrow` API and
    writes them directly into the HTTP response stream.
- **DataFusion** uses its Arrow plan directly and writes Arrow IPC bytes
    through the same HTTP streaming response path.

Empty results still produce a valid stream (schema message only).
`Compress` middleware applies normally. `count`, `schema`, and the
dataset-listing endpoints are JSON-only.

## Reading Arrow IPC in the browser

The built-in explorer UI (served at `/explore` when DataPress is built
with the `explorer` feature) decodes Arrow IPC responses directly in the
browser on its **API Query** tab, using a vendored Apache Arrow JS bundle.

DataFusion emits Arrow `Utf8View` for Parquet string columns. Published
`apache-arrow` npm releases (through 21.x) cannot decode `Utf8View` and
fail with `Unrecognized type: "undefined" (24)`. Read support for
`Utf8View`/`BinaryView` was added in
[apache/arrow-js#320](https://github.com/apache/arrow-js/pull/320), which
is merged on `main` but not yet in any published release.

Because of this, DataPress currently **builds the Apache Arrow JS bundle
from source** (from a pinned `apache/arrow-js` commit) rather than
downloading a published npm release. See the `docs:vendor-arrow` task in
`Taskfile.yml`. Once a release including #320 ships, the pinned commit
can be bumped to that release.

