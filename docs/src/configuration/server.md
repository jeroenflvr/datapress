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
# request_timeout_ms    = 30000      # 504 above this; 0 disables
# shutdown_timeout_secs = 30         # SIGTERM grace period
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
| `request_timeout_ms`    | `30000`     | Per-request handler timeout (ms). Long handlers are cancelled and the client gets `504`. `0` disables. |
| `shutdown_timeout_secs` | `30`        | Grace period for in-flight requests after `SIGTERM` / `SIGINT`.                           |

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

Applies to both JSON and raw payloads (`web::JsonConfig` and
`web::PayloadConfig`). Rejects oversized bodies before the handler is
ever called.

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
