# Server settings

The `[server]` block is fully optional — every field has a sensible
default.

```toml
[server]
backend = "datafusion"    # "datafusion" (default) | "duckdb"
listen  = "127.0.0.1"     # bind address; "0.0.0.0" to expose
port    = 8080
# workers = 8             # omit for one worker per CPU
# prefix  = "/datapress"  # mount every route under this path
# compress = true
# max_body_bytes        = 1048576    # 413 above this
# max_page_size         = 100000     # clamp query page_size above this
# force_lazy_above_mb   = 0          # >0: force lazy for datasets larger than this
# request_timeout_ms    = 30000      # 504 above this; 0 disables
# shutdown_timeout_secs = 30         # SIGTERM grace period
# environment           = "production"  # Explorer navbar badge label; unset = no badge
# environment_color     = "danger"      # Bootstrap colour for the badge (see table below)

[server.quack]                      # DuckDB backend only; experimental
enabled = false
uri = "quack:localhost"             # default port 9494; use literal localhost
# token = "change-me"               # optional; generated and logged if omitted
allow_other_hostname = false        # true for quack:0.0.0.0:9494 behind TLS proxy
read_only = true                    # allow reads plus Quack attach handshake

[server.pgwire]                     # DataFusion backend only; opt-in build feature
enabled  = false
listen   = "127.0.0.1"              # loopback-only unless a password + TLS are set
port     = 5432
username = "datapress"
# password  = "change-me"           # required for any non-loopback bind
# tls_cert  = "/etc/datapress/pg.crt"   # PEM cert; set together with tls_key
# tls_key   = "/etc/datapress/pg.key"   # PKCS#8 key; set together with tls_cert

[swagger]
enabled = true     # default; set false to suppress the UI
path    = "/docs"  # mount point for the UI and openapi.json

[metrics]
enabled = true       # off by default
path    = "/metrics" # scrape path

[docs]
enabled = true           # default: false
path    = "/mkdocs"      # default: /mkdocs

[explorer]
enabled = true        # default; set false to hide the UI at runtime
path    = "/explore"  # mount point

[explorer.oauth2]
client_id = "datapress-explorer"
issuer    = "http://localhost:8080/realms/datapress"
scopes    = ["datasets:read", "datasets:reload"]
pkce      = true


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


## Reference

| Field                   | Default     | Notes                                                                                     |
|-------------------------|-------------|-------------------------------------------------------------------------------------------|
| `backend`               | `datafusion`| Informational hint logged at startup. Each binary always runs as its own backend regardless. |
| `listen`                | `127.0.0.1` | Loopback by default — the service is **not** network-exposed unless you opt in.           |
| `port`                  | `8080`      | TCP port.                                                                                 |
| `workers`               | *(unset)*   | Actix worker threads. Unset = one per CPU.                                                |
| `prefix`                | `""`        | URL prefix in front of every app route. Must start with `/` and not end with `/`.         |
| `compress`              | `true`      | Negotiate gzip / brotli / zstd via `Accept-Encoding`.                                     |
| `max_body_bytes`        | `1048576`   | Max accepted JSON request body. Larger → `413 Payload Too Large`.                         |
| `max_page_size`         | `100000`    | Max rows returned by one `/query` page. Larger `page_size` values are clamped.             |
| `force_lazy_above_mb`   | `0`         | `>0`: datasets whose backing files exceed this many MiB are forced into `lazy` mode at startup. `0` disables. Local sources are stat'd; S3 sources are sized on the `datafusion` backend by listing the object store (the `duckdb` backend sizes local sources only). Delta is measured by its parquet data files. |
| `request_timeout_ms`    | `30000`     | Per-request handler timeout (ms). Long handlers are cancelled and the client gets `504`. `0` disables. |
| `shutdown_timeout_secs` | `30`        | Grace period for in-flight requests after `SIGTERM` / `SIGINT`.                           |
| `environment`           | *(unset)*   | Label shown as a badge in the Explorer navbar, e.g. `"development"`, `"staging"`, `"production"`. Unset = no badge. |
| `environment_color`     | *(auto)*    | Bootstrap colour name for the badge (see table below). Overrides the colour otherwise inferred from the environment name. Only meaningful when `environment` is set. |

## Environment badge

When `environment` is set, the Explorer navbar shows a coloured badge next to
the logo — handy to tell dev / staging / production apart at a glance. If
`environment_color` is omitted the colour is inferred from the name
(`production`/`prod` → red, `staging`/`stage`/`uat` → yellow,
`development`/`dev`/`local` → green, anything else → grey). Set
`environment_color` to any Bootstrap colour to override it:

| name      | hex       | color |
|-----------|-----------|-------|
| primary   | `#0d6efd` | 🟦    |
| secondary | `#6c757d` | ⬜    |
| success   | `#198754` | 🟩    |
| info      | `#0dcaf0` | 🟦    |
| warning   | `#ffc107` | 🟨    |
| danger    | `#dc3545` | 🟥    |
| light     | `#f8f9fa` | ⬜    |
| dark      | `#212529` | ⬛    |

