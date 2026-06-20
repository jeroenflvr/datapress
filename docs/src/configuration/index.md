---
description: >-
  Configure DataPress from a single TOML file: server settings, datasets,
  S3 / object storage, and equality indexing for low-latency filters.
---

# Configuration

Every DataPress instance reads a single TOML file at startup. By
convention it's called `datasets.toml`; override with the
`DATASETS_CONFIG` environment variable.

It has one `[server]` block and one `[[dataset]]` block per table you
want to expose.

```toml
[server]                  # optional; defaults shown below
backend = "datafusion"    # or "duckdb"
listen  = "127.0.0.1"
port    = 8080

[[dataset]]               # one block per dataset
name = "..."
# source, s3, index, lazy follow
```

## Pages

- [Server settings](server.md) — listen, port, workers, prefix,
  compression, body limits, timeouts, graceful shutdown.
- [Datasets](datasets.md) — `source`, `lazy`, parquet vs delta, local
  files, directories, globs.
- [S3 / object storage](s3.md) — credentials, endpoints, addressing
  styles, per-dataset env overrides.
- [Indexing](indexing.md) — DataFusion equality-index policy.
- [Documentation site](docs-site.md) — enabling the embedded MkDocs site.
- [Authentication](../operations/auth.md) — OIDC / OAuth2 bearer
  validation and scope-based authorization (`[auth]`).

## Optional feature blocks

A few features are opt-in and configured in their own block:

- `[sql]` — the [raw SQL endpoint](../query/sql.md) (`POST /api/v1/sql`).
  Disabled by default; set `enabled = true` to expose it.
- `[auth]` — [OIDC / OAuth2 authentication](../operations/auth.md) with
  scope-based authorization. Disabled by default; requires a binary built
  with the `auth` feature. Set `enabled = true` to enforce bearer tokens.
- `[swagger.oauth2]` — drives the embedded Swagger UI's "Authorize"
  button through an OIDC Authorization Code + PKCE flow. See
  [Authentication › Swagger UI SSO](../operations/auth.md#swagger-ui-sso).

## Examples

A minimal public server (no auth):

```toml
[server]
backend = "datafusion"
listen  = "0.0.0.0"
port    = 8080

[[dataset]]
name = "sales"
source.kind     = "parquet"
source.location = "./data/sales.parquet"
```

The same server with OIDC scope-based authorization — reads require the
`datasets:read` scope and reloads require `datasets:reload`:

```toml
[server]
backend = "datafusion"
listen  = "0.0.0.0"
port    = 8080

[auth]
enabled              = true
issuer               = "https://login.microsoftonline.com/<tenant-id>/v2.0"
audience             = "api://datapress"
read_scopes          = ["datasets:read"]
reload_scopes        = ["datasets:reload"]
anonymous_read       = false
admin_token_fallback = false

[[dataset]]
name = "sales"
source.kind     = "parquet"
source.location = "./data/sales.parquet"
```

To keep reads public but still protect reloads, set
`anonymous_read = true` and leave `read_scopes` empty:

```toml
[auth]
enabled        = true
issuer         = "https://login.microsoftonline.com/<tenant-id>/v2.0"
audience       = "api://datapress"
anonymous_read = true
reload_scopes  = ["datasets:reload"]
```

See [Authentication](../operations/auth.md) for the full field reference,
the equivalent Python `AuthConfig`, and a runnable Keycloak example.
