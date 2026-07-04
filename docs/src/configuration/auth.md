---
description: >-
  Configure OIDC / OAuth2 bearer-token authentication and scope-based
  authorization for the DataPress HTTP API.
---

# Authentication (OIDC / OAuth2)

DataPress can enforce bearer-token authentication on every API request,
validating JWTs against any standards-compliant OpenID Connect / OAuth 2.0
issuer (Microsoft Entra ID, Auth0, Keycloak, Okta, Google, …).

Full setup, IdP options, Python usage, and request-validation internals are
covered in [Operations › Authentication](../operations/auth.md). This page
covers the `[auth]` TOML block.

## Build

Auth is opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,auth
```

When the binary is built without the `auth` feature but
`[auth] enabled = true` is set, the server logs a warning at startup and
skips OIDC enforcement (the legacy `X-Admin-Token` guard still works).

## Configuration

```toml
[auth]
enabled              = true
issuer               = "https://login.microsoftonline.com/<tenant-id>/v2.0"
audience             = "api://datapress"
algorithms           = ["RS256"]            # RS/ES/PS variants only
leeway_secs          = 60
jwks_refresh_secs    = 3600
read_scopes          = ["datasets:read"]
reload_scopes        = ["datasets:reload"]
anonymous_read       = false                # true = keep GETs public
tenant_claim         = "/tid"               # JSON-pointer into JWT claims
allowed_tenants      = ["<tenant-id>"]      # empty = allow any tenant
admin_token_fallback = true                 # keep X-Admin-Token working
start_degraded       = true                 # warn-and-continue if IdP unreachable at boot
```

| Key                    | Default      | Notes                                                                                          |
|------------------------|--------------|-----------------------------------------------------------------------------------------------|
| `enabled`              | `false`      | Master switch. When false the section is a no-op.                                              |
| `issuer`               | *(required)* | OIDC issuer URL. JWKS is discovered from `{issuer}/.well-known/openid-configuration`.          |
| `audience`             | `""`         | Empty disables `aud` validation.                                                               |
| `algorithms`           | `["RS256"]`  | Algorithm allow-list. Only RS/ES/PS variants are accepted.                                     |
| `leeway_secs`          | `60`         | Clock skew tolerance for `exp` / `nbf`.                                                        |
| `jwks_refresh_secs`    | `3600`       | Background JWKS refresh interval (clamped to ≥ 60 s).                                         |
| `read_scopes`          | `[]`         | Required on every read endpoint when `anonymous_read = false`.                                 |
| `reload_scopes`        | `[]`         | Required on `POST .../reload` (unless the admin-token fallback applies).                       |
| `anonymous_read`       | `false`      | When true, read endpoints (`GET /api/...`) don't require a token.                              |
| `tenant_claim`         | `""`         | JSON-pointer (e.g. `/tid`) into JWT claims for tenant filtering.                               |
| `allowed_tenants`      | `[]`         | If set, the `tenant_claim` value must match one entry. Empty = allow any tenant.               |
| `admin_token_fallback` | `true`       | Keep `X-Admin-Token` header working in parallel with OIDC for `reload_scopes`.                 |
| `start_degraded`       | `true`       | If false, an unreachable JWKS at boot fails startup rather than warning and continuing.        |

Health probes (`/healthz`, `/readyz`, `/version`) are always unauthenticated.

## Swagger UI SSO

Add a `[swagger.oauth2]` block so the Swagger UI's "Authorize" button drives
a PKCE flow and injects the resulting token into every "Try it out" request:

```toml
[swagger.oauth2]
issuer    = "https://login.microsoftonline.com/<tenant-id>/v2.0"
client_id = "<swagger-ui-spa-client-id>"
scopes    = ["openid", "profile", "datasets:read", "datasets:reload"]
```

See [Swagger UI](swagger.md#oidc-single-sign-on-optional) for the full field
reference and IdP redirect-URI setup.

## Further reading

- [Operations › Authentication](../operations/auth.md) — detailed setup guide,
  Python `AuthConfig`, request-validation flow, free/self-hosted IdP options,
  and Keycloak quick-start.
