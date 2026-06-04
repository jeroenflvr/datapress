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

## Raw SQL over one dataset

Enable the SQL endpoint with `sql_enabled=True`, then run a `SELECT` with
`DataPressClient.sql()`. It returns a list of row dicts.

```python
import asyncio
from datap_rs import DataPressClient
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

CFG = DataPressConfig(
    backend="datafusion",
    listen="127.0.0.1",
    port=8000,
    sql_enabled=True,        # exposes POST /api/v1/sql
    sql_max_rows=50_000,     # server-side hard cap
)
DS = DatasetConfig(
    name="accidents",
    source="data/us_accidents/march_2023.parquet",
    format="parquet",
)

async def main():
    server = asyncio.create_task(DataPress(CFG, [DS]).run())
    await asyncio.sleep(2)

    c = DataPressClient("http://127.0.0.1:8000")
    rows = c.sql(
        "SELECT State, COUNT(*) AS n "
        "FROM accidents GROUP BY State ORDER BY n DESC",
        max_rows=10,
    )
    for r in rows:
        print(r["State"], r["n"])

    server.cancel()

asyncio.run(main())
```

A rejected statement (DML, multiple statements, an unknown table, more
than one dataset, or a file-reading function) raises
`DataPressHTTPError` with `status == 400`; when the endpoint is disabled
the status is `404`:

```python
from datap_rs import DataPressHTTPError

try:
    c.sql("DELETE FROM accidents")          # not read-only
except DataPressHTTPError as e:
    print(e.status, e.payload)              # 400 {'error': 'only read-only ...'}
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

## S3 with a dynamic credentials provider

Resolve credentials at startup from a secrets manager instead of
hard-coding them. Pass any zero-argument callable returning an
`HMACKeyPair` as `credentials_provider`. It runs **once** during
`DataPress(...)` construction, the result is cached indefinitely, and it
overrides any inline `access_key_id` / `secret_access_key`.

```python
from datap_rs.datapress import (
    DataPress, DataPressConfig, DatasetConfig, S3Config, HMACKeyPair,
)

def fetch_creds() -> HMACKeyPair:
    secret = my_secrets_client.get("datapress/s3")   # Vault, AWS SM, ...
    return HMACKeyPair(
        access_key=secret["access_key_id"],
        secret_key=secret["secret_access_key"],
    )

s3 = S3Config(
    region="us-east-1",
    endpoint="http://minio.local:9000",
    addressing_style="path",
    allow_http=True,
    credentials_provider=fetch_creds,   # overrides any inline static creds
)

ds  = DatasetConfig(name="events", source="s3://events/2025/", s3=s3)
cfg = DataPressConfig(backend="datafusion", port=8000)
dp  = DataPress(cfg, [ds])             # fetch_creds() called exactly once here
```

### AWS Secrets Manager (boto3)

```python
import json
import boto3
from datap_rs.datapress import S3Config, HMACKeyPair

def aws_secret_provider() -> HMACKeyPair:
    sm = boto3.client("secretsmanager", region_name="us-east-1")
    secret = json.loads(
        sm.get_secret_value(SecretId="datapress/s3")["SecretString"]
    )
    return HMACKeyPair(
        access_key=secret["AWS_ACCESS_KEY_ID"],
        secret_key=secret["AWS_SECRET_ACCESS_KEY"],
    )

s3 = S3Config(region="us-east-1", credentials_provider=aws_secret_provider)
```

### HashiCorp Vault (hvac)

```python
import hvac
from datap_rs.datapress import S3Config, HMACKeyPair

def vault_provider() -> HMACKeyPair:
    client = hvac.Client(url="https://vault.internal:8200")
    client.auth.approle.login(role_id=ROLE_ID, secret_id=SECRET_ID)
    data = client.secrets.kv.v2.read_secret_version(
        path="datapress/s3"
    )["data"]["data"]
    return HMACKeyPair(
        access_key=data["access_key_id"],
        secret_key=data["secret_access_key"],
    )

s3 = S3Config(
    region="us-east-1",
    endpoint="https://s3.internal:9000",
    addressing_style="path",
    credentials_provider=vault_provider,
)
```

### Sharing one provider across datasets with a closure

A single callable can serve every dataset — the result is cached on first
use, so the secrets backend is hit at most once:

```python
from datap_rs.datapress import (
    DataPress, DataPressConfig, DatasetConfig, S3Config, HMACKeyPair,
)

