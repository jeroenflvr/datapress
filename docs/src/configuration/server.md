# Server settings

The `[server]` block is fully optional — every field has a sensible
default.

```toml
[server]
backend = "datafusion"    # "datafusion" (default) | "duckdb"
listen  = "127.0.0.1"     # bind address; "0.0.0.0" to expose
port    = 8080
# workers = 8             # omit for one worker per CPU
# prefix  = "/datapress"  # mount every route under this path
# compress = true
# max_body_bytes        = 1048576    # 413 above this
# max_page_size         = 100000     # clamp query page_size above this
# request_timeout_ms    = 30000      # 504 above this; 0 disables
# shutdown_timeout_secs = 30         # SIGTERM grace period

[server.quack]                      # DuckDB backend only; experimental
enabled = false
uri = "quack:localhost"             # default port 9494; use literal localhost
# token = "change-me"               # optional; generated and logged if omitted
allow_other_hostname = false        # true for quack:0.0.0.0:9494 behind TLS proxy
read_only = true                    # allow reads plus Quack attach handshake
```

## Reference

| Field                   | Default     | Notes                                                                                     |
|-------------------------|-------------|-------------------------------------------------------------------------------------------|
| `backend`               | `datafusion`| Informational hint logged at startup. Each binary always runs as its own backend regardless. |
| `listen`                | `127.0.0.1` | Loopback by default — the service is **not** network-exposed unless you opt in.           |
| `port`                  | `8080`      | TCP port.                                                                                 |
| `workers`               | *(unset)*   | Actix worker threads. Unset = one per CPU.                                                |
| `prefix`                | `""`        | URL prefix in front of every app route. Must start with `/` and not end with `/`.         |
| `compress`              | `true`      | Negotiate gzip / brotli / zstd via `Accept-Encoding`.                                     |
| `max_body_bytes`        | `1048576`   | Max accepted JSON request body. Larger → `413 Payload Too Large`.                         |
| `max_page_size`         | `100000`    | Max rows returned by one `/query` page. Larger `page_size` values are clamped.             |
| `request_timeout_ms`    | `30000`     | Per-request handler timeout (ms). Long handlers are cancelled and the client gets `504`. `0` disables. |
| `shutdown_timeout_secs` | `30`        | Grace period for in-flight requests after `SIGTERM` / `SIGINT`.                           |

## DuckDB Quack server

DuckDB builds can optionally start DuckDB's experimental <sup>1</sup> Quack remote
protocol server after datasets are registered:

```toml
[server]
backend = "duckdb"

[server.quack]
enabled = true
uri = "quack:localhost"       # default port 9494
token = "analytics-token"     # optional, but recommended
read_only = true              # default
```

Quack exposes the DuckDB SQL surface of the in-process database. DataPress
therefore keeps it disabled by default, binds to localhost by default, and
installs a read-only authorization hook by default. If `token` is omitted,
Quack generates one at startup and DataPress logs it once.

With `read_only = true`, DataPress allows read/inspection statements and
the Quack client attach handshake, but rejects write-oriented and DDL
statements such as `CREATE`, `INSERT`, `UPDATE`, `DELETE`, `COPY`,
`DROP`, `ALTER`, `LOAD`, and `INSTALL`.

DuckDB's Quack extension currently treats only the literal hostname
`localhost` as local. Use `uri = "quack:localhost"`; `quack:127.0.0.1`
is rejected unless `allow_other_hostname = true`.

To listen on a non-local address, set both a non-local URI and
`allow_other_hostname = true`, then put a TLS-terminating reverse proxy in
front of it:

```toml
[server.quack]
enabled = true
uri = "quack:0.0.0.0:9494"
allow_other_hostname = true
token = "analytics-token"
```

DuckDB CLI clients can connect with a Quack secret:

```sql
CREATE SECRET (
	TYPE quack,
	TOKEN 'analytics-token',
	SCOPE 'quack:localhost'
);

ATTACH 'quack:localhost' AS datapress (TYPE quack);
FROM datapress.accidents LIMIT 10;
```

Or simplified, using the secret directly in ATTACH statement:

```sql
ATTACH 'quack:localhost' AS datapress (TOKEN 'analytics-token');
FROM datapress.accidents LIMIT 10;
```

### Connecting to a remote host

