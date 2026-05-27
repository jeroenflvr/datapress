# Side-by-side comparison

| Aspect                         | DuckDB                             | Arrow + DataFusion                                 |
|--------------------------------|------------------------------------|----------------------------------------------------|
| Binary                         | `datapress-duckdb`                 | `datapress-datafusion`                             |
| Storage                        | Reads parquet on demand            | Materialises into `RecordBatch`es in RAM           |
| Equality index (`[dataset.index]`) | Ignored                        | `auto` / `none` / `list`                           |
| `lazy = true`                  | Allowed, redundant                 | Registers a `ListingTable`                         |
| Startup time                   | Sub-second on huge datasets        | Proportional to dataset size + index width         |
| RAM footprint                  | Just DuckDB                        | Resident Arrow + index maps                        |
| Point-lookup latency           | DuckDB SQL (parallel hash)         | `O(1)` via equality index on `eq` / `in`           |
| Range / `LIKE` / `is_null`     | SQL path                           | SQL path (DataFusion)                              |
| Sort / `group_by` / `distinct` | SQL path                           | SQL path                                           |
| Arrow IPC responses            | ✅ native `query_arrow`            | ✅ from resident chunks, zero-copy                 |
| Hot reload (`/reload`)         | ✅                                  | ✅ atomic `ArcSwap` swap                            |
| Delta support                  | ✅ via `delta` extension            | ✅ via `deltalake` crate                            |
| S3 support                     | ✅ via `httpfs`                     | ✅ via `object_store`                               |
| Best for                       | Huge or growing datasets, full SQL | Hot, RAM-resident data with indexed point queries  |

The HTTP request and response shapes are identical, so you can A/B
both backends against the same client.
