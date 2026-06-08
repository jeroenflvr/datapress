# datap-rs-client

Python client for a [DataPress](https://github.com/jeroenflvr/datapress) dataset
server, backed by the native Rust client (`datapress-client`) via PyO3.

Requests are plain Python dicts; responses come back as dicts. Structured
queries can optionally be decoded into a `pyarrow.Table`.

## Install

```sh
pip install datap-rs-client          # core
pip install datap-rs-client[arrow]   # + pyarrow for query_arrow()
```

> This project standardises on [`uv`](https://docs.astral.sh/uv/):
> `uv pip install datap-rs-client[arrow]`.

## Usage

```python
from datap_rs_client import DataPressClient

client = DataPressClient("http://127.0.0.1:8000")

client.datasets()
# ['accidents']

client.count("accidents", predicates=[{"col": "Severity", "op": "gte", "val": 3}])
# 123456

rows = client.query(
    "accidents",
    columns=["State", "Severity"],
    predicates=[{"col": "Severity", "op": "gte", "val": 3}],
    page_size=1000,
)
rows["page"], len(rows["data"])
# (1, 1000)

# Arrow (requires the [arrow] extra)
table = client.query_arrow("accidents", columns=["State", "Severity"], page_size=100_000)
table.num_rows
```

### Authentication

```python
client = DataPressClient(
    "http://127.0.0.1:8000",
    bearer_token="…",     # servers with auth enabled
    admin_token="…",      # required by reload()
)
```

### SQL

```python
client.sql("SELECT State, COUNT(*) AS n FROM accidents GROUP BY State", max_rows=100)
```

## DataFrames

`query_arrow(...)` returns a `pyarrow.Table` (install the `[arrow]` extra).
Arrow is the zero-copy interchange format for every popular dataframe
library, so a single query feeds them all:

```python
from datap_rs_client import DataPressClient

client = DataPressClient("http://127.0.0.1:8000")
table = client.query_arrow(
    "accidents",
    columns=["State", "Severity"],
    predicates=[{"col": "Severity", "op": "gte", "val": 3}],
    page_size=1_000_000,
)
```

### Polars

```python
import polars as pl

# Zero-copy from the Arrow table.
df = pl.from_arrow(table)
df.group_by("State").len().sort("len", descending=True)
```

### pandas

```python
import pandas as pd  # noqa: F401  (pyarrow drives the conversion)

# Arrow-backed dtypes (recommended) …
df = table.to_pandas(types_mapper=pd.ArrowDtype)
# … or classic NumPy-backed dtypes:
df = table.to_pandas()
df.groupby("State")["Severity"].mean()
```

### DuckDB

```python
import duckdb

# DuckDB queries the Arrow table in place — no copy, no temp files.
duckdb.sql("SELECT State, COUNT(*) AS n FROM table GROUP BY State ORDER BY n DESC")
```

### PySpark

```python
from pyspark.sql import SparkSession

spark = SparkSession.builder.getOrCreate()
# Spark has no direct Arrow-table constructor; go via pandas (Arrow-accelerated).
sdf = spark.createDataFrame(table.to_pandas())
sdf.groupBy("State").count().orderBy("count", ascending=False).show()
```

### DataFusion

```python
from datafusion import SessionContext

ctx = SessionContext()
df = ctx.from_arrow(table)
df.aggregate([df["State"]], [df["Severity"].mean()])
```

### PyArrow / Arrow ecosystem

```python
# The result is already a pyarrow.Table.
table.column("Severity").combine_chunks()
table.to_batches()           # -> list[pyarrow.RecordBatch]
table.to_pydict()            # -> dict[str, list]
```

> Anything implementing the [Arrow C Data Interface](https://arrow.apache.org/docs/format/CDataInterface.html)
> (Polars, DuckDB, DataFusion, Vaex, cuDF, …) can consume the table
> directly. For libraries without an Arrow constructor, `table.to_pandas()`
> is the universal fallback.

## Relationship to `datap-rs`

`datap-rs` ships the **server**; `datap-rs-client` is a standalone **client**.
They are independent packages — install whichever you need.

## License

MIT
