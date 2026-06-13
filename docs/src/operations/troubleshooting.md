---
description: >-
  Diagnose and fix common DataPress runtime issues — OOM kills during dataset
  load, slow cold-cache queries, reload 403s, empty datasets skipped at startup,
  and the DuckDB legacy CXX ABI build failure.
---

# Troubleshooting

Common runtime issues and how to diagnose them. Each section starts with
the symptom you see, then explains the root cause and the fix.

## Exit code 137 / process killed / "OOMKilled"

### Symptom

One of the following:

- The process disappears with no Rust panic, no stack trace, just an exit
  code of **137** (= 128 + 9 = `SIGKILL`).
- `dmesg` shows `Out of memory: Killed process … (datapress)`.
- In Kubernetes / Docker the container status reads `OOMKilled`.
- Resident memory grows far past what the parquet file size suggests
  (e.g. a 3 GB snappy file pushing the process past 100 GB RSS) before
  the kernel intervenes.

This almost always happens during **dataset load at startup** or during
a `POST /api/datasets/{name}/reload`, not during a query.

### Root cause: the equality-index build on a wide schema

By default every dataset is loaded with `index.mode = "auto"` and
`index.max_cardinality = 100_000`. Auto mode iterates every eligible
column (`Utf8`, `Bool`, `Int8..Int64`) and builds a hash map from each
distinct value to a sorted list of row positions.

For a **narrow** table (a few dozen columns) this is cheap and gives you
O(1) `eq` / `in` lookups. For a **wide** table (hundreds of columns) it
is catastrophic — and that's the case you're hitting.

### What "cardinality" actually means

> **Cardinality of a column = the number of distinct (unique) values
> that column contains.**

A few concrete examples for an `accidents` dataset with ~8 million rows:

| column                | example values                          | typical cardinality | indexed under Auto? |
| --------------------- | --------------------------------------- | ------------------- | ------------------- |
| `severity`            | 1, 2, 3, 4                              | **4**               | ✅ tiny             |
| `state`               | "CA", "TX", "NY", …                     | **~50**             | ✅ tiny             |
| `weather_condition`   | "Fair", "Rain", "Snow", …               | **~150**            | ✅ small            |
| `city`                | "Los Angeles", "Houston", …             | **~15 000**         | ✅ medium           |
| `zipcode`             | "90210", "10001", …                     | **~50 000**         | ✅ borderline       |
| `description`         | full natural-language sentences         | **~5 000 000**      | ❌ near-unique      |
| `id`                  | unique row identifier                   | **~8 000 000**      | ❌ literally unique |
| `start_lat` (Float64) | 34.0522, …                              | (not eligible)      | ❌ skipped          |

Low cardinality (a handful of distinct values) → an index is small and
extremely useful.
High cardinality (near-unique strings) → the index is almost as large
as the column itself, and barely speeds anything up.

`max_cardinality` (default `100_000`) is the cut-off: Auto stops
indexing a column once it sees that many distinct values, and discards
the partial map. **But it only stops when the threshold is crossed —
everything allocated up to that point still has to be allocated.**

### Why memory blows up

For each indexed column, the index stores:

1. **One `String` per distinct value** — the hash key, copied out of
   the Arrow string buffer. ~24 bytes header + the bytes of the value.
2. **One `Vec<u32>` per distinct value** — the row positions where
   that value occurs. 4 bytes per row + Vec overhead.

Now multiply across the dataset. Take a wide table with `C = 320`
indexable columns and `R = 8 000 000` rows. Even in the *best* case
(every column has low cardinality), the row-position vectors alone
weigh:

```text
C × R × 4 bytes = 320 × 8 000 000 × 4 = ~10 GB
```

…just for the `Vec<u32>` payloads. **Add to that:**

- The string keys themselves (variable, can be tens of GB on its own
  for wide string-heavy schemas).
- HashMap overhead — typically a 2–3× multiplier over the raw key+value
  bytes due to open addressing + load factor.
- **Concurrency multiplier:** all `C` maps are built **in parallel via
  rayon**, so they coexist at peak.
- The materialised Arrow `RecordBatch` (decompressed Parquet — usually
  4–8× the compressed file size) sitting next to the index.
- A transient **doubling** during `concat_batches`, where the source
  `Vec<RecordBatch>` and the concatenated single batch are both alive
  for the length of the merge.

For the US Accidents dataset shipped in our tests (~8 M rows × 45 cols),
this all fits comfortably in memory. For a 320-column dataset, it
doesn't — peak can easily exceed 120 GB even though the file on disk
is 3 GB.

### Fix: use `mode = "list"` and name the columns you filter on

This is the right answer for **any table with more than ~50 columns**.
Look at the queries you actually run and only index the columns that
appear on the left-hand side of an `eq` or `in` predicate.

```toml
[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/*.parquet"

[dataset.index]
mode    = "list"
columns = ["state", "severity", "weather_condition"]
```

Everything else is reached through the engine's vectorised SQL
fallback, which is still fast — it just doesn't have an O(1) lookup
shortcut.

### Fix: tighten `max_cardinality`

If you really want Auto mode, push the cap down. For most workloads
**1 000 is plenty** — equality indexes on columns with more distinct
values than that rarely help because each `eq` lookup still scans a
long row-id vector.

```toml
[dataset.index]
mode            = "auto"
max_cardinality = 1_000
```