def make_provider(secret_path: str):
    def provider() -> HMACKeyPair:
        secret = my_secrets_client.get(secret_path)
        return HMACKeyPair(secret["access_key_id"], secret["secret_access_key"])
    return provider

events_creds = make_provider("datapress/events")

datasets = [
    DatasetConfig(
        name="events",
        source="s3://events/2025/",
        s3=S3Config(region="us-east-1", credentials_provider=events_creds),
    ),
    DatasetConfig(
        name="events_archive",
        source="s3://events/archive/",
        s3=S3Config(region="us-east-1", credentials_provider=events_creds),
    ),
]

dp = DataPress(DataPressConfig(backend="datafusion", port=8000), datasets)
```

!!! note "Error handling"

    The callable must return an `HMACKeyPair` with both keys non-empty.
    Anything else — a wrong return type, an empty key, or an exception
    raised inside the callable — surfaces as a `ValueError` (or the
    original exception) from `DataPress(...)`, so a misconfigured provider
    fails fast at startup rather than at first query.


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
- dataset-scoped optional scopes such as `datasets:accidents:read`,
  `datasets:accidents:reload`, `datasets:events:read`, and
  `datasets:events:reload`
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

## OIDC scopes per dataset

`AuthConfig` is attached to one `DataPress` server instance. That makes
the strict dataset-isolation pattern simple and explicit: run one server
per dataset (or per access domain), give each instance dataset-named
scopes, and let your gateway expose them under the paths you want.

The bundled Keycloak realm includes optional dataset scopes for these
examples. A token with `datasets:accidents:read` can call the accidents
server, but it will not satisfy the events server, which expects
`datasets:events:read`.

```python
# serve_dataset_scopes.py
import asyncio
from datap_rs.datapress import (
    AuthConfig,
    DataPress,
    DataPressConfig,
    DatasetConfig,
)

ISSUER = "http://localhost:8080/realms/datapress"
AUDIENCE = "datapress-api"


def auth_for(dataset: str) -> AuthConfig:
    return AuthConfig(
        enabled=True,
        issuer=ISSUER,
        audience=AUDIENCE,
        read_scopes=[f"datasets:{dataset}:read"],
        reload_scopes=[f"datasets:{dataset}:reload"],
        algorithms=["RS256"],
        admin_token_fallback=False,
    )


async def serve_dataset(name: str, source: str, port: int) -> None:
    cfg = DataPressConfig(
        backend="duckdb",
        listen="127.0.0.1",
        port=port,
        prefix=f"/{name}",
    )
    dataset = DatasetConfig(name=name, source=source, format="parquet")
    await DataPress(cfg, datasets=[dataset], auth=auth_for(name)).run()


async def main() -> None:
    await asyncio.gather(
        serve_dataset("accidents", "data/us_accidents/march_2023.parquet", 8001),
        serve_dataset("events", "data/events/*.parquet", 8002),
    )


if __name__ == "__main__":
    asyncio.run(main())
```

Request a token for only one dataset:

```python
import requests

TOKEN_URL = "http://localhost:8080/realms/datapress/protocol/openid-connect/token"


def token_for(scope: str) -> str:
    return requests.post(
        TOKEN_URL,
        data={
            "grant_type": "client_credentials",
            "client_id": "datapress-api",
            "client_secret": "datapress-secret",
            "scope": scope,
        },
        timeout=5,
    ).json()["access_token"]


accidents_token = token_for("datasets:accidents:read")
headers = {"Authorization": f"Bearer {accidents_token}"}

print(requests.get(
    "http://127.0.0.1:8001/accidents/api/v1/datasets",
    headers=headers,
    timeout=5,
).status_code)  # 200

print(requests.get(
    "http://127.0.0.1:8002/events/api/v1/datasets",
    headers=headers,
    timeout=5,
).status_code)  # 403: token lacks datasets:events:read
```

For a single server that intentionally exposes several datasets to the
same audience, keep the coarse scopes:

```python
auth = AuthConfig(
    enabled=True,
    issuer="http://localhost:8080/realms/datapress",
    audience="datapress-api",
    read_scopes=["datasets:read"],
    reload_scopes=["datasets:reload"],
)
```

A scope the client didn't request — e.g. hitting `/reload` with only
`datasets:read` — returns `403 Forbidden`. An expired or unsigned token
returns `401 Unauthorized`. Tear it all down with
`docker compose down -v` from `examples/keycloak/`.
