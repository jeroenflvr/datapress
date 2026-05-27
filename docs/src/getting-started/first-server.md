# First server

## 1. A parquet file

Drop a parquet file somewhere reachable by the process. For the
examples in these docs we use the
[Kaggle US accidents 2016–2023](https://www.kaggle.com/datasets/sobhanmoosavi/us-accidents)
dataset:

```bash
ls data/us_accidents/march_2023.parquet
```

A directory of `*.parquet`, a glob, or an `s3://...` URL all work too —
see [Configuration › Datasets](../configuration/datasets.md).

## 2. `datasets.toml`

A minimal config:

```toml
[server]
listen = "127.0.0.1"
port   = 8080

[[dataset]]
name = "accidents"

[dataset.source]
kind     = "parquet"
location = "data/us_accidents/march_2023.parquet"
```

Save it as `datasets.toml` in the working directory. Override the path
with the `DATASETS_CONFIG` env var if you keep it elsewhere.

## 3. Run a backend

=== "DuckDB"

    ```bash
    task run:duckdb
    # or, without taskfile:
    RUST_LOG=info ./target/release/datapress-duckdb
    ```

=== "Arrow + DataFusion"

    ```bash
    task run:datafusion
    # or:
    RUST_LOG=info ./target/release/datapress-datafusion
    ```

=== "Python"

    ```python
    import asyncio
    from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

    async def main() -> None:
        ds = DatasetConfig(
            name="accidents",
            source="data/us_accidents/march_2023.parquet",
            format="parquet",
        )
        cfg = DataPressConfig(backend="duckdb", port=8000)
        await DataPress(cfg, datasets=[ds]).run()

    asyncio.run(main())
    ```

Startup logs print the bind address, worker count, route table, and a
summary line including the active backend and shutdown grace period.

## 4. Talk to it

```bash
curl http://localhost:8080/api/v1/datasets
curl http://localhost:8080/healthz
curl http://localhost:8080/readyz
curl http://localhost:8080/version
```

See the [quick tour](quick-tour.md) for a tour of every route.