### Fix: disable indexing entirely

If you mostly filter on ranges (`gt`, `lt`, `between`) or substring
matches (`like`, `ilike`), the equality index never fires anyway. Save
the RAM:

```toml
[dataset.index]
mode = "none"
```

### Fix: switch to lazy mode (last resort)

For datasets that simply cannot fit in RAM in decompressed form,
flip `lazy = true`. The DataFusion backend registers a `ListingTable`
and streams row groups from disk on every query — bounded memory, no
index, higher per-query latency.

```toml
[[dataset]]
name = "huge_telemetry"
lazy = true

[dataset.source]
kind     = "parquet"
location = "data/telemetry/*.parquet"
```

Always pass an explicit `columns = [...]` in queries against lazy
datasets so parquet projection pushdown can skip the columns you don't
need.

### Rule of thumb

| dataset shape                       | recommended `mode`                   |
| ----------------------------------- | ------------------------------------ |
| narrow (≤ 50 cols), fits in RAM     | `auto` (default)                     |
| wide (> 50 cols), fits in RAM       | `list` with 3–10 filter columns      |
| only filter on ranges / `ilike`     | `none`                               |
| does not fit in RAM at all          | `lazy = true` + `mode = "none"`      |

See [Configuration › Indexing](../configuration/indexing.md) for the full
index reference.

## Slow first query (cold-cache)

### Symptom

The first query after startup or after a reload takes 100s of ms; later
queries on the same dataset are sub-ms.

### Root cause

The OS page cache is empty, so the first scan pulls the parquet bytes
off disk. With `lazy = true`, *every* query reads from disk for the
columns it touches; that's the deliberate trade-off of lazy mode.

### Fix

- For eager datasets: warm the cache with one representative query
  after startup.
- For lazy datasets: keep query column lists tight (only the columns
  you actually need in the response).

## `403 Forbidden` on `/api/datasets/{name}/reload`

### Symptom

```text
{"error": "forbidden"}
```

### Root cause

The reload endpoint requires the `X-Admin-Token` request header to
match the `ADMIN_TOKEN` env var the server was started with. If
`ADMIN_TOKEN` is **unset**, the endpoint is **disabled** and every
call returns 403 regardless of the header.

### Fix

```bash
ADMIN_TOKEN=dev-secret task run:datafusion
# then:
curl -X POST -H 'X-Admin-Token: dev-secret' \
  http://localhost:8080/api/datasets/accidents/reload
```

See [Operations › Dataset reload](reload.md) for the full reload
semantics and the OIDC-based alternative under
[Operations › Authentication](auth.md).

## "skipping empty dataset '…'" at startup

### Symptom

A dataset is missing from `/api/v1/datasets` and the log shows a
warning like:

```text
WARN  skipping empty dataset 'events': dataset 'events': no *.parquet files found in data/events/
```

### Root cause

The configured `source.location` resolved to zero files (or zero rows).
Rather than aborting the whole server, DataPress logs a warning and
**skips** that one dataset — the rest of the registry still loads and
serves traffic. Common reasons:

- A glob pattern (`data/*.parquet`) that doesn't match anything on
  this host.
- A directory path with no `.parquet` files in it.
- An S3 prefix (`s3://bucket/events/`) with no objects under it yet.
- A relative path that's correct on your dev box but wrong inside the
  container (the process's CWD differs).

This applies to both the `datafusion` and `duckdb` backends, and to the
Python bindings (`DataPress(...)`) the same way — the dataset is dropped,
not fatal.

!!! note "Delta tables are never \"empty\""
    An empty Delta table still carries a schema in its transaction log,
    so it registers normally as a 0-row dataset. Only parquet sources
    that resolve to *no files* are skipped.

!!! warning "`reload` still errors"
    `POST /api/v1/datasets/{name}/reload` returns an error if the
    reloaded source is empty — an admin reload is an explicit action, so
    it reports failure instead of silently dropping the dataset.

### Fix

- Run `ls` against the same path you put in the config — from the
  same working directory the server starts in.
- For glob patterns: shell-expand them by hand
  (`echo data/*.parquet`) to confirm what they resolve to.
- For S3: list the prefix (`aws s3 ls s3://bucket/events/`) to confirm
  objects exist.
- For containers: prefer absolute paths or mount the data directory
  at a known location.


## DuckDB build fails in CI with `legacy CXX ABI`

### Symptom

A wheel build in `.github/workflows/publish.yml` fails with:

```text
#error "DuckDB does not provide extensions for this (legacy) CXX ABI
 - Explicitly set DUCKDB_PLATFORM ..."
```

### Root cause

The `manylinux` containers used by `PyO3/maturin-action` still default
to the pre-cxx11 GCC C++ ABI (`_GLIBCXX_USE_CXX11_ABI=0`). The bundled
`libduckdb-sys` refuses to compile against that ABI without an explicit
opt-in.

### Fix

Already applied in `.github/workflows/publish.yml`:

```yaml
- uses: PyO3/maturin-action@v1
  env:
    DUCKDB_PLATFORM: ${{ matrix.target == 'x86_64' && 'linux_amd64_gcc4' || 'linux_arm64_gcc4' }}
  with:
    sccache: 'false'    # sccache also breaks the bundled C++ build
    ...
```

If you fork the workflow and hit this again, make sure both the
`env.DUCKDB_PLATFORM` and `sccache: 'false'` lines are present.
