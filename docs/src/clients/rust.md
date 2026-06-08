---
description: >-
  datapress-client: an async + blocking Rust client for a DataPress server,
  wrapping the JSON and Arrow IPC HTTP endpoints. Lightweight, no server crates.
---

# Rust client (`datapress-client`)

Async + blocking Rust client for a running DataPress server, wrapping the JSON
and Arrow IPC HTTP endpoints. It is lightweight — only `reqwest` + `serde` by
default — and does not depend on the server crates (no DuckDB or DataFusion).

## Install

```bash
cargo add datapress-client
```

```toml
# Async-only, reqwest + serde (no Arrow decode, no blocking wrapper):
datapress-client = { version = "0.4", default-features = false }
```

## Features

- `arrow` *(default)* — decode Arrow IPC stream responses into
  `arrow::record_batch::RecordBatch`.
- `blocking` *(default)* — synchronous `blocking::Client` backed by a private
  current-thread Tokio runtime.

## Async

```rust,no_run
use datapress_client::{Client, QueryRequest, Predicate};

# async fn run() -> datapress_client::Result<()> {
let client = Client::new("http://127.0.0.1:8000")?;
let names = client.datasets().await?;

let req = QueryRequest::builder()
    .columns(["State", "Severity"])
    .predicate(Predicate::new("Severity", "gte", 3))
    .page_size(10_000)
    .build();
let resp = client.query_json("accidents", &req).await?;
println!("{} rows", resp.data.len());
# Ok(())
# }
```

## Blocking

```rust
use datapress_client::blocking::Client;

# fn run() -> datapress_client::Result<()> {
let client = Client::new("http://127.0.0.1:8000")?;
let count = client.count("accidents", &[])?;
println!("{count} rows");
# Ok(())
# }
```

## Arrow

```rust
# use datapress_client::{Client, QueryRequest};
# async fn run(client: Client, req: QueryRequest) -> datapress_client::Result<()> {
let batches = client.query_arrow("accidents", &req).await?;
for batch in &batches {
    println!("{} rows x {} cols", batch.num_rows(), batch.num_columns());
}
# Ok(())
# }
```

## Endpoints covered

| Method                 | Endpoint                                              |
| ---------------------- | ----------------------------------------------------- |
| `healthz` / `readyz`   | `GET /healthz`, `GET /readyz` (root)                  |
| `datasets`             | `GET {api}/datasets`                                  |
| `schema`               | `GET {api}/datasets/{name}/schema`                    |
| `count`                | `POST {api}/datasets/{name}/count`                    |
| `query_json`           | `POST {api}/datasets/{name}/query`                    |
| `query_arrow`          | `POST {api}/datasets/{name}/query/stream` (Arrow IPC) |
| `sql`                  | `POST {api}/sql`                                      |
| `reload`               | `POST {api}/datasets/{name}/reload`                   |

`{api}` defaults to `/api/v1` (configurable via `ClientBuilder::api_base`).
