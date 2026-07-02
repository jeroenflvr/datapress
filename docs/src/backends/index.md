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

## How a reload works

`POST /api/v1/datasets/{name}/reload` swaps a dataset for a freshly loaded
copy **without a restart and without dropping in-flight queries**. Each
backend keeps the old copy live until the new one is fully ready, but the
swap mechanism differs:

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant H as Reload handler
    participant DF as DataFusion (ArcSwap)
    participant DK as DuckDB (transaction)

    C->>H: POST /datasets/{name}/reload

    rect rgb(235, 244, 255)
    note over DF: DataFusion — ArcSwap double buffer
    H->>DF: load new Arrow chunks + index (off to the side)
    note over DF: existing queries keep reading the old Arc
    DF->>DF: atomic ArcSwap(new)
    note over DF: queries after the swap see the new Arc;<br/>old Arc dropped when its last reader finishes
    end

    rect rgb(235, 255, 240)
    note over DK: DuckDB — ACID transaction
    H->>DK: CREATE OR REPLACE TABLE name AS SELECT ...
    note over DK: runs inside a transaction — readers see the old table
    DK->>DK: COMMIT (or ROLLBACK on failure)
    note over DK: a failed reload leaves the existing table live
    end

    H-->>C: 200 OK
```

Because the swap is atomic on both engines, a reader never sees a
half-loaded dataset, and a failed load never takes the dataset offline. See
[Operations › Dataset reload](../operations/reload.md) for the endpoint,
auth requirements, and backend-specific caveats.
