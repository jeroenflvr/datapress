# Authentication (OIDC / OAuth2)

DataPress can validate incoming requests against any standards-compliant
OpenID Connect / OAuth 2.0 issuer (Microsoft Entra ID, Auth0, Keycloak,
Okta, Google, GitHub via an OIDC bridge, …). When enabled, every
request to `/api/...` must carry an `Authorization: Bearer <jwt>`
header that the server validates against the issuer's JWKS.

Health probes (`/healthz`, `/readyz`, `/version`) stay unauthenticated
so load balancers and Kubernetes liveness/readiness checks keep working.

## Build

The auth layer is opt-in at compile time so binaries without it stay
slim:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,auth
```

When the binary is built without `auth` but `[auth] enabled = true` in
the TOML, the server logs a warning at startup and skips OIDC
enforcement (the legacy `X-Admin-Token` guard still works).

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
anonymous_read       = false                # set true to keep GETs public
tenant_claim         = "/tid"               # JSON-pointer into JWT claims
allowed_tenants      = ["<tenant-id>"]      # empty = allow any tenant
admin_token_fallback = true                 # keep X-Admin-Token working
start_degraded       = true                 # warn-and-continue if IdP is down at boot
```

| Key                    | Default     | Notes                                                                  |
|------------------------|-------------|------------------------------------------------------------------------|
| `enabled`              | `false`     | Master switch. When false the section is a no-op.                      |
| `issuer`               | *(required)*| Must be `https://...`. JWKS fetched from `{issuer}/.well-known/jwks.json`. |
| `audience`             | `""`        | Empty disables `aud` validation.                                       |
| `algorithms`           | `["RS256"]` | Allow-list. Only RS/ES/PS variants are accepted.                       |
| `leeway_secs`          | `60`        | Clock skew tolerance for `exp` / `nbf`.                                |
| `jwks_refresh_secs`    | `3600`      | Background refresh interval (clamped to ≥ 60s).                        |
| `read_scopes`          | `[]`        | Required on every read endpoint when `anonymous_read = false`.         |
| `reload_scopes`        | `[]`        | Required on `POST .../reload` (unless the admin token fallback hits).  |
| `anonymous_read`       | `false`     | When true, read endpoints don't require a token at all.                |
| `tenant_claim`         | `""`        | JSON-pointer (e.g. `/tid`, `/org/id`) into the JWT claims.             |
| `allowed_tenants`      | `[]`        | If set, the token's `tenant_claim` value must be in this list.         |
| `admin_token_fallback` | `true`      | If true, `X-Admin-Token` still satisfies `reload_scopes`.              |
| `start_degraded`       | `true`      | If false, an unreachable JWKS at boot fails startup.                   |

## How requests are validated

1. Middleware extracts `Authorization: Bearer <jwt>`. No header →
   request is passed through; handlers will reject it if a scope is
   required (anonymous reads stay open when `anonymous_read = true`).
2. JWT header `kid` is looked up in the cached JWKS. Unknown `kid`
   triggers a single refresh.
3. Signature is verified with the matching JWK, then `iss`, `aud`,
   `exp`, `nbf`, and algorithm are checked against the allow-list.
4. Scopes from `scope` (space-separated) or `scp` (string or array) are
   parsed and lower-cased. The required scope list for the route must
   be a subset.
5. If `allowed_tenants` is non-empty, the value at `tenant_claim` must
   match one of them.

Failures produce a `401` with `WWW-Authenticate: Bearer realm="datapress"`
(bad/missing token) or `403` (valid token, missing scope or wrong
tenant).

## Swagger UI SSO

Add an `[swagger.oauth2]` block to make the embedded Swagger UI act as
an OIDC client (Authorization Code + PKCE):

```toml
[swagger.oauth2]
issuer    = "https://login.microsoftonline.com/<tenant-id>/v2.0"
client_id = "<swagger-ui-spa-client-id>"
scopes    = ["openid", "profile", "datasets:read", "datasets:reload"]
```

Register the Swagger UI URL (`https://<host>/docs/oauth2-redirect.html`)
as a redirect URI on the IdP side. The "Authorize" button in `/docs`
will then run the full PKCE flow and inject the resulting access token
into every "Try it out" request.

