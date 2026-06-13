# Datasets

Each `[[dataset]]` block declares one table that DataPress will expose.

## Common fields

| Field    | Required | Default        | Notes                                                                                            |
|----------|----------|----------------|--------------------------------------------------------------------------------------------------|
| `name`   | yes      | —              | URL slug + SQL table name. Must be unique.                                                       |
| `source` | yes      | —              | Sub-table: `{ kind = "parquet" \| "delta", location = "..." }`.                                  |
| `s3`     | no       | absent         | Only meaningful when `location` starts with `s3://`. See [S3 / object storage](s3.md).           |
| `index`  | no       | `mode="auto"`  | Equality-index policy (DataFusion only). See [Indexing](indexing.md).                            |
| `lazy`   | no       | `false`        | Skip materialisation; stream row groups at query time. DataFusion + DuckDB, parquet + delta.     |

## `source` reference

`[dataset.source]` is a tagged enum.

| `kind`    | `location`                                          | Notes                                                                                  |
|-----------|-----------------------------------------------------|----------------------------------------------------------------------------------------|
| `parquet` | a `.parquet` file                                   | Read as-is.                                                                            |
| `parquet` | a directory                                         | Every `*.parquet` inside (sorted, non-recursive). No glob patterns.                    |
| `parquet` | a glob (`data/*/2024-*.parquet`)                    | Supported wildcards: `*`, `?`, `[abc]`.                                                |
| `parquet` | `s3://bucket/key.parquet` or `s3://bucket/prefix/`  | Requires `[dataset.s3]`. DuckDB autoloads `httpfs`.                                    |
| `delta`   | a local directory                                   | Pointed at the table root (the dir containing `_delta_log/`).                          |
| `delta`   | `s3://bucket/path/to/table`                         | Requires `[dataset.s3]`. DuckDB autoloads `delta`; DataFusion uses the `deltalake` crate. |

!!! note "Delta on S3 always materialises"
    When `kind = "delta"` and `location` is `s3://...`, both backends
    fully materialise the table at startup. There is no incremental
    scan path — switch to `parquet` if you need on-demand page reads.

## Single parquet file

```toml
[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/march_2023.parquet"
```

## Directory of parquet files

`location` can be a directory; every `*.parquet` underneath is loaded
in sorted order (non-recursive).

```toml
[[dataset]]
name = "events"

[dataset.source]
kind     = "parquet"
location = "data/events/"
```

## Glob pattern

```toml
[[dataset]]
name = "sales_2024"

[dataset.source]
kind     = "parquet"
location = "data/sales/2024/*/*.parquet"
```

## Lazy mode for huge datasets

When the decompressed Arrow size won't fit in RAM (or the index is too
expensive to build), set `lazy = true`. The DataFusion backend
registers a `ListingTable` and streams row groups at query time;
column-projection pushdown and parquet row-group skipping happen
automatically.

**Trade-off:** higher per-query latency, no equality index. Always
pass explicit `columns=[...]` in your queries to maximise projection
pushdown.

```toml
[[dataset]]
name = "us_accidents"
lazy = true

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/*.parquet"
```

Lazy mode requirements:

- `backend = "datafusion"`
- `kind = "parquet"` (lazy delta is rejected at startup)

## Delta — local

```toml
[[dataset]]
name = "orders"

[dataset.source]
kind     = "delta"
location = "data/orders_delta/"
```

For S3-backed parquet and delta tables, see
[S3 / object storage](s3.md).

## Empty datasets are skipped, not fatal

If a dataset's `source.location` resolves to **no files** at startup —
an empty directory, a glob that matches nothing, an S3 prefix with no
objects yet, or a Delta table with no data files (no log segment, or a
committed schema with zero rows) — DataPress logs a warning and **skips
just that dataset**. The rest of the registry still loads and serves
traffic:

```text
WARN  skipping empty dataset 'events': dataset 'events': no *.parquet files found in data/events/
```

This applies to both backends (`datafusion` and `duckdb`), to parquet
sources whether local or `s3://`, to Delta tables, and to the Python
bindings the same way. The skipped dataset simply won't appear in
`/api/v1/datasets`.

!!! note "Empty Delta tables are skipped"
    A Delta table that resolves to **zero data files** is skipped, whether
    it has a committed transaction log + schema but no rows, or no log
    segment at all (an uninitialized or not-yet-a-Delta-table path). It
    won't appear in `/api/v1/datasets` or the explorer until it has rows.

!!! note "Inaccessible S3 sources are skipped too"
    An `s3://` source that returns **403 Access Denied** at startup (bad
    credentials, missing bucket/prefix policy, or an expired token) is
    logged and skipped the same way, instead of aborting the whole
    server. Fix the credentials or policy and `reload` (or restart) to
    pick it up.

    ```text
    WARN  skipping dataset 'events': S3 access denied — check credentials and bucket policy (...)
    ```

!!! warning "`reload` still errors on empty"
    `POST /api/v1/datasets/{name}/reload` returns an error if the
    reloaded source is empty. A reload is an explicit admin action, so it
    reports failure rather than silently dropping the live dataset.

See [Troubleshooting › empty datasets](../operations/troubleshooting.md#skipping-empty-dataset-at-startup)
for diagnosis tips.

