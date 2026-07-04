---
description: >-
  Configure the embedded Swagger UI and OpenAPI spec endpoint, including
  optional OIDC single-sign-on for the "Authorize" button.
---

# Swagger UI

DataPress embeds a [Swagger UI][swaggerui] that documents every API endpoint
and lets you try them out directly in the browser. It is served at a
configurable path (default `/docs`) alongside a machine-readable OpenAPI JSON
spec at `<path>/openapi.json`.

[swaggerui]: https://swagger.io/tools/swagger-ui/

## Build

Swagger UI is opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger
```

When the binary is built without the `swagger` feature but
`[swagger] enabled = true` is set in the TOML, the server logs a warning at
startup and skips the mount. You can disable it at runtime without
recompiling by setting `enabled = false`.

## Configuration

```toml
[swagger]
enabled = true     # default; set false to suppress the UI
path    = "/docs"  # mount point for the UI and openapi.json
```

| Key       | Default   | Notes                                                                                              |
|-----------|-----------|----------------------------------------------------------------------------------------------------|
| `enabled` | `true`    | Master switch. Set `false` to suppress the UI at runtime.                                          |
| `path`    | `"/docs"` | Mount point. The UI is served at `<path>` and the spec at `<path>/openapi.json`. Must start with `/` and not end with `/`. |

The spec is always generated from the current binary's registered routes —
there is no separate spec file to maintain.

## OIDC single-sign-on (optional)

If `[auth]` is enabled, "Try it out" requests from the Swagger UI need an
`Authorization: Bearer …` token. Add a `[swagger.oauth2]` block to give the
UI an **"Authorize"** button that runs a full Authorization Code + PKCE flow
against your IdP:

```toml
[swagger.oauth2]
issuer    = "https://login.microsoftonline.com/<tenant-id>/v2.0"
client_id = "<swagger-ui-spa-client-id>"
scopes    = ["openid", "profile", "datasets:read", "datasets:reload"]
# pkce = true   # default; disable only if your IdP doesn't support PKCE
```

| Key         | Default    | Notes                                                                                       |
|-------------|------------|---------------------------------------------------------------------------------------------|
| `issuer`    | *(required)* | OIDC issuer URL. Swagger UI discovers `authorization_endpoint` and `token_endpoint` from `{issuer}/.well-known/openid-configuration`. Must not end in `/`. |
| `client_id` | *(required)* | Public (SPA) OAuth2 client ID registered with the IdP. No client secret — the flow is PKCE-only. |
| `scopes`    | `[]`       | Scopes pre-checked in the authorize dialog. `openid` is always included.                    |
| `pkce`      | `true`     | Use PKCE for the code flow. Disable only if the IdP doesn't support it for public clients.  |

Register `https://<your-host>/docs/oauth2-redirect.html` as an allowed
redirect URI on the IdP client. Tokens acquired through the UI are attached
as `Authorization: Bearer …` to every "Try it out" request.

!!! note
    `[swagger.oauth2]` drives the **UI only** — it does not enable server-side
    token validation. To enforce bearer tokens on the API, configure `[auth]`
    separately. See [Authentication (OIDC / OAuth2)](../operations/auth.md).

## Disabling for production

The UI is useful during development but you may want to hide it from the
public internet in production:

```toml
[swagger]
enabled = false
```

The OpenAPI spec at `<path>/openapi.json` is also suppressed when
`enabled = false`. If you need machine-readable API docs without the
interactive UI, keep `enabled = true` and restrict access at the network
or reverse-proxy layer.

## From Python

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

config = DataPressConfig(
    backend="duckdb",
    port=8080,
    swagger_enabled=True,
    swagger_path="/docs",
)
```