## Migrating from `X-Admin-Token`

`admin_token_fallback = true` (the default) keeps the existing
`X-Admin-Token: $ADMIN_TOKEN` header working in parallel with OIDC so
you can roll OIDC out without breaking existing automation. Flip it to
`false` once every reload-caller is using a real token.

## Free / self-hostable OIDC providers for testing

You don't need a paid identity tenant to exercise the auth layer.
Anything that publishes a standards-compliant `/.well-known/jwks.json`
and signs with RS256/ES256 will work.

### Self-hosted (zero cost, full control)

- **[Keycloak](https://www.keycloak.org/)** — the reference open-source
  IdP. `docker run quay.io/keycloak/keycloak start-dev` gives you a
  working issuer in under a minute. Recommended for local development.
- **[Authentik](https://goauthentik.io/)** — modern Go/Python IdP,
  Docker-friendly, good admin UI.
- **[Zitadel](https://zitadel.com/opensource)** — open-source, also
  offered as a hosted free tier (see below).
- **[Ory Hydra](https://www.ory.sh/hydra/)** — OAuth2/OIDC server only
  (no user DB), pairs with Ory Kratos for accounts.
- **[Dex](https://dexidp.io/)** — small OIDC front-end that federates
  to GitHub/Google/LDAP/etc. Popular in Kubernetes setups.

### Free hosted tiers

- **[Auth0 Free](https://auth0.com/pricing)** — 25 000 MAU, full OIDC.
- **[Okta Developer / Auth0 by Okta](https://developer.okta.com/)** —
  free developer tenants with unlimited test users.
- **[Microsoft Entra ID Free](https://www.microsoft.com/security/business/identity-access/microsoft-entra-id-pricing)** —
  comes with any Microsoft account; perfect for `tid`-based multi-tenant
  testing.
- **[Zitadel Cloud Free](https://zitadel.com/pricing)** — 25 000 auth
  requests / month.
- **[Logto Cloud Free](https://logto.io/pricing)** — generous dev tier.
- **[FusionAuth](https://fusionauth.io/pricing)** — community edition
  is free to self-host; hosted tiers exist too.
- **Google Identity / "Sign in with Google"** — free OIDC issuer at
  `https://accounts.google.com`; good for read-only personal demos but
  no custom scopes/audiences.

### Local-only quick start with Keycloak

!!! tip "Turnkey stack"

    The repo ships a ready-to-go compose file at
    [`examples/keycloak/`](https://github.com/jeroenrosenberg/datapress/tree/main/examples/keycloak)
    with a pre-provisioned realm, service-account client, scopes, and a
    test user — `docker compose up -d` and you're done. The manual
    instructions below mirror what that file automates.

```bash
docker run --rm -p 8080:8080 \
  -e KEYCLOAK_ADMIN=admin \
  -e KEYCLOAK_ADMIN_PASSWORD=admin \
  quay.io/keycloak/keycloak:25.0 start-dev
```

Then in the admin console (http://localhost:8080):

1. Create a realm, e.g. `datapress`.
2. Create a client `datapress-api` (Client type: OpenID Connect,
   "Service accounts roles" enabled for client-credentials flows).
3. Define client scopes `datasets:read` and `datasets:reload` and
   assign them to the client.
4. Point DataPress at it:

   ```toml
   [auth]
   enabled       = true
   issuer        = "http://localhost:8080/realms/datapress"
   audience      = "datapress-api"
   read_scopes   = ["datasets:read"]
   reload_scopes = ["datasets:reload"]
   ```

   !!! warning "HTTPS in production"
       The `issuer` URL must be `https://` in real deployments. The
       `http://localhost` form is accepted only because the validator
       treats `localhost` as a development convenience.

5. Mint a token with the client-credentials flow and call the API:

   ```bash
   TOKEN=$(curl -s -X POST \
     http://localhost:8080/realms/datapress/protocol/openid-connect/token \
     -d grant_type=client_credentials \
     -d client_id=datapress-api \
     -d client_secret=<secret> \
     -d scope="datasets:read" | jq -r .access_token)

   curl -H "Authorization: Bearer $TOKEN" \
     http://localhost:8000/api/v1/datasets
   ```
