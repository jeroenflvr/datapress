# Logging

Logging is via the standard `env_logger` crate. Control verbosity with
`RUST_LOG`:

```bash
RUST_LOG=info   ./target/release/datapress-duckdb
RUST_LOG=debug  ./target/release/datapress-datafusion
RUST_LOG=warn,datapress_core=debug ./target/release/datapress-duckdb
```

If unset, logging is effectively off — set at least `RUST_LOG=info`
in production.

## Request log format

Every HTTP request emits one line via actix's `Logger` middleware:

```
%a "%r" %s %b bytes %Dms
```

| Token | Meaning                              |
|-------|--------------------------------------|
| `%a`  | Remote IP                            |
| `%r`  | First line of request (method + path + version) |
| `%s`  | Response status                      |
| `%b`  | Response size in bytes               |
| `%D`  | Time-to-respond in milliseconds      |

Example:

```
INFO  actix_web::middleware::logger 10.0.0.42 "POST /api/v1/datasets/accidents/query HTTP/1.1" 200 81234 bytes 12ms
```

## Startup log

On boot, DataPress logs:

- The bind address (e.g. `Listening on http://0.0.0.0:8080`).
- The worker count.
- The active backend.
- The full route table (versioned + legacy + probes).
- `shutdown_timeout_secs` and `request_timeout_ms` settings.

This is invaluable when debugging "why doesn't `/foo` work?" — the
answer is usually right there in the route table.

## Trade-offs

- The logger middleware adds ~1 µs per request; negligible at any
  realistic QPS.
- Logging at `debug` is noisy on hot paths (per-batch logs from the
  DataFusion scan). Use it for development only.
