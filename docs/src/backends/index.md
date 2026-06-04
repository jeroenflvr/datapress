---
description: >-
  DataPress ships two interchangeable engines behind one HTTP API — DuckDB and
  Apache Arrow + DataFusion — so you can compare them under the same workload.
---

# Backends

DataPress ships **two complete implementations** of the same HTTP API:

- [DuckDB](duckdb.md) — `crates/duckdb`, binary `datapress-duckdb`
- [Arrow + DataFusion](datafusion.md) — `crates/datafusion`, binary
  `datapress-datafusion`

Both speak the same request/response shapes, so you can A/B them under
real workloads without touching client code.

The Python wheel bundles both — pick at runtime via
`DataPressConfig(backend="duckdb"|"datafusion")`.

See [Comparison](comparison.md) for a side-by-side feature matrix.
