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
