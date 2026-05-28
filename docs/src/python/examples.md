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

## OIDC / OAuth2 with a local Keycloak

End-to-end: spin up the bundled Keycloak stack, start a DataPress server
with `AuthConfig`, then call it with a service-account token.

**1. Start Keycloak** (from the repo root):

```bash
cd examples/keycloak
docker compose up -d
# admin console: http://localhost:8080  (admin / admin)
# issuer:        http://localhost:8080/realms/datapress
```

The compose file pre-provisions:

- realm `datapress`
- confidential client `datapress-api` (secret `datapress-secret`, service
  accounts enabled)
- public client `datapress-swagger` (for Swagger UI SSO)
- scopes `datasets:read` and `datasets:reload`
- test user `alice` / `alice`

**2. Start DataPress with auth enabled** (`pip install datap-rs` —
wheels include the `auth` feature):

```python
# serve_auth.py
import asyncio
from datap_rs.datapress import (
    DataPress, DataPressConfig, DatasetConfig, AuthConfig,
)

async def main() -> None:
    cfg = DataPressConfig(
        backend="datafusion", listen="127.0.0.1", port=8000,
    )
    ds  = DatasetConfig(
        name="accidents",
        source="data/us_accidents/march_2023.parquet",
        format="parquet",
    )
    auth = AuthConfig(
        enabled=True,
        issuer="http://localhost:8080/realms/datapress",
        audience="datapress-api",
        read_scopes=["datasets:read"],
        reload_scopes=["datasets:reload"],
        algorithms=["RS256"],
    )
    await DataPress(cfg, datasets=[ds], auth=auth).run()

if __name__ == "__main__":
    asyncio.run(main())
```

```bash
python serve_auth.py
```

**3. Fetch a token and call the API**:

```python
# call_auth.py
import requests

KC   = "http://localhost:8080/realms/datapress/protocol/openid-connect/token"
BASE = "http://127.0.0.1:8000"

TOKEN = requests.post(
    KC,
    data={
        "grant_type":    "client_credentials",
        "client_id":     "datapress-api",
        "client_secret": "datapress-secret",
        "scope":         "datasets:read datasets:reload",
    },
    timeout=5,
).json()["access_token"]

H = {"Authorization": f"Bearer {TOKEN}"}

print("datasets:", requests.get(f"{BASE}/api/datasets", headers=H).json())
print("count:   ", requests.post(
    f"{BASE}/api/datasets/accidents/count", headers=H, json={},
).json())

# Anonymous → 401
print("anon:    ", requests.get(f"{BASE}/api/datasets").status_code)
```

`DataPressClient` is currently bearer-token-agnostic; reach for
`requests` (or any HTTP client) and set the `Authorization` header
yourself. The built-in `admin_token` kwarg still wires up the legacy
`X-Admin-Token` header for reload endpoints when
`admin_token_fallback=True`.

**4. Resource-owner password flow** (the bundled `alice` user, for
interactive scripts / notebooks):

```python
TOKEN = requests.post(
    "http://localhost:8080/realms/datapress/protocol/openid-connect/token",
    data={
        "grant_type":    "password",
        "client_id":     "datapress-api",
        "client_secret": "datapress-secret",
        "username":      "alice",
        "password":      "alice",
        "scope":         "datasets:read",
    },
    timeout=5,
).json()["access_token"]
```

A scope the client didn't request — e.g. hitting `/reload` with only
`datasets:read` — returns `403 Forbidden`. An expired or unsigned token
returns `401 Unauthorized`. Tear it all down with
`docker compose down -v` from `examples/keycloak/`.
