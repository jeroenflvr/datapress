---
description: >-
  Standalone DataPress clients — the datapress-cli command-line tool, the
  datap-rs-client Python package, and the datapress-client Rust crate — for
  talking to a running DataPress server.
---

# Clients

Besides the embedded [`datap-rs`](../python/index.md) wheel (which can both
*run* a server and talk to it), DataPress ships **standalone clients** that
only talk to an already-running server over HTTP. They share one lightweight
Rust core ([`datapress-client`](rust.md)) and are independent of the server
crates — no DuckDB or DataFusion is pulled in.

| Client                                     | Package          | Install                                   |
| ------------------------------------------ | ---------------- | ----------------------------------------- |
| [Command line](cli.md)                     | `datapress-cli`  | install script, `cargo install`           |
| [Python](python.md)                        | `datap-rs-client`| `uv pip install datap-rs-client[arrow]`   |
| [Rust library](rust.md)                    | `datapress-client`| `cargo add datapress-client`             |

All three speak the same [HTTP API](../reference/endpoints.md): list datasets,
fetch schemas, run structured queries (JSON or Arrow IPC), count rows, run raw
SQL, and reload datasets.

## Which one?

- **CLI** — shell scripts, ad-hoc inspection, piping JSON into `jq` or Arrow
  into a file.
- **Python** — notebooks and pipelines; `query_arrow()` returns a
  `pyarrow.Table` that feeds Polars, pandas, DuckDB, PySpark, and DataFusion
  zero-copy.
- **Rust** — embed the client in your own service or tool; async by default
  with an optional blocking wrapper.
