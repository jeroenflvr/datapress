# Arrow + DataFusion

**Crate:** `crates/datafusion` &nbsp;·&nbsp; **Binary:** `datapress-datafusion`

DataPress holds the dataset as Arrow `RecordBatch`es in process memory
and queries it with [Apache DataFusion](https://datafusion.apache.org/).

## Highlights

- **In-memory columnar.** Data is loaded once into Arrow chunks; every
  request operates on resident memory — no parquet read on the hot path.
- **Equality index.** A per-column `value → [row ids]` map (see
  [Configuration › Indexing](../configuration/indexing.md)) backs
  `O(1)` resolution of `eq` / `in` predicates. Combined predicates on
  multiple indexed columns merge sorted row-id lists without touching
  DataFusion.
- **Arrow IPC.** Paged `/query?format=arrow` responses and full
  `/query/stream` exports write Arrow batches into the HTTP response
  stream. Resident no-filter streams can reuse existing batches directly;
  SQL fallback paths may still collect DataFusion execution batches
  before DataPress encodes them.
- **Lazy parquet mode.** `lazy = true` registers a `ListingTable`
  pointing at parquet files; DataFusion handles projection &
  predicate pushdown for datasets too big to materialise.
- **Hot reload.** `POST /api/v1/datasets/{name}/reload` swaps the
  resident chunks atomically using an `ArcSwap` double buffer; queries
  in flight see the old data, queries arriving after the swap see the
  new. See [Operations › Dataset reload](../operations/reload.md).

## Trade-offs

- **Startup cost.** Materialising terabytes of parquet at boot is
  expensive in both time and RAM. Use `lazy = true` or `mode = "none"`
  for those cases.
- **RAM-bound.** Non-lazy datasets must fit in process memory
  (including index maps).
- **Wide-schema indexing.** Auto-indexing 200+ columns concurrently
  can blow up memory — switch to `mode = "list"`.

## When to pick DataFusion

- You need **sub-millisecond `eq` / `in`** point lookups on indexed
  columns (dashboards, search-as-you-type).
- The dataset **fits in RAM** (or you're happy to run lazy mode).
- You want **zero-copy Arrow** all the way out to the client.
- You need **atomic hot reload** of a dataset without dropping
  in-flight queries.

## When to skip DataFusion

- The dataset is too large for RAM and you don't want the lazy-mode
  trade-offs.
- You need DuckDB-specific SQL features.
- Startup time / memory footprint of the index matter more than
  point-lookup latency.
