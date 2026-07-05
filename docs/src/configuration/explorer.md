---
description: >-
  Configure the DataPress dataset explorer UI — the built-in server-rendered
  web app for browsing datasets, schemas, and running queries in the browser.
---

# Explorer UI

DataPress ships an optional browser-based explorer that lets you browse
registered datasets, inspect schemas, run structured API queries, and open
a live DuckDB-WASM terminal — all without leaving the browser.

It is served at a configurable path (default `/explore`) directly by the
DataPress process. No separate web server is needed.

## Build

The explorer is opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,explorer
```

When the binary is built without the `explorer` feature but
`[explorer] enabled = true` is set in the TOML, the server logs a warning
at startup and skips the mount. The feature flag has no effect on the HTTP
API.

## Configuration

```toml
[explorer]
enabled = true        # default; set false to hide the UI at runtime
path    = "/explore"  # mount point
```

| Key       | Default      | Notes                                                                                          |
|-----------|--------------|-----------------------------------------------------------------------------------------------|
| `enabled` | `true`       | Master switch. Set `false` to suppress the UI even when the feature is compiled in.            |
| `path`    | `"/explore"` | Mount point. Must start with `/` and not end with `/`. Cannot collide with `/api`, `/health*`, `/version`, or other reserved mounts. |

## What it shows

Open `http://localhost:8080/explore` (or your configured path) in a browser.

### Discovery tab

Lists every registered dataset with:

- row count, column count, and backing file size
- source kind (parquet / delta / lazy) and location
- schema — column names and inferred types
- equality index configuration (when enabled)

Datasets are sortable by name, row count, or column count.

### API Query tab

An in-browser query builder with two modes:

**Structured JSON** — builds a `POST /api/v1/datasets/<name>/query` request
(the same structured `QueryRequest` body used by the HTTP API). Supports
column projection, predicates, grouping, aggregation, sorting, pagination,
and result export as CSV, JSON, or Parquet (Parquet export uses a bundled
DuckDB-WASM instance entirely in-browser — no server round-trip).

**Raw SQL** — sends a `POST /api/v1/sql` request. This tab is only active
when `[sql] enabled = true`; otherwise it shows a warning banner. A
"To JSON query" button translates the SQL to a structured `QueryRequest`
and switches back to JSON mode — useful when the SQL endpoint is disabled
but you want to draft a query interactively.

Both modes support an **Arrow IPC** response toggle for faster large
results, custom request headers, and a timing readout showing time-to-first-
byte, total transfer time, body size, and row count.

### DuckDB terminal tab

An embedded DuckDB-WASM terminal that queries the Parquet export of each
dataset directly in the browser. The terminal connects to the server's
Parquet download endpoint for each dataset; no SQL is executed server-side
and no credentials are transmitted to the WASM sandbox. The DuckDB-WASM
bundle is self-hosted and served by DataPress itself (no CDN).

## OIDC single-sign-on (optional)

If `[auth]` is enabled, the explorer's **API Query** requests need an
`Authorization: Bearer …` token. Add an `[explorer.oauth2]` block to give the
API Query tab an **"Authorize"** button that runs a full Authorization Code +
PKCE flow against your IdP — the same flow the [Swagger UI](swagger.md#oidc-single-sign-on-optional)
offers:

```toml
[explorer.oauth2]
issuer    = "https://login.microsoftonline.com/<tenant-id>/v2.0"
client_id = "<explorer-spa-client-id>"
scopes    = ["openid", "profile", "datasets:read"]
# pkce = true   # default; disable only if your IdP doesn't support PKCE
```

| Key         | Default      | Notes                                                                                       |
|-------------|--------------|---------------------------------------------------------------------------------------------|
| `issuer`    | *(required)* | OIDC issuer URL. The endpoints are discovered from `{issuer}/.well-known/openid-configuration` at startup. Must not end in `/`. |
| `client_id` | *(required)* | Public (SPA) OAuth2 client ID registered with the IdP. No client secret — the flow is PKCE-only. |
| `scopes`    | `[]`         | Scopes requested by default. `openid` is always included.                                   |
| `pkce`      | `true`       | Use PKCE for the code flow. Disable only if the IdP doesn't support it for public clients.   |

Register `https://<your-host>/explore/oauth2-redirect.html` (matching your
`path`) as an allowed redirect URI on the IdP client. When you click
**Authorize**, DataPress opens a login popup; after sign-in, the token is
attached as `Authorization: Bearer …` to every request the API Query tab
makes. The token is held in the browser session only (`sessionStorage`) and
cleared on sign-out.

The endpoints are resolved once at startup. If discovery fails (unreachable
issuer, CORS, or a metadata document missing the required endpoints),
DataPress logs a warning and serves the explorer **without** the Authorize
button rather than a broken dialog.

!!! note
    `[explorer.oauth2]` drives the **UI only** — it does not enable server-side
    token validation. To enforce bearer tokens on the API, configure `[auth]`
    separately. See [Authentication (OIDC / OAuth2)](../operations/auth.md).

!!! tip "Try it locally with the bundled Keycloak"
    The repo ships a turnkey OIDC stack at
    [`examples/keycloak/`](https://github.com/jeroenflvr/datapress/tree/main/examples/keycloak).
    Run `docker compose up -d` there and it pre-provisions a public
    `datapress-explorer` client with the
    `http://localhost:8000/explore/oauth2-redirect.html` redirect URI already
    registered, so the **Authorize** button works out of the box:

    ```toml
    [explorer.oauth2]
    issuer    = "http://localhost:8080/realms/datapress"
    client_id = "datapress-explorer"
    scopes    = ["datasets:read", "datasets:reload"]
    ```

    See the [Keycloak walkthrough](../operations/auth.md#local-only-quick-start-with-keycloak)
    and [Python examples](../python/examples.md#browser-sign-in-swagger-ui-explorer)
    for the full end-to-end setup.

## Security note

The explorer uses the same session / cookie context as the browser. If
`[auth]` is enabled, the explorer's API requests inherit the browser's
`Authorization` header from the page that opened the explorer — or the
session cookie for same-origin requests. When `[explorer.oauth2]` is
configured, the API Query tab can also sign in directly via the **Authorize**
button. There is no separate explorer credential otherwise. Disable the
explorer (`enabled = false`) or restrict network access if you do not want
the UI reachable from untrusted networks.

## From Python

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

config = DataPressConfig(
    backend="duckdb",
    port=8080,
    explorer_enabled=True,
    explorer_path="/explore",
    # Optional: Authorize button (Authorization Code + PKCE) on the
    # API Query tab. Drives the UI only — configure AuthConfig to
    # actually enforce tokens on the API.
    explorer_oauth2_issuer="https://issuer.example.com",
    explorer_oauth2_client_id="datapress-explorer",
    explorer_oauth2_scopes=["openid", "profile", "datasets:read"],
    explorer_oauth2_pkce=True,
)
```

