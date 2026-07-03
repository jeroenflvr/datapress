# DataPress vs alternatives

DataPress is not the only way to put an HTTP query layer in front of columnar
files. This page explains where it fits relative to the options you are most
likely to evaluate.

The short version: DataPress is a **read-only publication layer** for data you
already curate as Parquet or Delta. It intentionally does less than database
products that also handle writes, governance, and broad federation.

## At a glance

| | **DataPress** | **[ROAPI](https://roapi.github.io/docs/)** | **[Seafowl](https://seafowl.io/)** | **[Datasette](https://datasette.io/)** |
|---|---|---|---|---|
| Language / engine | Rust · DuckDB and Arrow+DataFusion | Rust · DataFusion | Rust · DataFusion + delta-rs | Python · SQLite |
| Query interface | Structured JSON predicates (raw SQL opt-in) | SQL · GraphQL · REST · FlightSQL | SQL (HTTP) | SQL · JSON REST · web UI |
| PostgreSQL wire protocol | **Yes** (DataFusion backend, opt-in) | No | No | No |
| Client drivers / SDKs | **First-party CLI, Python, Rust, REST, HTTP, SQL clients; JDBC driver; any Postgres driver via pgwire** (Tableau, PowerBI, DBeaver, ..) | FlightSQL / REST clients | HTTP / SQL clients | Python API · HTTP |
| Engine choice | **Two, interchangeable, identical API** | One | One | One |
| Native sources | Parquet, Delta (local / S3) | Parquet, CSV, JSON, XLS, Delta, MySQL, Postgres, Sheets, Airtable | Parquet/CSV external tables; internal Delta storage | SQLite (plus loaders/plugins) |
| Output formats | JSON, Arrow IPC | JSON, Arrow, MessagePack, Parquet | JSON (CDN/HTTP-cache friendly) | JSON, CSV, web UI |
| Write path | No (read-only; reload-from-disk only) | No | **Yes** (uploads, DDL/DML) | Read-focused (writes via plugins) |
| Cross-dataset joins | No (one dataset per query) | **Yes** (cross-source) | Yes (SQL) | Yes (SQL, within a DB) |
| Memory model | Both engines: resident (eager) or streamed (`lazy`) | Resident (in-memory) | Resident + spill-to-disk | On-disk (SQLite) |
| Python embedding | **Yes — launch server from wheel** | Python bindings | No | Native Python app |
| Ops built in | **Probes, OIDC, metrics, hot reload** | Partial | Partial | Plugin ecosystem |
| Maturity | Early (v0.x, small community) | Established | Established, commercially backed | **Very mature, large ecosystem** |

## What makes DataPress distinct

- **Two interchangeable backends behind one API.**
  Clients keep the same contract while you choose DuckDB or Arrow+DataFusion at runtime.
- **Safe-by-default query surface.**
  Structured JSON predicates are first-class; raw SQL is opt-in and hidden when disabled.
- **Speak PostgreSQL when you need to.**
  The DataFusion backend can expose an opt-in PostgreSQL wire-protocol
  endpoint, so `psql`, BI tools, and Postgres drivers can query your
  datasets directly — no HTTP client required. See
  [PostgreSQL (pgwire)](../clients/postgresql.md).
- **Flexible memory strategy.**
  Both engines materialise resident data by default and can stream
  large datasets with `lazy = true`; DataFusion adds optional equality
  indexes for hot point lookups.
- **Embeddable server from Python.**
  `datap-rs` can configure and launch the server in-process.
- **Operations included.**
  Health/readiness/version probes, graceful shutdown, Prometheus metrics,
  OIDC/OAuth2, and atomic reload semantics are built in.

## When to choose [ROAPI](https://roapi.github.io/docs/)

Choose ROAPI if your primary need is broad protocol support and federation:

- SQL/GraphQL/REST/FlightSQL are all first-class.
- Cross-source joins are a core workflow.
- You need many source connectors under one API façade.

Choose DataPress instead when you specifically want a safer structured query
surface, DuckDB as a runtime option, or the in-process Python server workflow.

## When to choose [Seafowl](https://seafowl.io/)

Choose Seafowl if you need write capabilities and database-style workflows:

- Uploads and DDL/DML are required.
- Browser/CDN cache semantics are central to your workload.
- You want DataFusion spill-to-disk behavior under memory pressure.

Choose DataPress when you want a strictly read-only API publication layer and a
clean operational envelope around that scope.

## When to choose [Datasette](https://datasette.io/)

Choose Datasette when your product is human-facing exploration and publishing:

- Rich web UI and plugin ecosystem matter most.
- SQLite is an appropriate storage model.
- Community maturity and ecosystem depth are key selection criteria.

Choose DataPress when your priority is columnar-native API serving for systems
consumers and Arrow/JSON delivery over HTTP.

## When DataPress is the right call

DataPress is usually the right fit when most of these are true:

- Data already lives in Parquet or Delta (local or object storage).
- Consumers need both paged JSON and Arrow IPC over HTTP.
- You want SQL to be optional, not the default external contract.
- You want to switch engines without changing clients.
- You want strong ops defaults in the same binary.

## Maturity note

DataPress is intentionally transparent about maturity: it is a younger project
than ROAPI, Seafowl, and Datasette. Choose it when its model matches your
problem and you are comfortable with an early-stage dependency profile.
