# Indexing

The DataFusion backend builds an in-memory `value → [row ids]` map at
startup so that `eq` and `in` predicates resolve in `O(1)`.

!!! info "DataFusion only"
    DuckDB ignores the `[dataset.index]` block entirely — its own
    optimiser handles equality filters via zone maps and parallel
    hash/vector ops.

## Reference

| Field             | Default   | Meaning                                                            |
|-------------------|-----------|--------------------------------------------------------------------|
| `mode`            | `auto`    | `auto`, `none`, or `list`.                                         |
| `columns`         | `[]`      | Explicit column list. Required for `mode = "list"`.                |
| `max_cardinality` | `100000`  | Auto mode: stop indexing a column once distinct values exceed this. |

Index-eligible Arrow types: `Utf8` (including dictionary-encoded),
`Boolean`, signed integers (`Int8`/`Int16`/`Int32`/`Int64`). Floats,
temporals and binary columns always go through SQL.

## `mode = "auto"` (default)

Indexes every eligible column whose distinct-value count stays below
`max_cardinality`. Each column is built in parallel and abandoned if
the cap is exceeded.

```toml
[dataset.index]
mode            = "auto"
max_cardinality = 50_000     # tighten the cap if RAM is tight
```

!!! warning "Wide schemas (≳ 50 columns)"
    Auto can blow up memory. The index keys are heap-allocated
    `String`s; hundreds of maps building concurrently easily reach
    tens of GB. For wide tables, switch to `mode = "list"` and name
    the columns you actually filter on.

## `mode = "none"`

All predicates go through DataFusion SQL (still vectorised and
multi-threaded). Use this when:

- the dataset is wide and you don't have a fixed query pattern,
- startup time matters more than first-query latency,
- you mostly filter on ranges / `LIKE` (the index doesn't help those).

```toml
[dataset.index]
mode = "none"
```

## `mode = "list"`

Best for **wide tables with a known query pattern.** Only the listed
columns get an index; `max_cardinality` is ignored.

```toml
[[dataset]]
name = "us_accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/*.parquet"

[dataset.index]
mode    = "list"
columns = ["state", "severity", "weather_condition", "city"]
```

An empty `columns` list with `mode = "list"` is caught at startup:

```text
dataset 'foo': index.mode = "list" requires a non-empty index.columns
```

## Lazy datasets

`lazy = true` skips index building entirely — predicate pushdown is
delegated to DataFusion's parquet reader instead.
