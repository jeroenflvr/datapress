# Running a server

```python
import asyncio
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

async def main() -> None:
    ds = DatasetConfig(
        name="accidents",
        source="data/accidents.parquet",
        format="parquet",
        mode="auto",
        description="US accidents 2016-2023",
    )
    cfg = DataPressConfig(
        backend="datafusion",
        listen="0.0.0.0",
        port=8000,
        workers=8,
    )
    server = DataPress(cfg, datasets=[ds])
    await server.run()              # blocks until SIGINT / SIGTERM

if __name__ == "__main__":
    asyncio.run(main())
```

`DataPress` is constructed from a `DataPressConfig` and a list of
`DatasetConfig`. `await server.run()` boots the actix runtime, mounts
every route, and blocks the calling coroutine until a shutdown signal
arrives.

## Lifecycle

1. Workers spin up — number controlled by `workers` (defaults to one
   per CPU).
2. Each dataset is loaded by the chosen backend (lazy datasets only
   register their schema).
3. The bind address and full route table are logged.
4. The coroutine blocks on the actix server future.
5. On `SIGTERM` / `SIGINT` (e.g. Ctrl+C) the listening socket closes
   immediately; in-flight requests get up to
   `shutdown_timeout_secs` seconds to drain.
6. `await server.run()` returns. Any cleanup after it (logging,
   metrics flush, …) runs normally.

## Behind a reverse proxy

```python
DataPressConfig(backend="datafusion", port=8000, prefix="/datapress")
# → GET /datapress/api/v1/datasets, GET /datapress/health, ...
```

`prefix` must start with `/` and not end with `/`. The unprefixed
probes — `/healthz`, `/readyz`, `/version` — stay at the bare host
root.

## In a Jupyter notebook

```python
import asyncio, nest_asyncio
nest_asyncio.apply()                  # allow re-entrant event loops

server_task = asyncio.create_task(DataPress(cfg, [ds]).run())
# ... use DataPressClient against http://127.0.0.1:8000 ...
server_task.cancel()
```

See [Examples](examples.md).
