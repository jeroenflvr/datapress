# fast-api

A small Rust web service that exposes one or more **Parquet datasets** over a
JSON HTTP API. The same surface area is implemented twice — once on top of
**DuckDB**, once on top of **Apache Arrow + DataFusion** — so you can A/B the
engines under identical workloads.

- Built on [actix-web](https://actix.rs/) 4
- Datasets declared in a single [`datasets.toml`](datasets.toml)
- Dynamic schema inference at startup (no hard-coded columns)
- Identical request/response shapes across both backends

---

## Quick start

```bash
# 1. Put a parquet file somewhere (or point the config at an existing one).
ls data/accidents.parquet

# 2. Edit datasets.toml — see the example shipped in this repo.

# 3. Run a backend.
task run:duckdb        # or: task run:datafusion

# 4. Talk to it.
curl http://localhost:8080/api/datasets
```

`Taskfile.yml` wraps the typical `cargo build --release --features …`
invocations; see [`task --list`](Taskfile.yml) for the full menu.

---

## The two backends

| Aspect              | `fast-api-duckdb`                              | `fast-api-datafusion`                                |
|---------------------|------------------------------------------------|------------------------------------------------------|
| Engine              | DuckDB (embedded C++)                          | Arrow compute + DataFusion (pure Rust)               |
| Storage             | DuckDB in-memory table per dataset             | One contiguous `RecordBatch` per dataset             |
| Concurrency model   | Connection pool, blocking → `web::block`       | Async-native, multi-threaded `MemTable` partitions   |
| Predicate execution | DuckDB optimiser + parallel hash/vector ops    | Equality index → SIMD scan → DataFusion SQL          |
| Indexes             | Native DuckDB internals (zone maps, etc.)      | Per-dataset eq-index built at startup (configurable) |
| Memory profile      | DuckDB's own buffer manager                    | Whole dataset resident in RAM                        |
| Binary size         | Bundled DuckDB ≈ tens of MB                    | Lean — pure Rust                                     |
| Startup time        | Fast (just `read_parquet`)                     | Slower — reads all rows + builds eq-index            |
| Best at             | Heterogeneous SQL, joins, aggregations         | Dense filter scans, low-latency point lookups        |

### When to pick which

- **DuckDB** is the right default. It handles arbitrary SQL well, has a
  battle-tested optimiser, manages memory itself, and starts up in
  milliseconds because it lazily reads parquet pages on demand.
- **DataFusion** shines when:
  - the dataset fits comfortably in RAM,
  - you query the same columns repeatedly with equality/`IN` predicates
    (the in-process equality index turns those into O(1) lookups), and
  - you want a single static binary without a vendored C++ runtime.

The HTTP API is identical, so the practical comparison is "throughput and
p99 on your queries" — see [`TEST_Q.md`](TEST_Q.md) for a benchmark suite.

---

## Configuration: `datasets.toml`

Every instance reads this file at startup. One `[[dataset]]` entry per
table you want to expose.

```toml
[[dataset]]
name   = "accidents"                  # used in the URL: /api/datasets/accidents/...
source = "data/accidents.parquet"     # file OR directory of *.parquet

# Optional — DataFusion only. DuckDB ignores this block.
[dataset.index]
mode             = "auto"             # "auto" | "none" | "list"
columns          = []                 # required when mode = "list"
max_cardinality  = 100000             # used by "auto" to skip wide cols
```

### Source

- A single `.parquet` file, or
- A directory containing one or more `*.parquet` files (read in sorted order;
  no glob patterns).

### Equality-index policy (DataFusion only)

The DataFusion backend builds an in-memory `value -> [row ids]` map at
startup so that `eq` / `in` predicates resolve in O(1).

| `mode`   | Behaviour                                                              |
|----------|------------------------------------------------------------------------|
| `auto`   | Index every column whose distinct count stays below `max_cardinality`. |
| `none`   | Skip the index entirely — every query goes through DataFusion SQL.     |
| `list`   | Index only the named `columns`. Useful for huge datasets.              |

Override the config path with `DATASETS_CONFIG=/path/to/file.toml`.

---

## HTTP API

Three routes, both backends:

### `GET /api/datasets`

```json
{ "datasets": [ { "name": "accidents", "columns": 47 } ] }
```

### `GET /api/datasets/{name}/schema`

Returns the inferred columns plus a sample row so a client can see what
values look like without issuing a query.

```json
{
  "name": "accidents",
  "columns": [
    { "name": "ID",       "logical": "utf8", "sql_type": "VARCHAR",   "nullable": false },
    { "name": "Severity", "logical": "int",  "sql_type": "INTEGER",   "nullable": true  },
    { "name": "Start_Time", "logical": "temporal", "sql_type": "TIMESTAMP", "nullable": true }
  ],
  "sample": { "ID": "A-1", "Severity": 2, "Start_Time": "2016-02-08 05:46:00", ... }
}
```

`logical` values: `bool | int | float | utf8 | temporal | other`. Temporal
columns are returned as strings.

### `POST /api/datasets/{name}/query`

```json
{
  "columns":   ["ID", "City", "State", "Severity"],
  "predicates": [
    { "col": "State",    "op": "eq",  "val": "TX" },
    { "col": "Severity", "op": "gte", "val": 3   }
  ],
  "page":      1,
  "page_size": 50
}
```

Response:

```json
{ "data": [ { ... }, ... ], "page": 1, "page_size": 50 }
```

#### Request fields

| Field        | Type                | Default | Notes                                  |
|--------------|---------------------|---------|----------------------------------------|
| `columns`    | `string[]`          | `[]`    | Empty = all columns.                   |
| `predicates` | `Predicate[]`       | `[]`    | ANDed together.                        |
| `page`       | `int >= 1`          | `1`     | 1-based.                               |
| `page_size`  | `int 1..=1000`      | `100`   | Clamped to the inclusive range.        |

#### Predicate shape

```json
{ "col": "<column>", "op": "<operator>", "val": <json value | array | omitted> }
```

| `op`           | `val`                  | Meaning                              |
|----------------|------------------------|--------------------------------------|
| `eq`           | scalar                 | `col = val`                          |
| `neq`          | scalar                 | `col <> val`                         |
| `gt` / `gte`   | number / string        | `col > val` / `col >= val`           |
| `lt` / `lte`   | number / string        | `col < val` / `col <= val`           |
| `like`         | string with `%` / `_`  | SQL `LIKE`                           |
| `ilike`        | string with `%` / `_`  | Case-insensitive `LIKE`              |
| `in`           | non-empty array        | `col IN (v1, v2, …)`                 |
| `is_null`      | omit                   | `col IS NULL`                        |
| `is_not_null`  | omit                   | `col IS NOT NULL`                    |

Column names are looked up case-insensitively against the inferred schema
and quoted automatically, so `Temperature(F)` and similar identifiers work.

---

## Examples

```bash
# Discovery
curl -s http://localhost:8080/api/datasets | jq
curl -s http://localhost:8080/api/datasets/accidents/schema | jq

# Equality + range
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID","Severity","City","State","Start_Time"],
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3 }
    ],
    "page": 1, "page_size": 5
  }' | jq

# Substring + numeric range
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "Description",    "op": "ilike", "val": "%fog%" },
      { "col": "Temperature(F)", "op": "lt",    "val": 32 }
    ],
    "page_size": 10
  }' | jq

# IN list
curl -s -X POST http://localhost:8080/api/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "State", "op": "in", "val": ["NY","NJ","CT"] }
    ]
  }' | jq
```

For a deeper benchmark catalogue (light load + CPU/memory stress tests), see
[`TEST_Q.md`](TEST_Q.md).

---

## Project layout

```
src/
├── bin/
│   ├── duckdb.rs              # entrypoint for fast-api-duckdb
│   └── datafusion.rs          # entrypoint for fast-api-datafusion
├── config.rs                  # datasets.toml parsing + validation
├── schema.rs                  # backend-agnostic schema model
├── models.rs                  # Predicate / QueryRequest
├── errors.rs                  # AppError + actix ResponseError
├── duckdb_backend/
│   ├── db.rs                  # Registry: pool + dataset schemas
│   ├── repository.rs          # DatasetRepository (SQL builder)
│   └── handlers.rs            # actix routes
└── datafusion_backend/
    ├── store.rs               # Store: RecordBatch + eq-index per dataset
    └── handlers.rs            # actix routes
```

The shared crate (`config`, `schema`, `models`, `errors`) compiles without
either feature; each backend lives behind its own Cargo feature so building
one never pulls in the other.

---

## Build flags

```bash
# DuckDB only
cargo build --release --bin fast-api-duckdb     --features duckdb

# DataFusion only
cargo build --release --bin fast-api-datafusion --features datafusion

# Both
task build
```

Release builds use LTO + `codegen-units = 1` (see `[profile.release]` in
`Cargo.toml`). Expect noticeably longer link times in exchange for tighter
inner loops.

---

## Environment variables

| Variable          | Default          | Purpose                                  |
|-------------------|------------------|------------------------------------------|
| `DATASETS_CONFIG` | `datasets.toml`  | Path to the dataset registry file.       |
| `DB_POOL_SIZE`    | `num_cpus`       | DuckDB connection pool size (DuckDB only).|
| `RUST_LOG`        | `info`           | Standard `env_logger` filter.            |

Both binaries bind to `0.0.0.0:8080`.

---

## Status / non-goals

- No authentication or rate-limiting — put this behind your own gateway.
- No write path: parquet sources are read-only and loaded at startup.
- No `ORDER BY` / cursor pagination — pagination is plain `OFFSET / LIMIT`,
  so deep pages get expensive (see `H5` in `TEST_Q.md`).
- DataFusion backend keeps the whole dataset in memory. DuckDB does not.
