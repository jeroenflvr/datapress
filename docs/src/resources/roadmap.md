---
description: >-
  What's planned next for DataPress — upcoming features by quarter, including a
  JDBC driver landing in Q3 2026.
---

# Roadmap

Where DataPress is headed. Dates are targets, not commitments, and the list is
refreshed as work lands. For shipped changes see the
[Changelog](../reference/changelog.md).

## Q3 2026

- **JDBC driver.** A first-class JDBC driver so JVM tools — BI clients, SQL
  IDEs, and JDBC-based ETL — can connect to a DataPress server directly and
  query datasets over the standard `java.sql` API. This complements the
  existing HTTP JSON / Arrow IPC surface and the DuckDB-native
  [Quack protocol](../backends/duckdb.md#quack-remote-protocol) with a driver
  the wider Java ecosystem can use out of the box.

- **pgwire.** A native postgreSQL interface so PowerBI and other Postgres-speaking tools (Tableau, DBeaver, psql, pandas, Alteryx via postgreSQL ODBC, ..) can connect directly.

- **MCP 2026-07-28 revision.** Support the stateless-core revision of the
  Model Context Protocol as a second accepted protocol version alongside the
  current 2025-11-25 stable revision. Key additions: `Mcp-Method` /
  `Mcp-Name` headers and the stateless request model. The transport layer is
  already structured for this addition (protocol version isolated in
  `mcp/http.rs`).

- **Per-query engine resource caps.** Bound agent-written joins and
  aggregations with engine-level memory limits: `memory_limit` for DuckDB and
  a DataFusion memory pool cap. This prevents a poorly-written SQL tool call
  from exhausting server memory; the current `request_timeout_ms` only bounds
  wall-clock time.

## Q4 2026

- PowerBI [DirectQuery](https://learn.microsoft.com/en-us/power-bi/connect-data/desktop-use-directquery) support.  

## Have a request?

Feature ideas and use cases are welcome — open an issue or discussion on
[GitHub](https://github.com/jeroenflvr/datapress).