## DuckDB Quack server

DuckDB builds can optionally start DuckDB's experimental <sup>1</sup> Quack remote
protocol server after datasets are registered:

```toml
[server]
backend = "duckdb"

[server.quack]
enabled = true
uri = "quack:localhost"       # default port 9494
token = "analytics-token"     # optional, but recommended
read_only = true              # default
```

Quack exposes the DuckDB SQL surface of the in-process database. DataPress
therefore keeps it disabled by default, binds to localhost by default, and
installs a read-only authorization hook by default. If `token` is omitted,
Quack generates one at startup and DataPress logs it once.

With `read_only = true`, DataPress allows read/inspection statements and
the Quack client attach handshake, but rejects write-oriented and DDL
statements such as `CREATE`, `INSERT`, `UPDATE`, `DELETE`, `COPY`,
`DROP`, `ALTER`, `LOAD`, and `INSTALL`.

DuckDB's Quack extension currently treats only the literal hostname
`localhost` as local. Use `uri = "quack:localhost"`; `quack:127.0.0.1`
is rejected unless `allow_other_hostname = true`.

To listen on a non-local address, set both a non-local URI and
`allow_other_hostname = true`, then put a TLS-terminating reverse proxy in
front of it:

```toml
[server.quack]
enabled = true
uri = "quack:0.0.0.0:9494"
allow_other_hostname = true
token = "analytics-token"
```

DuckDB CLI clients can connect with a Quack secret:

```sql
CREATE SECRET (
	TYPE quack,
	TOKEN 'analytics-token',
	SCOPE 'quack:localhost'
);

ATTACH 'quack:localhost' AS datapress (TYPE quack);
FROM datapress.accidents LIMIT 10;
```

Or simplified, using the secret directly in ATTACH statement:

```sql
ATTACH 'quack:localhost' AS datapress (TOKEN 'analytics-token');
FROM datapress.accidents LIMIT 10;
```

### Connecting to a remote host

For any host other than `localhost`, Quack defaults to **HTTPS**. If the
server is reached over plain HTTP (for example in development, or before a
TLS-terminating proxy is in place), the attach will fail unless you pass
`DISABLE_SSL true`:

```sql
ATTACH 'quack:remote_ip' AS remote_db (TOKEN 'analytics-token', DISABLE_SSL true);
FROM remote_db.accidents LIMIT 10;
```

Omit `DISABLE_SSL true` (the default) when the server is fronted by TLS.

<sup>1</sup> Quack is still highly experimental. Among other things, `SHOW TABLES;` is not yet supported.


## PostgreSQL wire protocol (pgwire)

!!! warning "Experimental"
    The pgwire endpoint is **experimental** — behaviour and configuration may
    change between releases, and some Postgres clients or introspection queries
    may not work yet. It's not recommended for production-critical workloads.

DataFusion builds compiled with the `pgwire` Cargo feature can expose a
**PostgreSQL wire-protocol** endpoint alongside the HTTP API. Any PostgreSQL
client — `psql`, JDBC/ODBC drivers, or BI tools like Power BI and Tableau —
can then query your datasets directly. Each dataset appears as a table in the
default catalog/schema (`public` to Postgres clients).

The endpoint is **read-only** (DataFusion has no write path here) and **off by
default**. It is only available on the `datafusion` backend and only when the
binary/wheel was built with the `pgwire` feature; a `pgwire.enabled = true`
config on a build without the feature logs a warning and is otherwise a no-op.

```toml
[server]
backend = "datafusion"

[server.pgwire]
enabled  = true
listen   = "127.0.0.1"
port     = 5432
username = "datapress"
# password = "change-me"
```