For any host other than `localhost`, Quack defaults to **HTTPS**. If the
server is reached over plain HTTP (for example in development, or before a
TLS-terminating proxy is in place), the attach will fail unless you pass
`disable_ssl true`:

```sql
ATTACH 'quack:remote_ip' AS remote_db (TOKEN 'analytics-token', disable_ssl true);
FROM remote_db.accidents LIMIT 10;
```

Omit `disable_ssl true` (the default) when the server is fronted by TLS.

<sup>1</sup> Quack is still highly experimental. Among other things, `SHOW TABLES;` is not yet supported.


## Behind a reverse proxy

When nginx / Traefik / Caddy forwards a path prefix verbatim, set
`prefix` so app routes match:

```toml
[server]
prefix = "/datapress"
# → GET /datapress/api/v1/datasets, GET /datapress/health, ...
```

The unprefixed probes — `/healthz`, `/readyz`, `/version` — stay at the
bare host root regardless. That way orchestrators don't need to know how
the service is exposed.

## Compression

On by default and negotiated per request via `Accept-Encoding`. Clients
that want raw JSON send `Accept-Encoding: identity` or omit the header.
Disable when sitting behind a proxy that already compresses, or to save
CPU on a trusted LAN.

```toml
[server]
compress = false
```

## Request size limit

```toml
[server]
max_body_bytes = 10_485_760   # 10 MiB
```

`max_body_bytes` is an **incoming request-body** limit. It applies to
the bytes the client sends to DataPress: for example the JSON body of a
`POST /api/v1/datasets/{name}/query` request. It is wired into both
Actix's JSON extractor and raw payload extractor (`web::JsonConfig` and
`web::PayloadConfig`). Oversized requests are rejected with
`413 Payload Too Large` before the query handler runs.

It is not a response-size limit. DataPress does not truncate JSON or
Arrow IPC responses at `max_body_bytes`, and it does not drop rows to
make a response fit that value. Response size is determined by the query
result: selected columns, row count, Arrow/JSON encoding overhead, and
optional HTTP compression.

For query requests the order is:

1. The HTTP request body must fit within `max_body_bytes`.
2. The JSON body is parsed into the query request.
3. `page` is normalized to at least `1`; `page_size` is clamped to `[1, max_page_size]`.
4. The backend applies `page`, `page_size`, and optional top-level `limit` to choose rows.
5. The chosen rows are encoded as JSON or Arrow IPC.

That means a small query body can legitimately produce a much larger
response. If `max_body_bytes = 10_485_760` and an Arrow IPC query with
`page_size = 1000` returns about 10 MiB, the two numbers only correlate
by coincidence unless the client, proxy, or load balancer has its own
separate response-size limit. DataPress itself uses `max_body_bytes` only
on the request side.

To control response size, reduce `page_size`, project fewer `columns`,
add more selective `predicates`, or page through the result set. See
[Arrow IPC vs JSON](../query/arrow-ipc.md#response-size-and-max_body_bytes)
for the Arrow-specific details.

## Query page-size limit

```toml
[server]
max_page_size = 100_000
```

`max_page_size` controls the largest row page a `/query` request can
ask for. The default is `100_000`. If a client sends a larger
`page_size`, DataPress clamps it to `max_page_size`; the response reports
the effective value in the JSON body or Arrow IPC `X-Page-Size` header.

This is separate from `max_body_bytes`: `max_page_size` limits rows in
the response page, while `max_body_bytes` limits bytes in the incoming
request body.

## Request timeout

```toml
[server]
request_timeout_ms = 60_000   # 60 s
# request_timeout_ms = 0      # disabled
```

A handler that doesn't produce a response within `request_timeout_ms`
is cancelled at the next `.await` point and the client sees
`504 Gateway Timeout` with body `{"error":"request timed out"}`.

## Graceful shutdown

```toml
[server]
shutdown_timeout_secs = 30
```

On `SIGTERM` or `SIGINT`:

1. The listening socket is closed — no new connections.
2. In-flight requests get up to `shutdown_timeout_secs` to drain.
3. Workers are stopped.

Set this **lower** when fast restarts matter more than slow handlers;
set it **higher** for long-running aggregations or large parquet
exports. The startup log records which signal triggered shutdown:

```
INFO  Received SIGTERM, shutting down gracefully (up to 30s for in-flight requests)...
INFO  Shutdown complete.
```

See [Operations › Graceful shutdown](../operations/graceful-shutdown.md)
for the orchestrator-side tuning.
