# DataPress vs. other tools

DataPress is not the only way to put an HTTP query layer in front of
columnar files. This page explains, honestly, where it fits relative to the
projects you are most likely to be choosing between — and where one of those
projects is the better call.

If you take one thing away: DataPress is a **publication layer** for data you
already curate as Parquet or Delta. It is deliberately narrow. Several tools
below do more than DataPress does, and some are far more mature. Pick the one
whose shape matches your problem.

## At a glance

| | **DataPress** | **ROAPI** | **Seafowl** | **Datasette** |
|---|---|---|---|---|
| Language / engine | Rust · DuckDB **and** Arrow+DataFusion | Rust · DataFusion | Rust · DataFusion + delta-rs | Python · SQLite |
| Query interface | Structured JSON predicates (raw SQL opt-in) | SQL · GraphQL · REST · FlightSQL | SQL (HTTP) | SQL · JSON REST · web UI |
| Engine choice | **Two, interchangeable, identical API** | One | One | One |
| Native sources | Parquet, Delta (local / S3) | Parquet, CSV, JSON, XLS, Delta, MySQL, Postgres, Google Sheets, Airtable | Parquet/CSV external tables; internal Delta storage | SQLite (plus loaders/plugins) |
| Output formats | JSON, Arrow IPC | JSON, Arrow, MessagePack, Parquet | JSON (CDN/HTTP-cache friendly) | JSON, CSV, web UI |
| Write path | No (read-only; reload-from-disk only) | No | **Yes** (uploads, DDL/DML) | Read-focused (writes via plugins) |
| Cross-dataset joins | No (one dataset per query) | **Yes** (cross-source) | Yes (SQL) | Yes (SQL, within a DB) |
| Memory model | DuckDB: lazy reads · DataFusion: resident | Resident (in-memory) | Resident + spill-to-disk | On-disk (SQLite) |
| Embeddable from Python | **Yes — launch the server from a wheel** | Python bindings | No | It *is* Python |
| Ops (probes, OIDC, metrics, hot reload) | **Built in** | Partial | Partial | Plugin ecosystem |
| Maturity / community | Early (v0.x, small) | Established | Established, commercially backed | **Very mature, large ecosystem** |

Treat the table as a map, not a scoreboard. The prose below is where the real
decision lives.

## What makes DataPress distinct

A few things are genuinely unusual about DataPress rather than just
table-stakes:

- **Two interchangeable backends behind one API.** The same request and
  response shapes run on DuckDB *or* Arrow+DataFusion, selected in config. You
  can A/B the engines on your own workload and switch without touching a single
  client. No other tool here ships two engines you can swap under an identical
  contract.
- **A safe-by-default query surface.** Clients send a structured JSON
  predicate document, not SQL. Raw SQL exists but is opt-in, validated, and
  returns `404` while disabled so its presence isn't even advertised. If your
  threat model dislikes handing arbitrary SQL to the internet, this is a
  meaningfully different default from the SQL-first tools.
- **A real mixed-engine memory story.** The DuckDB backend reads Parquet
  pages lazily, so it is not bound by dataset-fits-in-RAM. The DataFusion
  backend keeps data resident with an optional per-column equality index for
  O(1) point lookups. You choose the trade-off per deployment.
- **Embeddable from Python as a server, not just a client.** `pip install
  datap-rs` gives you a wheel (PyO3 + maturin) that *configures and launches*
  the server in-process — handy for notebooks, jobs, and event-driven reloads
  from a Python consumer.
- **Operations included, not bolted on.** Liveness/readiness/version probes,
  graceful shutdown, Prometheus metrics, OIDC/OAuth2 bearer scopes, and atomic
  hot reload (with documented per-backend swap semantics) ship in the box.

## When to choose something else

### Choose **ROAPI** if you want standard query surfaces or federation

[ROAPI](https://roapi.github.io/docs/) is the closest sibling: a Rust,
DataFusion-based, no-code read-only API server. Reach for it instead of
DataPress when:

- You want clients to speak **SQL, GraphQL, REST, or FlightSQL** rather than a
  bespoke predicate DSL. ROAPI translates all four into DataFusion plans.
- You need to **join across data sources** — ROAPI can serve and join CSV,
  JSON, XLS, Parquet, Delta, MySQL, Postgres, Google Sheets, and Airtable in
  one place. DataPress queries one dataset at a time and reads only Parquet and
  Delta.
- You want extra wire formats out of the box (MessagePack, Parquet) or a more
  established project with a wider user base.

DataPress's counter-arguments are the dual backend, the safe-by-default
predicate API, the DuckDB lazy-read path (ROAPI's DataFusion core wants the
dataset in memory), and the heavier ops/auth story. But if "give me SQL and
joins over many sources" is the requirement, ROAPI is the more direct answer.

### Choose **Seafowl** if you need writes or browser-cached dashboards

[Seafowl](https://seafowl.io/) is an analytical database (DataFusion +
delta-rs) built for data-driven web apps. Prefer it when:

- You need a **write path** — uploads, `CREATE TABLE`, DDL/DML, WASM UDFs.
  DataPress is read-only by design; its only mutation is reloading a dataset
  from disk.
- You are powering **dashboards or notebooks that query straight from the
  browser** and want results cached by a CDN. Seafowl's HTTP API is explicitly
  engineered around ETags and HTTP-cache friendliness; DataPress is not.
- Your queries can exceed RAM and you want DataFusion's **spill-to-disk** under
  a memory budget.

DataPress wins where you specifically want the *DuckDB* engine, an opt-in/no-SQL
surface, or the in-process Python server. For "a small analytical DB I can
write to and cache at the edge," Seafowl is purpose-built for that.

### Choose **Datasette** if you want exploration, publishing, and a UI

[Datasette](https://datasette.io/) is the mature, batteries-included option for
turning data into an explorable, published site. Prefer it when:

- You want a **web UI, faceting, full-text search, and a huge plugin
  ecosystem** rather than a headless API.
- Your data is comfortable in **SQLite**, and you value a large community and
  long track record over columnar/Parquet-native performance.
- You are publishing for humans to browse as much as for systems to consume.

DataPress is columnar-native and built for Arrow-speed bulk pulls and
operational APIs, not interactive exploration. For data journalism, open-data
publishing, and "let people poke at this dataset," Datasette is the stronger
tool.

## When DataPress is the right call

Pick DataPress when most of these are true:

- Your source of truth is already **Parquet or Delta**, local or in S3.
- Consumers speak **HTTP**, and you want both small paged JSON (for app
  surfaces) and **Arrow IPC** (for Polars / pandas / DuckDB / pipelines) from
  one deployment.
- You'd rather expose a **structured, SQL-optional** query contract than open a
  SQL endpoint.
- You want to **choose or A/B the execution engine** without rewriting clients.
- You want **production ops** — probes, graceful shutdown, metrics, OIDC,
  atomic hot reload — without assembling them yourself.
- You like the option to **embed and launch the server from Python**.

## An honest note on maturity

The most important caveat isn't a feature gap — it's age. DataPress is an
early-stage project with a small community. ROAPI, Seafowl, and Datasette all
have more history, more users, and (for Seafowl) commercial backing. If you are
choosing a dependency you need to be confidently maintained and battle-tested
*today*, weigh that honestly. DataPress is a reasonable choice when its shape
fits your problem well and you're comfortable adopting a young dependency; it is
not yet the safe default pick on maturity grounds alone.
