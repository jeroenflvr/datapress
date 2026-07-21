# Docs-reconciliation fact table

> **Purpose.** This file is the single source of truth used to reconcile
> `README.md`, the `docs/` MkDocs source, and the deployed site
> (`https://docs.datap-rs.org`) with the actual behavior of the code on `main`.
> Every claim below is anchored to a `file:line` citation. When docs disagree
> with this table, the docs are wrong — fix the docs, not the code.
>
> Ordering of authority: **code on `main` > `CHANGELOG.md` > docs site > README**.
>
> Regenerate / re-verify when `crates/core/src/config.rs`,
> `crates/core/src/server.rs`, `crates/core/src/handlers/**`, or the per-binary
> `main.rs` entrypoints change.

## 1. Config file resolution

| Claim | Code location | Value |
| --- | --- | --- |
| Unified `datapress` binary config env var | [crates/datapress/src/main.rs:14](../../crates/datapress/src/main.rs#L14) | `DATAPRESS_CONFIG_FILE` |
| Unified binary resolution order (first match wins) | [crates/datapress/src/main.rs:80-98](../../crates/datapress/src/main.rs#L80-L98) | `--config <FILE>` → `$DATAPRESS_CONFIG_FILE` → `./datasets.toml` → `$HOME/datasets.toml` |
| Per-backend `datapress-datafusion` binary config env var | [crates/datafusion/src/bin/datapress-datafusion.rs:8](../../crates/datafusion/src/bin/datapress-datafusion.rs#L8) | `DATASETS_CONFIG`, default `datasets.toml` |
| Per-backend `datapress-duckdb` binary config env var | [crates/duckdb/src/bin/datapress-duckdb.rs:8](../../crates/duckdb/src/bin/datapress-duckdb.rs#L8) | `DATASETS_CONFIG`, default `datasets.toml` |

**Resolution:** `DATAPRESS_CONFIG_FILE` and `DATASETS_CONFIG` are **not** aliases.
They apply to *different binaries*: the unified `datapress` binary reads
`DATAPRESS_CONFIG_FILE`; the standalone `datapress-datafusion` /
`datapress-duckdb` binaries read `DATASETS_CONFIG`. Docs that tell a unified-binary
reader to set `DATASETS_CONFIG` (or vice versa) are wrong.

## 2. Backend selection

| Claim | Code location | Value |
| --- | --- | --- |
| Unified binary dispatches on `server.backend` at runtime | [crates/datapress/src/main.rs:63-71](../../crates/datapress/src/main.rs#L63-L71) | `Backend::Duckdb → serve_duckdb`, `Backend::Datafusion → serve_datafusion` |
| Default backend | [crates/core/src/config.rs:274-282](../../crates/core/src/config.rs#L274-L282) (enum `#[default]` at L1180) | `Backend::Datafusion` |
| `datapress-datafusion` binary ignores mismatched `server.backend` (warns, runs datafusion) | [crates/datafusion/src/bin/datapress-datafusion.rs:11-17](../../crates/datafusion/src/bin/datapress-datafusion.rs#L11-L17) | warn-only |
| `datapress-duckdb` binary ignores mismatched `server.backend` (warns, runs duckdb) | [crates/duckdb/src/bin/datapress-duckdb.rs:11-17](../../crates/duckdb/src/bin/datapress-duckdb.rs#L11-L17) | warn-only |

**Resolution:** `server.backend` is authoritative **only for the unified
`datapress` binary**. For the single-backend binaries it is an informational hint —
a mismatch logs a WARN and the binary runs its compiled-in engine anyway.

## 3. Prefix and probe composition (contested)

| Claim | Code location | Value |
| --- | --- | --- |
| Probes are mounted **inside** `web::scope(prefix)` | [crates/core/src/server.rs:501-509](../../crates/core/src/server.rs#L501-L509) | `web::scope(prefix.as_str()).service(healthz).service(readyz).service(version).service(health)` |
| API mount is inside the same prefix scope | [crates/core/src/server.rs:508](../../crates/core/src/server.rs#L508) | `web::scope("/api/v1")` nested in `web::scope(prefix)` |
| Docs / Swagger / Explorer are registered **before** the prefix scope, at `{prefix}{path}` mount strings | [crates/core/src/server.rs:479-499](../../crates/core/src/server.rs#L479-L499) | mounted at precomputed `{prefix}{path}` |
| Default prefix | [crates/core/src/config.rs:281](../../crates/core/src/config.rs#L281) | `""` (empty) |

**Resolution (WINNER: `configuration/server` / reverse-proxy doc; LOSER: README + `reference/endpoints`):**
Health/readiness/version probes are served **under** the configured `server.prefix`
(e.g. with `prefix = "/dp"` they live at `/dp/healthz`, `/dp/readyz`, `/dp/version`,
`/dp/health`). Any doc claiming probes bypass / sit outside the prefix is wrong.

## 4. Glob support (contested)

| Claim | Code location | Value |
| --- | --- | --- |
| DataFusion local + S3 accept globs via `ListingTable` | [crates/datafusion/src/store.rs:29](../../crates/datafusion/src/store.rs#L29); test [crates/datafusion/tests/end_to_end.rs:259-263](../../crates/datafusion/tests/end_to_end.rs#L259-L263) | `city=*/*.parquet` unions files |
| DuckDB passes local globs straight to `read_parquet()` | [crates/duckdb/src/db.rs:762-789](../../crates/duckdb/src/db.rs#L762-L789) | glob / explicit list passed through |
| DuckDB auto-appends `**/*.parquet` to plain S3 prefixes | [crates/core/src/config.rs:2367-2376](../../crates/core/src/config.rs#L2367-L2376); test L3287-L3310 | `s3://bucket/logs/` → `s3://bucket/logs/**/*.parquet`; existing globs unchanged |

**Resolution:** Both backends support glob patterns (`*`, `?`, `[…]`) for local
paths and S3 prefixes. Docs stating "No glob patterns" (e.g.
`configuration/datasets.md`) are wrong for glob-bearing locations. Plain
directory locations still expand to their contained `*.parquet` files.

## 5. Residency / lazy / memory model

| Claim | Code location | Value |
| --- | --- | --- |
| `MaterializeResidency` variants | [crates/core/src/config.rs:503-514](../../crates/core/src/config.rs#L503-L514) | `auto` (default), `memory`, `lazy` |
| Server-level force-lazy threshold | [crates/core/src/config.rs:279-281](../../crates/core/src/config.rs#L279-L281) | `force_lazy_above_mb`, `0` disables |
| `auto` residency degrades to memory when no storage configured (WARN) | [crates/datafusion/src/store.rs:1951-2005](../../crates/datafusion/src/store.rs#L1951-L2005) | `effective_residency()` |
| DataFusion eager datasets held as Arrow `RecordBatch`es in RAM; lazy uses `ListingTable` | [crates/datafusion/src/store.rs:78-99](../../crates/datafusion/src/store.rs#L78-L99) | eager = resident, lazy = streamed |
| DuckDB `force_lazy_above_mb` sizes local sources only; S3 needs explicit `lazy = true` | [crates/core/src/config.rs:249-255](../../crates/core/src/config.rs#L249-L255) | backend-specific sizing |
| Dataset lifecycle states | [crates/core/src/backend.rs:218-228](../../crates/core/src/backend.rs#L218-L228) | `pending`, `building`, `published`, `failed` |

**Resolution:** "Whole dataset resident in RAM" is only true for **eager**
DataFusion datasets. Lazy datasets (explicit `lazy = true`, `residency = "lazy"`,
or auto-demoted above `force_lazy_above_mb`) stream from a `ListingTable` and are
not fully resident. Docs asserting unconditional in-RAM residency need the eager/lazy
qualifier.

## 6. Feature/section enable defaults (contested: `[docs]`)

| Block | Field | Default | Code location |
| --- | --- | --- | --- |
| `[docs]` | `enabled` | **`true`** | [crates/core/src/config.rs:852](../../crates/core/src/config.rs#L852) |
| `[docs]` | `path` | `"/mkdocs"` | [crates/core/src/config.rs:853](../../crates/core/src/config.rs#L853) |
| `[swagger]` | `enabled` / `path` | `true` / `"/docs"` | [crates/core/src/config.rs:877-878](../../crates/core/src/config.rs#L877-L878) |
| `[explorer]` | `enabled` / `path` | `true` / `"/explore"` | [crates/core/src/config.rs:1018-1019](../../crates/core/src/config.rs#L1018-L1019) |
| `[metrics]` | `enabled` / `path` | `false` / `"/metrics"` | [crates/core/src/config.rs:927-928](../../crates/core/src/config.rs#L927-L928) |
| `[sql]` | `enabled` / `max_rows` | `false` / `100000` | [crates/core/src/config.rs:1060-1061](../../crates/core/src/config.rs#L1060-L1061) |

**Resolution:** `[docs].enabled` defaults to **`true`**. README is correct; any doc
page (e.g. `configuration/server` / `configuration/docs-site`) claiming the docs
site is off by default is wrong.

## 7. `[server]` defaults

| Field | Default | Code location |
| --- | --- | --- |
| `backend` | `datafusion` | [config.rs:274-282](../../crates/core/src/config.rs#L274-L282) |
| `listen` | `127.0.0.1` | [config.rs:278](../../crates/core/src/config.rs#L278) |
| `port` | `8080` | [config.rs:279](../../crates/core/src/config.rs#L279) |
| `workers` | `None` (actix default) | [config.rs:280](../../crates/core/src/config.rs#L280) |
| `prefix` | `""` | [config.rs:281](../../crates/core/src/config.rs#L281) |
| `compress` | `true` | [config.rs:282](../../crates/core/src/config.rs#L282) |
| `max_body_bytes` | `1048576` (1 MiB) | [config.rs:283](../../crates/core/src/config.rs#L283) |
| `max_page_size` | `100000` | [config.rs:284](../../crates/core/src/config.rs#L284) |
| `force_lazy_above_mb` | `0` | [config.rs:285](../../crates/core/src/config.rs#L285) |
| `request_timeout_ms` | `30000` | [config.rs:286](../../crates/core/src/config.rs#L286) |
| `shutdown_timeout_secs` | `30` | [config.rs:287](../../crates/core/src/config.rs#L287) |
| `environment` | `None` | [config.rs:290](../../crates/core/src/config.rs#L290) |
| `environment_color` | `None` | [config.rs:291](../../crates/core/src/config.rs#L291) |
| `saved_queries_dir` | `None` | [config.rs:295](../../crates/core/src/config.rs#L295) |

### `[server.startup]` / `[server.refresh]`

| Field | Default | Code location |
| --- | --- | --- |
| `startup.max_concurrent` | `4` | [config.rs:641](../../crates/core/src/config.rs#L641) |
| `startup.readiness` | `all` (`all` \| `any`) | [config.rs:642](../../crates/core/src/config.rs#L642) |
| `refresh.max_concurrent` | `1` | [config.rs:659](../../crates/core/src/config.rs#L659) |

### `[server.storage]` (optional; `None` unless present)

| Field | Default | Code location |
| --- | --- | --- |
| `backend` | `local` (`local` \| `s3`) | [config.rs:482](../../crates/core/src/config.rs#L482) |
| `root` | `""` | [config.rs:483](../../crates/core/src/config.rs#L483) |
| `force_lazy_above_mb` | `512` | [config.rs:485,519](../../crates/core/src/config.rs#L485) |
| `materialization_memory_mb` | `None` | [config.rs:502](../../crates/core/src/config.rs#L502) |
| `materialization_sort_spill_reservation_mb` | `None` | [config.rs:514](../../crates/core/src/config.rs#L514) |

### `[server.quack]`

| Field | Default | Code location |
| --- | --- | --- |
| `enabled` | `false` | [config.rs:698](../../crates/core/src/config.rs#L698) |
| `uri` | `"quack:localhost"` (port 9494 implicit) | [config.rs:699](../../crates/core/src/config.rs#L699) |
| `token` | `None` | [config.rs:700](../../crates/core/src/config.rs#L700) |
| `allow_other_hostname` | `false` | [config.rs:701](../../crates/core/src/config.rs#L701) |
| `read_only` | `true` | [config.rs:702](../../crates/core/src/config.rs#L702) |

### `[server.pgwire]`

| Field | Default | Code location |
| --- | --- | --- |
| `enabled` | `false` | [config.rs:781](../../crates/core/src/config.rs#L781) |
| `listen` | `127.0.0.1` | [config.rs:782](../../crates/core/src/config.rs#L782) |
| `port` | `5432` | [config.rs:783](../../crates/core/src/config.rs#L783) |
| `username` | `"datapress"` | [config.rs:784](../../crates/core/src/config.rs#L784) |
| `password` | `None` | [config.rs:785](../../crates/core/src/config.rs#L785) |
| `tls_cert` / `tls_key` | `None` / `None` | [config.rs:786-787](../../crates/core/src/config.rs#L786-L787) |

## 8. `[auth]` defaults

| Field | Default | Code location |
| --- | --- | --- |
| `enabled` | `false` | [config.rs:1166](../../crates/core/src/config.rs#L1166) |
| `issuer` / `audience` | `""` / `""` | [config.rs:1167-1168](../../crates/core/src/config.rs#L1167-L1168) |
| `read_scopes` / `reload_scopes` | `[]` / `[]` | [config.rs:1169-1170](../../crates/core/src/config.rs#L1169-L1170) |
| `manage_scopes` | `["datasets:manage"]` | [config.rs:1171](../../crates/core/src/config.rs#L1171) |
| `anonymous_read` | `false` | [config.rs:1172](../../crates/core/src/config.rs#L1172) |
| `start_degraded` | `true` | [config.rs:1173](../../crates/core/src/config.rs#L1173) |
| `algorithms` | `["RS256"]` | [config.rs:1174](../../crates/core/src/config.rs#L1174) |
| `leeway_secs` | `60` | [config.rs:1175](../../crates/core/src/config.rs#L1175) |
| `jwks_refresh_secs` | `3600` | [config.rs:1176](../../crates/core/src/config.rs#L1176) |
| `tenant_claim` | `""` | [config.rs:1177](../../crates/core/src/config.rs#L1177) |
| `allowed_tenants` | `[]` | [config.rs:1178](../../crates/core/src/config.rs#L1178) |
| `admin_token_fallback` | `true` | [config.rs:1179](../../crates/core/src/config.rs#L1179) |

## 9. `[datafusion]` defaults

| Field | Default | Code location |
| --- | --- | --- |
| `pushdown_filters` | `false` | [config.rs:1102](../../crates/core/src/config.rs#L1102) |
| `reorder_filters` | `false` | [config.rs:1103](../../crates/core/src/config.rs#L1103) |
| `list_files_cache` | `false` | [config.rs:1104](../../crates/core/src/config.rs#L1104) |
| `list_files_cache_mb` | `64` | [config.rs:1105](../../crates/core/src/config.rs#L1105) |
| `list_files_cache_ttl_secs` | `60` | [config.rs:1106](../../crates/core/src/config.rs#L1106) |

## 10. `[[dataset]]` defaults

| Field | Default | Code location |
| --- | --- | --- |
| `dict_encode` | `true` | [config.rs:1259](../../crates/core/src/config.rs#L1259) |
| `lazy` | `false` | [config.rs:1265](../../crates/core/src/config.rs#L1265) |
| `on_start` | `eager` (`eager` \| `lazy` \| `skip`) | [config.rs:1287](../../crates/core/src/config.rs#L1287) |
| `refresh` | `None` | [config.rs:1291](../../crates/core/src/config.rs#L1291) |
| `materialize` | `None` | [config.rs:1297](../../crates/core/src/config.rs#L1297) |
| `managed` | `false` | [config.rs:1305](../../crates/core/src/config.rs#L1305) |
| `temp` | `false` | [config.rs:1309](../../crates/core/src/config.rs#L1309) |
| `source.kind` | `parquet` (`parquet` \| `delta` \| `query`) | [config.rs:1511](../../crates/core/src/config.rs#L1511) |
| `source.depends_on` | `[]` | [config.rs:1514](../../crates/core/src/config.rs#L1514) |
| `index.mode` | `auto` (`auto` \| `none` \| `list`) | [config.rs:1550](../../crates/core/src/config.rs#L1550) |
| `index.max_cardinality` | `100000` | [config.rs:1552](../../crates/core/src/config.rs#L1552) |
| `refresh.timeout` | `600s` | [config.rs:1635,1649](../../crates/core/src/config.rs#L1635) |
| `refresh.jitter` | `true` | [config.rs:1640](../../crates/core/src/config.rs#L1640) |
| `refresh.debounce` | `5s` | [config.rs:1645,1653](../../crates/core/src/config.rs#L1645) |
| `refresh.on_upstream_reload` | `false` | [config.rs:1631](../../crates/core/src/config.rs#L1631) |
| `materialize.residency` | `auto` (`auto` \| `memory` \| `lazy`) | [config.rs:1547](../../crates/core/src/config.rs#L1547) |
| `materialize.sort_by` | `[]` | [config.rs:1551](../../crates/core/src/config.rs#L1551) |
| `materialize.reuse_on_start` | `false` | [config.rs:1555](../../crates/core/src/config.rs#L1555) |

## 11. HTTP routes

Base API mount: `/api/v1` (nested inside `server.prefix`).

| Method(s) | Path | Gate | Code location |
| --- | --- | --- | --- |
| GET | `/api/v1/datasets` | — | [handlers/v1.rs](../../crates/core/src/handlers/v1.rs) |
| POST | `/api/v1/datasets` | `datasets:manage` | handlers/v1.rs |
| POST | `/api/v1/datasets/persist` | `datasets:manage` | handlers/v1.rs |
| GET | `/api/v1/datasets/{name}/schema` | — | handlers/v1.rs |
| GET | `/api/v1/datasets/{name}/status` | — | handlers/v1.rs |
| POST | `/api/v1/datasets/{name}/query` | — | handlers/v1.rs |
| POST | `/api/v1/datasets/{name}/query/stream` | — | [handlers/v1.rs:120-123](../../crates/core/src/handlers/v1.rs#L120-L123) |
| POST | `/api/v1/datasets/{name}/count` | — | handlers/v1.rs |
| GET, HEAD | `/api/v1/datasets/{name}/parquet` | — | [handlers/v1.rs:125-133](../../crates/core/src/handlers/v1.rs#L125-L133) |
| GET, HEAD | `/api/v1/datasets/{name}/all.parquet` | — | handlers/v1.rs |
| POST | `/api/v1/sql` | `[sql].enabled` | [handlers/v1.rs:119](../../crates/core/src/handlers/v1.rs#L119) |
| POST | `/api/v1/datasets/{name}/reload` | reload scope | [handlers/v1.rs:136](../../crates/core/src/handlers/v1.rs#L136) |
| POST | `/api/v1/datasets/reload-all` | reload scope | handlers/v1.rs |
| POST | `/api/v1/config/reload` | reload scope | handlers/v1.rs |
| POST, GET | `/api/v1/queries` | `datasets:manage` | [handlers/v1.rs:139-141](../../crates/core/src/handlers/v1.rs#L139-L141) |
| DELETE | `/api/v1/queries/{name}` | `datasets:manage` | handlers/v1.rs |
| GET | `/healthz` `/readyz` `/version` `/health` | under `prefix` | [handlers/mod.rs:106-253](../../crates/core/src/handlers/mod.rs#L106-L253) |
| GET | `{prefix}{docs.path}` (default `/mkdocs`) | `docs` feature + `[docs].enabled` | [server.rs:479-483](../../crates/core/src/server.rs#L479-L483) |
| GET | `{prefix}{swagger.path}` (default `/docs`) | `swagger` feature + `[swagger].enabled` | [server.rs:485-492](../../crates/core/src/server.rs#L485-L492) |
| GET | `{prefix}{explorer.path}` (default `/explore`) | `explorer` feature + `[explorer].enabled` | [server.rs:494-499](../../crates/core/src/server.rs#L494-L499) |
| GET | `{prefix}{metrics.path}` (default `/metrics`) | `metrics` feature + `[metrics].enabled` | server.rs |

> When documenting the route surface, describe it by capability, not a fixed
> count — the number changes as feature-gated routes are added.
