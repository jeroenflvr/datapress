# Local Keycloak for DataPress

One-command OIDC stack for testing the `auth` feature locally.

## Start

```bash
cd examples/keycloak
docker compose up -d
```

- Admin console: <http://localhost:8080> (`admin` / `admin`)
- Issuer URL: `http://localhost:8080/realms/datapress`
- Realm, clients, and scopes are pre-provisioned from `realm-datapress.json`.

## Pre-provisioned

| Item | Value |
| --- | --- |
| Realm | `datapress` |
| Service-account client | `datapress-api` (secret `datapress-secret`) |
| Swagger UI client (public) | `datapress-swagger` |
| Scopes | `datasets:read`, `datasets:reload`, `datasets:accidents:read`, `datasets:accidents:reload`, `datasets:events:read`, `datasets:events:reload` |
| Test user | `alice` / `alice` |

## Get a service-account token

```bash
curl -s -X POST \
  http://localhost:8080/realms/datapress/protocol/openid-connect/token \
  -d grant_type=client_credentials \
  -d client_id=datapress-api \
  -d client_secret=datapress-secret \
  -d scope='datasets:read datasets:reload' | jq -r .access_token
```

## Point DataPress at it

```toml
# datapress.toml
[auth]
enabled         = true
issuer          = "http://localhost:8080/realms/datapress"
audience        = "datapress-api"
read_scopes     = ["datasets:read"]
reload_scopes   = ["datasets:reload"]
algorithms      = ["RS256"]
```

Then:

```bash
cargo run --features auth -- --config datapress.toml
TOKEN=$(curl -s -X POST http://localhost:8080/realms/datapress/protocol/openid-connect/token \
  -d grant_type=client_credentials -d client_id=datapress-api \
  -d client_secret=datapress-secret -d scope='datasets:read' | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" http://localhost:8000/v1/datasets
```

## Python

```python
import asyncio
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig, AuthConfig

dp = DataPress(
  DataPressConfig(backend="duckdb", listen="127.0.0.1", port=8000),
  [DatasetConfig(name="accidents", source="data/accidents.parquet", format="parquet")],
    auth=AuthConfig(
        enabled=True,
        issuer="http://localhost:8080/realms/datapress",
        audience="datapress-api",
    read_scopes=["datasets:accidents:read"],
    reload_scopes=["datasets:accidents:reload"],
    ),
)
asyncio.run(dp.run())
```

`AuthConfig` applies to one DataPress server instance. For strict
per-dataset scope boundaries, run one Python server per dataset or per
access domain, each with its own `read_scopes` and `reload_scopes`.

## Swagger UI SSO

Add to your DataPress config:

```toml
[swagger.oauth2]
client_id = "datapress-swagger"
issuer    = "http://localhost:8080/realms/datapress"
scopes    = ["datasets:read", "datasets:reload"]
pkce      = true
```

DataPress runs OIDC discovery against
`<issuer>/.well-known/openid-configuration` at startup and emits an
`oauth2` `authorizationCode` flow (with the discovered authorize/token
endpoints) into the OpenAPI spec, so the Swagger UI's **Authorize**
dialog shows the scope checkboxes and login button. If discovery is
unreachable at boot, the docs are still served — just without the
Authorize button.

From Python, set the equivalent kwargs on `DataPressConfig`:

```python
DataPressConfig(
    backend="duckdb",
    swagger_oauth2_issuer="http://localhost:8080/realms/datapress",
    swagger_oauth2_client_id="datapress-swagger",
    swagger_oauth2_scopes=["datasets:read", "datasets:reload"],
    swagger_oauth2_pkce=True,
)
```

## Stop / reset

```bash
docker compose down        # stop
docker compose down -v     # stop + wipe data
```