Authentication is **cleartext password**, so DataPress enforces these rules at
startup (the process refuses to start otherwise):

- A **loopback** bind (`127.0.0.1` / `::1`) may omit the password — handy for
  local development.
- Any **non-loopback** bind (for example `0.0.0.0`) **requires** both a
  `password` **and** TLS (`tls_cert` + `tls_key`), so credentials never cross
  the network in the clear.
- `tls_cert` and `tls_key` must be set together (both or neither).

Enabling TLS for a network-exposed listener:

```toml
[server.pgwire]
enabled  = true
listen   = "0.0.0.0"
port     = 5432
username = "datapress"
password = "change-me"
tls_cert = "/etc/datapress/pg.crt"   # PEM certificate
tls_key  = "/etc/datapress/pg.key"   # PKCS#8 private key
```

| Field      | Default       | Notes                                                                                  |
|------------|---------------|----------------------------------------------------------------------------------------|
| `enabled`  | `false`       | Master switch. Requires the `pgwire` feature and `backend = "datafusion"`.              |
| `listen`   | `127.0.0.1`   | Bind address. Loopback-only unless a `password` **and** TLS are configured.             |
| `port`     | `5432`        | TCP port (the PostgreSQL default).                                                      |
| `username` | `datapress`   | Username clients must present.                                                          |
| `password` | *(unset)*     | Cleartext password. Optional on loopback; required for any non-loopback bind.           |
| `tls_cert` | *(unset)*     | PEM certificate path. Enables TLS; must be paired with `tls_key`.                       |
| `tls_key`  | *(unset)*     | PKCS#8 private-key path. Must be paired with `tls_cert`.                                 |

Connect with `psql`:

```bash
psql "host=127.0.0.1 port=5432 user=datapress password=change-me dbname=datapress" \
  -c "SELECT count(*) FROM accidents"
```

See [PostgreSQL (pgwire)](../clients/postgresql.md) for client-specific setup
(BI tools, DBeaver/DataGrip, connection strings, TLS).


## Behind a reverse proxy

When nginx / Traefik / Caddy forwards a path prefix verbatim, set
`prefix` so app routes match:

```toml
[server]
prefix = "/datapress"
# → GET /datapress/api/v1/datasets, GET /datapress/health,
#   GET /datapress/healthz, GET /datapress/readyz, ...
```

Every route — including the probes `/healthz`, `/readyz`, `/version` — is
mounted under the prefix. Orchestrator liveness and readiness probe paths
must include it.

## Compression

On by default and negotiated per request via `Accept-Encoding`. Clients
that want raw JSON send `Accept-Encoding: identity` or omit the header.
Disable when sitting behind a proxy that already compresses, or to save
CPU on a trusted LAN.

```toml
[server]
compress = false
```

## Request size limit

```toml
[server]
max_body_bytes = 10_485_760   # 10 MiB
```

`max_body_bytes` is an **incoming request-body** limit. It applies to
the bytes the client sends to DataPress: for example the JSON body of a
`POST /api/v1/datasets/{name}/query` request. It is wired into both
Actix's JSON extractor and raw payload extractor (`web::JsonConfig` and
`web::PayloadConfig`). Oversized requests are rejected with
`413 Payload Too Large` before the query handler runs.

It is not a response-size limit. DataPress does not truncate JSON or
Arrow IPC responses at `max_body_bytes`, and it does not drop rows to
make a response fit that value. Response size is determined by the query
result: selected columns, row count, Arrow/JSON encoding overhead, and
optional HTTP compression.

For query requests the order is:

1. The HTTP request body must fit within `max_body_bytes`.
2. The JSON body is parsed into the query request.
3. `page` is normalized to at least `1`; `page_size` is clamped to `[1, max_page_size]`.
4. The backend applies `page`, `page_size`, and optional top-level `limit` to choose rows.
5. The chosen rows are encoded as JSON or Arrow IPC.

That means a small query body can legitimately produce a much larger
response. If `max_body_bytes = 10_485_760` and an Arrow IPC query with
`page_size = 1000` returns about 10 MiB, the two numbers only correlate
by coincidence unless the client, proxy, or load balancer has its own
separate response-size limit. DataPress itself uses `max_body_bytes` only
on the request side.

