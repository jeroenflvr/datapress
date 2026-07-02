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

## Q4 2026

- **pgwire.** A native postgreSQL interface so PowerBI and other Postgres-speaking tools (Tableau, DBeaver, psql, pandas, Alteryx via postgreSQL ODBC, ..) can connect directly.

## Have a request?

Feature ideas and use cases are welcome — open an issue or discussion on
[GitHub](https://github.com/jeroenflvr/datapress).
