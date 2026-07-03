---
description: >-
  Connect to a DataPress server over the PostgreSQL wire protocol (pgwire).
  Query your Parquet and Delta datasets from psql, Postgres drivers, and BI
  tools like Power BI and Tableau — read-only, DataFusion backend, opt-in.
---

# PostgreSQL wire protocol (pgwire)

The **DataFusion** backend can expose a native **PostgreSQL wire-protocol**
endpoint alongside the HTTP API. Any PostgreSQL client — `psql`, JDBC/ODBC
drivers, or BI tools such as Power BI, Tableau, and Metabase — can then query
your datasets directly, without knowing anything about the DataPress HTTP API.

!!! warning "Experimental"
    The PostgreSQL wire-protocol endpoint is **experimental**. It relies on
    emulated `pg_catalog`/`information_schema` metadata, so some clients and
    introspection queries may not work yet, and its behaviour and configuration
    can change between releases. It's ready for querying datasets from `psql`
    and common BI tools, but not recommended for production-critical workloads.
    Please [report](https://github.com/jeroenflvr/datapress/issues) any client
    that fails to connect or browse schemas.

Each registered dataset appears as a table in the default catalog/schema
(`public` to Postgres clients), so `SELECT * FROM my_dataset` just works.

- **Read-only** — DataFusion has no write path here, so the endpoint serves
  `SELECT` / `WITH … SELECT` queries only.
- **DataFusion only** — the DuckDB backend has no pgwire path.
- **Opt-in** — available only when the binary/wheel was built with the `pgwire`
  Cargo feature, and disabled by default even then.

!!! note "Requires the `pgwire` build feature"
    The endpoint is compiled in only when the server binary (or the `datap-rs`
    wheel) is built with the `pgwire` Cargo feature and run on the
    `datafusion` backend. A `pgwire.enabled = true` config on a build without
    the feature logs a warning at startup and is otherwise a no-op.

## Enable the endpoint

Turn it on with the [`[server.pgwire]`](../configuration/server.md#postgresql-wire-protocol-pgwire)
block (or the `pgwire_*` kwargs when [launching from Python](../python/config.md#postgresql-wire-protocol-pgwire)):

=== "TOML"

    ```toml
    [server]
    backend = "datafusion"

    [server.pgwire]
    enabled  = true
    listen   = "127.0.0.1"
    port     = 5432
    username = "datapress"
    password = "change-me"
    ```

=== "Python"

    ```python
    from datap_rs.datapress import DataPressConfig

    cfg = DataPressConfig(
        backend="datafusion",
        pgwire_enabled=True,
        pgwire_listen="127.0.0.1",
        pgwire_port=5432,
        pgwire_username="datapress",
        pgwire_password="change-me",
    )
    ```

Authentication is **cleartext password**, so DataPress enforces these rules at
startup (the process refuses to start otherwise):

- A **loopback** bind (`127.0.0.1` / `::1`) may omit the password — handy for
  local development.
- Any **non-loopback** bind (for example `0.0.0.0`) **requires** both a
  password **and** TLS, so credentials never cross the network in the clear.

!!! warning "TLS for network-exposed listeners"
    To accept connections from other hosts, bind to a non-loopback address and
    provide a certificate and key. Without TLS a non-loopback listener is
    refused at startup.

    ```toml
    [server.pgwire]
    enabled  = true
    listen   = "0.0.0.0"
    port     = 5432
    username = "datapress"
    password = "change-me"
    tls_cert = "/etc/datapress/pg.crt"   # PEM certificate
    tls_key  = "/etc/datapress/pg.key"   # PKCS#8 private key
    ```

## Connect with `psql`

```bash
psql "host=127.0.0.1 port=5432 user=datapress password=change-me dbname=datapress" \
  -c "SELECT count(*) FROM accidents"
```

Or interactively:

```bash
psql "host=127.0.0.1 port=5432 user=datapress password=change-me dbname=datapress"
datapress=> SELECT severity, count(*) FROM accidents GROUP BY severity ORDER BY 2 DESC;
```

The `dbname` is not meaningful to DataFusion (there is a single catalog); any
value is accepted. When TLS is enabled, add `sslmode=require`:

```bash
psql "host=db.example.com port=5432 user=datapress password=change-me dbname=datapress sslmode=require"
```

## Connection strings

Most drivers accept a standard PostgreSQL URI:

```text
postgresql://datapress:change-me@127.0.0.1:5432/datapress
```

With TLS:

```text
postgresql://datapress:change-me@db.example.com:5432/datapress?sslmode=require
```

## BI tools

!!! note "BI connectors usually require TLS"
    Power BI's PostgreSQL connector (Npgsql) defaults to **Encrypt
    connection = on**, and many other BI drivers require SSL by default. A
    DataPress listener without TLS will refuse those connections, so configure
    `tls_cert` / `tls_key` (or disable encryption in the client for local,
    loopback testing).

- **Power BI** — *Get Data → PostgreSQL database*. Enter the server as
  `host:port` (e.g. `db.example.com:5432`) and the database as `datapress`.
  Keep *Encrypt connection* on and use a TLS-enabled listener.
- **Tableau / Metabase / Superset** — choose the **PostgreSQL** connector and
  supply host, port `5432`, database `datapress`, and the configured
  username/password.

## DBeaver / DataGrip

Create a standard **PostgreSQL** connection (not a DataPress-specific driver):

1. Host `127.0.0.1`, Port `5432`, Database `datapress`.
2. Username `datapress`, Password as configured.
3. For a TLS listener, set SSL mode to `require` in the driver's SSL settings.

Tables (your datasets) appear under the `public` schema. Queries are read-only
`SELECT`s; DDL/DML is not supported.

## Notes and limits

- Datasets are exposed as tables in the default catalog/schema; column names
  and types come from each dataset's Arrow schema.
- Only read queries are supported — there is no `INSERT`/`UPDATE`/`DELETE` or
  DDL.
- The endpoint reflects the datasets currently registered with the server; use
  the HTTP [reload](../operations/reload.md) / [register](../operations/register.md)
  operations to change what is visible.
- **Connection pooling is supported.** Pooling drivers (notably Npgsql, used by
  Power BI) reset a pooled connection by issuing session-maintenance statements
  such as `DISCARD ALL`. DataFusion doesn't model these, so DataPress
  transparently acknowledges `DISCARD`, `DEALLOCATE`, `RESET`, and `UNLISTEN` as
  no-ops (transactions — `BEGIN`/`COMMIT`/`ROLLBACK` — are likewise accepted but
  not real transactions, since the endpoint is read-only). You should not need
  to disable pooling.