To control response size, reduce `page_size`, project fewer `columns`,
add more selective `predicates`, or page through the result set. See
[Arrow IPC vs JSON](../query/arrow-ipc.md#response-size-and-max_body_bytes)
for the Arrow-specific details.

## Query page-size limit

```toml
[server]
max_page_size = 100_000
```

`max_page_size` controls the largest row page a `/query` request can
ask for. The default is `100_000`. If a client sends a larger
`page_size`, DataPress clamps it to `max_page_size`; the response reports
the effective value in the JSON body or Arrow IPC `X-Page-Size` header.

This is separate from `max_body_bytes`: `max_page_size` limits rows in
the response page, while `max_body_bytes` limits bytes in the incoming
request body.

## Request timeout

```toml
[server]
request_timeout_ms = 60_000   # 60 s
# request_timeout_ms = 0      # disabled
```

A handler that doesn't produce a response within `request_timeout_ms`
is cancelled at the next `.await` point and the client sees
`504 Gateway Timeout` with body `{"error":"request timed out"}`.

## Graceful shutdown

```toml
[server]
shutdown_timeout_secs = 30
```

On `SIGTERM` or `SIGINT`:

1. The listening socket is closed — no new connections.
2. In-flight requests get up to `shutdown_timeout_secs` to drain.
3. Workers are stopped.

Set this **lower** when fast restarts matter more than slow handlers;
set it **higher** for long-running aggregations or large parquet
exports. The startup log records which signal triggered shutdown:

```
INFO  Received SIGTERM, shutting down gracefully (up to 30s for in-flight requests)...
INFO  Shutdown complete.
```

See [Operations › Graceful shutdown](../operations/graceful-shutdown.md)
for the orchestrator-side tuning.

## DataFusion performance tuning

The DataFusion backend runs with stock defaults unless you opt in via a
top-level `[datafusion]` block (it is **not** part of `[server]`). Every
knob is **off by default**, so the block changes nothing until you set it.
It mainly helps lazy parquet datasets — especially on S3 — and the DuckDB
backend ignores it entirely.

```toml
[datafusion]
# Evaluate row filters *during* the parquet decode so rows failing a
# predicate are never materialised (on top of the row-group / page-index
# pruning that always happens). Best for selective filters over large row
# groups. Default false.
pushdown_filters = true

# Let the scan reorder pushed-down predicates by selectivity. Only has an
# effect together with pushdown_filters. Default false.
reorder_filters = true

# Cache object-store file listings so repeated lazy queries reuse LIST
# results instead of re-listing the source prefix every time — the dominant
# per-query cost on S3. Default false.
list_files_cache = true

# Memory budget for the listing cache, in MiB. Default 64.
list_files_cache_mb = 64

# How long a cached listing stays valid, in seconds. Bounds how long newly
# written files take to become visible without a reload. 0 = never expires.
# Default 60.
list_files_cache_ttl_secs = 60
```

| Field                       | Default | Notes                                                                                                                  |
|-----------------------------|---------|------------------------------------------------------------------------------------------------------------------------|
| `pushdown_filters`          | `false` | Evaluate row-level predicates during the parquet decode so failing rows are never materialised. Best for selective filters over large row groups. |
| `reorder_filters`           | `false` | Reorder pushed-down predicates by selectivity. Only effective together with `pushdown_filters`.                        |
| `list_files_cache`          | `false` | Cache object-store file listings so repeated lazy queries reuse `LIST` results — the dominant per-query cost on S3.    |
| `list_files_cache_mb`       | `64`    | Memory budget for the listing cache, in MiB.                                                                           |
| `list_files_cache_ttl_secs` | `60`    | How long a cached listing stays valid (seconds). Bounds how long newly written files take to become visible. `0` = never expires. |

!!! note
    `list_files_cache` does **not** apply to delta sources — their file
    list comes from the transaction log, not an object-store `LIST`. The
    `pushdown_filters` / `reorder_filters` knobs still affect the underlying
    parquet scan for delta. Row-group / page-index / bloom-filter pruning and
    the parquet footer `metadata_size_hint` are already on by DataFusion's
    defaults, so there is nothing to toggle for those.

The Python `DataPressConfig` mirrors these as `datafusion_*` kwargs — see
[Python › Configuration](../python/config.md#datafusion-performance-tuning).

