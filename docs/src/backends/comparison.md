# Side-by-side comparison

| Aspect                         | DuckDB                             | Arrow + DataFusion                                 |
|--------------------------------|------------------------------------|----------------------------------------------------|
| Binary                         | `datapress-duckdb`                 | `datapress-datafusion`                             |
| Storage                        | In-memory table (eager) or streaming view (`lazy`) | `RecordBatch`es in RAM (eager) or `ListingTable` (`lazy`) |
| Equality index (`[dataset.index]`) | Ignored                        | `auto` / `none` / `list`                           |
| `lazy = true`                  | Registers a streaming view         | Registers a `ListingTable`                         |
| Startup time                   | Eager: proportional to dataset · lazy: sub-second | Eager: dataset size + index width · lazy: sub-second |
| RAM footprint                  | Just DuckDB                        | Resident Arrow + index maps                        |
| Point-lookup latency           | DuckDB SQL (parallel hash)         | `O(1)` via equality index on `eq` / `in`           |
| Range / `LIKE` / `is_null`     | SQL path                           | SQL path (DataFusion)                              |
| Sort / `group_by` / `distinct` | SQL path                           | SQL path                                           |
| Arrow IPC responses            | ✅ native `query_arrow`            | ✅ from resident chunks, zero-copy                 |
| Hot reload (`/reload`)         | ✅ DuckDB transactional replace     | ✅ `ArcSwap` double-buffer swap                     |
| Delta support                  | ✅ via `delta` extension            | ✅ via `deltalake` crate                            |
| S3 support                     | ✅ via `httpfs`                     | ✅ via `object_store`                               |
| Best for                       | Huge or growing datasets, full SQL | Hot, RAM-resident data with indexed point queries  |

The HTTP request and response shapes are identical, so you can A/B
both backends against the same client.
