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
| Scopes | `datasets:read`, `datasets:reload` |
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
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig, AuthConfig

dp = DataPress(
    DataPressConfig(host="127.0.0.1", port=8000),
    [DatasetConfig(name="demo", backend="duckdb", source_kind="parquet",
                   source_uri="data/demo.parquet")],
    auth=AuthConfig(
        enabled=True,
        issuer="http://localhost:8080/realms/datapress",
        audience="datapress-api",
        read_scopes=["datasets:read"],
        reload_scopes=["datasets:reload"],
    ),
)
dp.serve()
```

## Swagger UI SSO

Add to your DataPress config:

```toml
[swagger.oauth2]
client_id          = "datapress-swagger"
authorization_url  = "http://localhost:8080/realms/datapress/protocol/openid-connect/auth"
token_url          = "http://localhost:8080/realms/datapress/protocol/openid-connect/token"
scopes             = ["datasets:read", "datasets:reload"]
use_pkce           = true
```

## Stop / reset

```bash
docker compose down        # stop
docker compose down -v     # stop + wipe data
```
