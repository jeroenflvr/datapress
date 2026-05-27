# Examples

## End-to-end: run a server and query it

```python
# example.py
import asyncio
from datap_rs import DataPressClient
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

CFG = DataPressConfig(backend="datafusion", listen="127.0.0.1", port=8000)
DS  = DatasetConfig(
    name="accidents",
    source="data/us_accidents/march_2023.parquet",
    format="parquet",
)

async def serve():
    await DataPress(CFG, datasets=[DS]).run()

async def main():
    server = asyncio.create_task(serve())
    await asyncio.sleep(2)              # give the server a beat

    c = DataPressClient("http://127.0.0.1:8000")
    print("datasets:", c.datasets())
    print("count:   ", c.count("accidents"))

    table = c.query("accidents", {
        "columns":   ["State", "Severity"],
        "predicates": [{"col": "State", "op": "eq", "val": "TX"}],
        "page_size": 5_000,
    })
    print("got", table.num_rows, "rows; columns:", table.column_names)

    server.cancel()

asyncio.run(main())
```

## S3-backed dataset

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig, S3Config

s3 = S3Config(
    region="us-east-1",
    endpoint="http://minio.local:9000",
    addressing_style="path",
    allow_http=True,
)

ds = DatasetConfig(
    name="events",
    source="s3://events/2025/",
    format="parquet",
    s3=s3,
)

cfg = DataPressConfig(backend="datafusion", port=8000)
```

## Jupyter notebook

```python
import asyncio, nest_asyncio
nest_asyncio.apply()

from datap_rs import DataPressClient
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

cfg = DataPressConfig(backend="datafusion", port=8000)
ds  = DatasetConfig(name="accidents", source="data/accidents.parquet",
                    format="parquet")

task = asyncio.create_task(DataPress(cfg, [ds]).run())
client = DataPressClient("http://127.0.0.1:8000")

# ... explore in cells ...
df = pl.from_arrow(client.query("accidents", {"page_size": 50_000}))

task.cancel()                      # when you're done
```

## Multiple datasets

```python
datasets = [
    DatasetConfig(name="states",  source="data/ref/states.parquet"),
    DatasetConfig(
        name="accidents",
        source="data/accidents/2024/*.parquet",
        mode="list",
        index_columns=["state", "severity"],
    ),
    DatasetConfig(
        name="raw_telemetry",
        source="data/telemetry/*.parquet",
        format="parquet",
        lazy=True,
    ),
]
await DataPress(DataPressConfig(backend="datafusion"), datasets=datasets).run()
```
