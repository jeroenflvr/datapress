---
description: >-
  Standalone DataPress clients — the datapress-cli command-line tool, the
  datap-rs-client Python package, the datapress-client Rust crate, the
  datapress-jdbc JDBC driver, and the PostgreSQL wire-protocol endpoint — for
  talking to a running DataPress server.
---

# Clients

Besides the embedded [`datap-rs`](../python/index.md) wheel (which can both
*run* a server and talk to it), DataPress ships **standalone clients** that
only talk to an already-running server over HTTP. The CLI, Python, and Rust
clients share one lightweight Rust core ([`datapress-client`](rust.md)) and are
independent of the server crates — no DuckDB or DataFusion is pulled in. A
separate pure-Java [JDBC driver](jdbc.md) lets BI tools connect over standard
`java.sql`. The DataFusion backend can also expose a
[PostgreSQL wire-protocol endpoint](postgresql.md) that **any** Postgres client
can talk to directly.

| Client                                     | Package          | Install                                   |
| ------------------------------------------ | ---------------- | ----------------------------------------- |
| [Command line](cli.md)                     | `datapress-cli`  | install script, `cargo install`           |
| [Python](python.md)                        | `datap-rs-client`| `uv pip install datap-rs-client[arrow]`   |
| [Rust library](rust.md)                    | `datapress-client`| `cargo add datapress-client`             |
| [JDBC driver](jdbc.md)                     | `datapress-jdbc` | Maven Central `org.datap-rs:datapress-jdbc` |
| [PostgreSQL (pgwire)](postgresql.md)       | *(any PG client)*| built-in; enable `[server.pgwire]`        |

The CLI, Python, and Rust clients speak the same
[HTTP API](../reference/endpoints.md): list datasets, fetch schemas, run
structured queries (JSON or Arrow IPC), count rows, run raw SQL, and reload
datasets. The JDBC driver exposes the raw-SQL path through `java.sql`. The
PostgreSQL endpoint speaks the native Postgres protocol instead of HTTP.

## Which one?

- **CLI** — shell scripts, ad-hoc inspection, piping JSON into `jq` or Arrow
  into a file.
- **Python** — notebooks and pipelines; `query_arrow()` returns a
  `pyarrow.Table` that feeds Polars, pandas, DuckDB, PySpark, and DataFusion
  zero-copy.
- **Rust** — embed the client in your own service or tool; async by default
  with an optional blocking wrapper.
- **JDBC** — connect BI/SQL tools (DBeaver, DataGrip) and JVM apps over standard
  `java.sql`; read-only `SELECT` streamed as Arrow. Requires the server's raw-SQL
  endpoint to be enabled.
- **PostgreSQL (pgwire)** — point `psql`, Postgres drivers, or BI tools (Power
  BI, Tableau) at DataPress using the native PostgreSQL protocol. DataFusion
  backend only, opt-in via the `pgwire` build feature.
