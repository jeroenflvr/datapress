---
description: >-
  Curated links to the engines DataPress builds on (Apache DataFusion, Apache
  Arrow, DuckDB), the benchmarks the OLAP world measures itself against, the
  people behind these projects, and head-to-head comparisons with Spark and
  pandas.
---

# Resources & links

DataPress stands on the shoulders of two excellent columnar engines and the
research communities around them. This page collects the external links worth
bookmarking — project homepages, benchmarks, comparisons, and the people whose
work powers it all.

## Engines & projects

The two backends DataPress wraps, plus the broader ecosystem they live in.

- [Apache DataFusion](https://datafusion.apache.org/) — the embeddable Rust
  query engine behind the `datapress-datafusion` backend.
- [Apache Arrow](https://arrow.apache.org/) — the columnar memory format and
  IPC protocol DataPress serves over the wire.
- [DuckDB](https://duckdb.org/) — the in-process analytical database behind the
  `datapress-duckdb` backend. See [Why DuckDB](https://duckdb.org/why_duckdb)
  for its design goals.
- [DuckDB-WASM](https://github.com/duckdb/duckdb-wasm) — the browser build that
  powers the DataPress in-page SQL terminal.
- [sqlparser-rs](https://github.com/sqlparser-rs/sqlparser-rs) — the SQL
  parser DataPress uses to validate the raw-SQL endpoint.
- [Apache Spark](https://spark.apache.org/) — the distributed comparison point
  for large-scale workloads.
- [pandas](https://pandas.pydata.org/) — the single-node DataFrame baseline.
- [Polars](https://pola.rs/) — a fast Rust/Arrow DataFrame library, a common
  point of comparison with DuckDB and DataFusion.
- [ClickHouse](https://clickhouse.com/) — column-oriented OLAP database and
  author of the ClickBench benchmark.

## Benchmarks

How the analytical-database world measures itself.

- [ClickBench](https://benchmark.clickhouse.com/) — the de-facto analytical
  DBMS benchmark; DuckDB, DataFusion, ClickHouse, Spark and dozens more on one
  leaderboard. ([methodology & sources](https://github.com/ClickHouse/ClickBench/))
- [TPC-H](https://www.tpc.org/tpch/) — the classic ad-hoc decision-support
  benchmark (22 queries over a star-ish schema).
- [TPC-DS](https://www.tpc.org/tpcds/) — the larger, more complex
  decision-support successor to TPC-H (99 queries).
- [DataFusion vs DuckDB benchmark](https://github.com/alamb/datafusion-duckdb-benchmark)
  — Andrew Lamb's reproducible head-to-head harness.

## Comparisons

DataPress ships the same HTTP API on top of both engines, so the most direct
comparison lives right here in the docs:

- [DuckDB vs Arrow + DataFusion](../backends/comparison.md) — the DataPress
  side-by-side: startup time, RAM footprint, indexing, point-lookup latency.

For the wider DataFusion / DuckDB / Spark / pandas / Polars landscape, the
ClickBench leaderboard and the benchmark harnesses above are the most current,
reproducible references.

## People

The maintainers and creators whose work DataPress builds on.

- **Andrew Lamb** — Apache DataFusion / Arrow / Parquet PMC; drives much of
  DataFusion's day-to-day.
  [Blog](https://andrew.nerdnetworks.org/) ·
  [GitHub](https://github.com/alamb)
- **Andy Grove** — original author of Apache DataFusion and author of the
  *How Query Engines Work* book.
  [Site](https://andygrove.io/) ·
  [GitHub](https://github.com/andygrove) ·
  [How Query Engines Work](https://howqueryengineswork.com/)
- **Hannes Mühleisen** — co-creator of DuckDB, co-founder & CEO of DuckDB Labs,
  researcher at CWI Amsterdam.
  [Site](https://hannes.muehleisen.org/) ·
  [GitHub](https://github.com/hannes)
- **Mark Raasveldt** — co-creator of DuckDB and co-founder of DuckDB Labs.
  [Site](https://www.markraasveldt.com/) ·
  [GitHub](https://github.com/Mytherin)
