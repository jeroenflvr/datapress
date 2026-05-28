# Python configuration

## `DataPressConfig`

```python
from datap_rs.datapress import DataPressConfig

cfg = DataPressConfig(
    backend="datafusion",         # "datafusion" | "duckdb"
    listen="0.0.0.0",
    port=8000,
    workers=8,
    prefix="",                    # e.g. "/datapress" behind a proxy
    compress=True,
    max_body_bytes=1_048_576,     # 413 above this
    request_timeout_ms=30_000,    # 504 above this; 0 disables
    shutdown_timeout_secs=30,     # SIGTERM/SIGINT grace period
)
```

Every kwarg mirrors the TOML `[server]` block. See
[Configuration › Server](../configuration/server.md) for full semantics.

## `DatasetConfig`

```python
from datap_rs.datapress import DatasetConfig

ds = DatasetConfig(
    name="accidents",
    source="data/accidents.parquet",   # file, dir, glob, or s3://...
    format="parquet",                  # "parquet" | "delta"
    mode="auto",                       # eq-index policy
    description="US accidents 2016-2023",
    lazy=False,
    # DataFusion eq-index policy:
    # mode="list", index_columns=["State","Severity"],
    # index_max_cardinality=100_000,
)
```

| kwarg                  | Meaning                                                              |
|------------------------|----------------------------------------------------------------------|
| `name`                 | URL slug + table name. Required.                                     |
| `source`               | Local path, glob, or `s3://...` URL. Required.                       |
| `format`               | `"parquet"` (default) or `"delta"`.                                  |
| `mode`                 | DataFusion eq-index policy: `"auto"` (default), `"none"`, `"list"`.  |
| `index_columns`        | Required when `mode="list"`.                                         |
| `index_max_cardinality`| Auto-mode cardinality cap. Default 100_000.                          |
| `lazy`                 | DataFusion+parquet only. Stream from disk instead of materialising.  |
| `description`          | Free-form metadata; surfaced by `/api/v1/datasets`.                  |
| `s3`                   | `S3Config` — only for `s3://` sources.                               |

## `S3Config`

```python
from datap_rs.datapress import S3Config

s3 = S3Config(
    region="us-east-1",
    endpoint="http://localhost:9000",   # MinIO / R2 / Wasabi / Backblaze
    addressing_style="path",            # or "virtual"
    allow_http=True,                    # only for non-https endpoints
    access_key_id=None,
    secret_access_key=None,
    session_token=None,
)
```

Credentials fall back to the standard AWS env vars
(`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
`AWS_REGION`) when not set inline. See
[Configuration › S3](../configuration/s3.md) for the full precedence
chain and per-dataset env var overrides.

## `AuthConfig`

Available when the wheel is built with the `auth` Cargo feature
(`maturin build --release --features auth`). Mirrors the TOML `[auth]`
block — see [Operations › Authentication](../operations/auth.md) for
field semantics and the full validation pipeline.

```python
from datap_rs.datapress import AuthConfig

auth = AuthConfig(
    enabled=True,
    issuer="https://login.example.com/realms/datapress",
    audience="datapress-api",
    read_scopes=["datasets:read"],
    reload_scopes=["datasets:reload"],
    anonymous_read=False,
    algorithms=["RS256"],
    leeway_secs=60,
    jwks_refresh_secs=3600,
    tenant_claim="",                   # e.g. "/tenant"
    allowed_tenants=[],
    admin_token_fallback=True,
    start_degraded=True,
)
```

| kwarg                  | Default          | Meaning                                                                |
|------------------------|------------------|------------------------------------------------------------------------|
| `enabled`              | `False`          | Master switch. When `False` all other fields are ignored.              |
| `issuer`               | `""`             | OIDC issuer URL. Required when `enabled=True`.                         |
| `audience`             | `""`             | Expected `aud` claim. Empty = skip audience check.                     |
| `read_scopes`          | `[]`             | Scopes required for `GET` endpoints.                                   |
| `reload_scopes`        | `[]`             | Scopes required for reload / admin endpoints.                          |
| `anonymous_read`       | `False`          | Allow unauthenticated `GET` requests.                                  |
| `algorithms`           | `["RS256"]`      | Permitted JWT signing algorithms.                                      |
| `leeway_secs`          | `60`             | Clock-skew tolerance for `exp` / `nbf`.                                |
| `jwks_refresh_secs`    | `3600`           | Background JWKS refresh interval.                                      |
| `tenant_claim`         | `""`             | JSON Pointer (`/tenant_id`, `/realm_access/roles/0`, …).               |
| `allowed_tenants`      | `[]`             | Allow-list. Requires `tenant_claim`.                                   |
| `admin_token_fallback` | `True`           | Honour the legacy `X-Admin-Token` header on reload endpoints.          |
| `start_degraded`       | `True`           | Boot even if JWKS fetch fails; all requests rejected until it recovers.|

Pass it to `DataPress(...)` via the `auth=` keyword:

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig, AuthConfig

dp = DataPress(
    DataPressConfig(backend="datafusion", listen="0.0.0.0", port=8000),
    [DatasetConfig(name="accidents", source="data/accidents.parquet")],
    auth=AuthConfig(
        enabled=True,
        issuer="http://localhost:8080/realms/datapress",
        audience="datapress-api",
        read_scopes=["datasets:read"],
        reload_scopes=["datasets:reload"],
    ),
)
import asyncio; asyncio.run(dp.run())
```

Validation rules (raised as `ValueError`):

- `enabled=True` requires a non-empty `issuer`.
- `allowed_tenants` requires `tenant_claim` to be set.
- `tenant_claim` must be a JSON Pointer starting with `/`.

To spin up a local OIDC provider for testing, see
[`examples/keycloak/`](https://github.com/jeroenflvr/fast-api/tree/main/examples/keycloak)
— one `docker compose up` and you have a pre-provisioned realm with
client `datapress-api` and the right scopes.
