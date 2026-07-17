//! Swagger UI + embedded OpenAPI specification.
//!
//! Compiled in only when the `swagger` cargo feature is enabled.
//! Builds an [`utoipa::openapi::OpenApi`] by hand from a `serde_json`
//! literal (no per-handler annotations — the curated spec lives here)
//! and hands it to [`utoipa_swagger_ui::SwaggerUi`] for rendering.
//!
//! The UI is mounted at `{prefix}{[swagger].path}` (default `/docs`);
//! the raw spec is exposed at `<mount>/openapi.json` so external tooling
//! (Postman, code generators, …) can consume it directly.

use actix_web::dev::HttpServiceFactory;
use actix_web::{HttpResponse, http::header, web};
use utoipa::openapi::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub use crate::oauth2::{ResolvedOAuth2, resolve_oauth2};

/// Build the [`SwaggerUi`] actix service for the given mount path.
///
/// `mount` is the already-prefixed mount string (e.g. `/dp/docs`).
/// `prefix` is the server prefix (e.g. `/dp`) used to set
/// `servers[0].url` in the OpenAPI spec so "Try it out" targets the
/// right base. Pass `""` when no prefix is configured.
pub fn service(
    mount: &str,
    oauth2: Option<&ResolvedOAuth2>,
    prefix: &str,
) -> impl HttpServiceFactory + use<> {
    let ui = SwaggerUi::new(format!("{mount}/{{_:.*}}"))
        .url(format!("{mount}/openapi.json"), openapi(oauth2, prefix));
    if let Some(o) = oauth2 {
        let oauth_cfg = utoipa_swagger_ui::oauth::Config::new()
            .client_id(&o.client_id)
            .scopes(o.scopes.clone())
            .use_pkce_with_authorization_code_grant(o.pkce);
        ui.oauth(oauth_cfg)
    } else {
        ui
    }
}

/// Register the Swagger UI plus a `mount` → `mount/` redirect.
///
/// Without the redirect, visiting the bare mount path (e.g. `/docs`)
/// 404s because `SwaggerUi`'s tail-capture route requires the trailing
/// slash to match the empty asset path.
///
/// `mount` is the already-prefixed path; `prefix` is threaded into the
/// OpenAPI `servers` entry so "Try it out" resolves against the right base.
pub fn configure(
    mount: &str,
    oauth2: Option<&ResolvedOAuth2>,
    prefix: &str,
    cfg: &mut web::ServiceConfig,
) {
    let redirect_target = format!("{mount}/");
    cfg.service(
        web::resource(mount.to_string()).route(web::get().to(move || {
            let to = redirect_target.clone();
            async move {
                HttpResponse::MovedPermanently()
                    .insert_header((header::LOCATION, to))
                    .finish()
            }
        })),
    )
    .service(service(mount, oauth2, prefix));
}

/// Build the OpenAPI document. The spec is authored as a JSON literal
/// here rather than via `#[utoipa::path]` macros on every handler:
/// the API surface is small and stable, and a hand-written spec gives
/// us full control over examples + descriptions without scattering
/// attributes across the handler tree.
///
/// `prefix` is the configured `server.prefix` (e.g. `"/dp"` or `""`).
/// It is set as `servers[0].url` so Swagger UI's "Try it out" targets
/// the correct base path when the server is mounted behind a prefix.
fn openapi(oauth2: Option<&ResolvedOAuth2>, prefix: &str) -> OpenApi {
    let version = env!("CARGO_PKG_VERSION");
    let server_url = if prefix.is_empty() { "/" } else { prefix };

    // Reusable inline parameter — utoipa doesn't accept `$ref`-style
    // parameters at the Operation level, so we splice the object in
    // wherever it's needed instead.
    let dataset_name_param = serde_json::json!({
        "name":     "name",
        "in":       "path",
        "required": true,
        "schema":   { "type": "string" },
        "description": "Dataset identifier as declared in `datasets.toml`."
    });

    // -----------------------------------------------------------------------
    // Build paths map by merging smaller json! sections.  Each section covers
    // one logical resource group to stay well within the macro recursion limit.
    // -----------------------------------------------------------------------
    let mut paths = serde_json::Map::new();

    // --- probe paths ---
    paths.extend(
        serde_json::json!({
            "/healthz": {
                "get": {
                    "tags":    ["probes"],
                    "summary": "Liveness probe",
                    "description": "Returns 200 once the process is up. Does not touch the backend.",
                    "responses": { "200": { "description": "OK" } }
                }
            },
            "/readyz": {
                "get": {
                    "tags":    ["probes"],
                    "summary": "Readiness probe",
                    "description": "Returns 200 once every dataset has finished loading. Returns 503 while datasets are still warming up.",
                    "responses": {
                        "200": { "description": "Ready" },
                        "503": { "description": "Not ready" }
                    }
                }
            },
            "/version": {
                "get": {
                    "tags":    ["probes"],
                    "summary": "Build / version metadata",
                    "responses": {
                        "200": {
                            "description": "Version info",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/VersionInfo" }
                                }
                            }
                        }
                    }
                }
            }
        })
        .as_object()
        .expect("probe paths")
        .clone(),
    );

    // --- dataset collection: GET /datasets and POST /datasets ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "List registered datasets",
                    "description": "Returns status entries for all configured datasets (including pending, building, and failed states). Each entry includes lifecycle state, kind, residency, refresh observability fields, and `depends_on`.",
                    "responses": {
                        "200": {
                            "description": "Dataset status entries",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "datasets": {
                                                "type":  "array",
                                                "items": { "$ref": "#/components/schemas/DatasetStatusEntry" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                "post": {
                    "tags":    ["admin"],
                    "summary": "Register a new dataset at runtime",
                    "description": "Load a Parquet or Delta source into the running server without a restart. The dataset is held in memory only — call `POST /api/v1/datasets/persist` to also append it to the on-disk config. Requires the configured reload/admin permission.",
                    "security": [ { "AdminToken": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/DatasetConfig" },
                                "example": {
                                    "name":   "events",
                                    "source": { "kind": "parquet", "location": "/data/events/*.parquet" }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Dataset registered",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/DatasetSummary" }
                                }
                            }
                        },
                        "400": { "description": "Invalid config, unreachable source, or a dataset of that name already exists" },
                        "401": { "description": "Missing or invalid admin token" }
                    }
                }
            }
        })
        .as_object()
        .expect("datasets collection paths")
        .clone(),
    );

    // --- dataset collection: persist + reload-all ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/persist": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Append a dataset to the on-disk config",
                    "description": "Append the given dataset's `[[dataset]]` block to the `datasets.toml` this server was loaded from, so a runtime-registered dataset survives a restart. Takes the same body as `POST /api/v1/datasets`. Requires the reload/admin permission and only works when the server was started from a config file.",
                    "security": [ { "AdminToken": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/DatasetConfig" },
                                "example": {
                                    "name":   "events",
                                    "source": { "kind": "parquet", "location": "/data/events/*.parquet" }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Block appended",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "persisted": { "type": "boolean" },
                                            "path":      { "type": "string" }
                                        }
                                    }
                                }
                            }
                        },
                        "400": { "description": "Server has no on-disk config file, or the write failed" },
                        "401": { "description": "Missing or invalid admin token" }
                    }
                }
            },
            "/api/v1/datasets/reload-all": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Rebuild all datasets in topological order (R8.11)",
                    "description": "Enqueue every reloadable dataset as one wave in topological order (dependencies before dependents) and return 202 immediately. Datasets currently building or pending with `lazy`/`skip` on_start are skipped. Requires the reload/admin permission.",
                    "security": [ { "AdminToken": [] } ],
                    "responses": {
                        "202": {
                            "description": "Wave enqueued",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "enqueued": { "type": "array", "items": { "type": "string" } },
                                            "skipped":  { "type": "array", "items": { "type": "string" } }
                                        }
                                    }
                                }
                            }
                        },
                        "401": { "description": "Missing or invalid admin token" }
                    }
                }
            }
        })
        .as_object()
        .expect("persist + reload-all paths")
        .clone(),
    );

    // --- dataset item: schema ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/schema": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "Schema, row count, indexed columns, and sample row",
                    "parameters": [ dataset_name_param ],
                    "responses": {
                        "200": {
                            "description": "Schema response",
                            "content": {
                                "application/json": {
                                    "schema": { "type": "object" }
                                }
                            }
                        },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("schema path")
        .clone(),
    );

    // --- dataset item: status ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/status": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "Full status entry for a single dataset (T5.1)",
                    "description": "Returns a comprehensive status object including lifecycle state (`state`), source kind, residency, refresh observability fields (`last_refresh_at`, `last_refresh_duration_ms`, `next_refresh_at`, `refresh_source`, `consecutive_failures`, `last_error`), row count, column count, storage metadata, and `depends_on`.",
                    "parameters": [ dataset_name_param ],
                    "responses": {
                        "200": {
                            "description": "Dataset status",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/DatasetStatusEntry" }
                                }
                            }
                        },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("status path")
        .clone(),
    );

    // --- dataset item: query ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/query": {
                "post": {
                    "tags":    ["datasets"],
                    "summary": "Run a query against a dataset",
                    "description": "Project, filter, group and sort rows. Set the `Accept` header to `application/vnd.apache.arrow.stream` to receive Arrow IPC instead of JSON.",
                    "parameters": [ dataset_name_param ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/QueryRequest" },
                                "example": {
                                    "columns":    ["state", "severity"],
                                    "predicates": [ { "col": "state", "op": "eq", "val": "CA" } ],
                                    "page":       1,
                                    "page_size":  100
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Query result (JSON or Arrow IPC)",
                            "headers": {
                                "X-Dataset-Refreshed-At": {
                                    "description": "RFC-3339 publish timestamp of the current generation (T5.2).",
                                    "schema": { "type": "string", "format": "date-time" }
                                }
                            },
                            "content": {
                                "application/json": { "schema": { "type": "object" } },
                                "application/vnd.apache.arrow.stream": {
                                    "schema": { "type": "string", "format": "binary" }
                                }
                            }
                        },
                        "400": { "description": "Invalid query" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("query path")
        .clone(),
    );

    // --- dataset item: query/stream ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/query/stream": {
                "post": {
                    "tags":    ["datasets"],
                    "summary": "Stream a full query result as Arrow IPC",
                    "description": "Runs the same query shape as `/query`, but returns one Arrow IPC stream for all matching rows in a single HTTP response. `page` and `page_size` are ignored; optional `limit` caps the total rows returned.",
                    "parameters": [ dataset_name_param ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/QueryRequest" },
                                "example": {
                                    "columns": ["state", "severity"],
                                    "predicates": [ { "col": "state", "op": "eq", "val": "CA" } ],
                                    "limit": 100000
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Arrow IPC stream for the full query result",
                            "content": {
                                "application/vnd.apache.arrow.stream": {
                                    "schema": { "type": "string", "format": "binary" }
                                }
                            }
                        },
                        "400": { "description": "Invalid query" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("query/stream path")
        .clone(),
    );

    // --- dataset item: count ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/count": {
                "post": {
                    "tags":    ["datasets"],
                    "summary": "Count rows matching predicates",
                    "parameters": [ dataset_name_param ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/CountRequest" },
                                "example": {
                                    "predicates": [ { "col": "state", "op": "eq", "val": "CA" } ]
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Row count",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "count": { "type": "integer", "format": "int64" }
                                        }
                                    }
                                }
                            }
                        },
                        "400": { "description": "Invalid request" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("count path")
        .clone(),
    );

    // --- dataset item: parquet export (GET + HEAD, same path key) ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/parquet": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "Download the full dataset as a Parquet file",
                    "description": "Returns the entire dataset serialised as a single Parquet file with HTTP range + `HEAD` support. The file is cached per generation; a subsequent `/reload` invalidates the cache. Disabled for datasets with a column-access `projection_filter`.",
                    "parameters": [ dataset_name_param ],
                    "responses": {
                        "200": {
                            "description": "Parquet file",
                            "content": {
                                "application/octet-stream": { "schema": { "type": "string", "format": "binary" } }
                            }
                        },
                        "403": { "description": "Parquet export disabled for this dataset (projection filter active)" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            },
            "/api/v1/datasets/{name}/all.parquet": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "Download the full dataset as a Parquet file (alternate URL)",
                    "description": "Identical to `GET /api/v1/datasets/{name}/parquet`. This alternate URL is convenient for tools that need a `.parquet` file extension in the path.",
                    "parameters": [ dataset_name_param ],
                    "responses": {
                        "200": {
                            "description": "Parquet file",
                            "content": {
                                "application/octet-stream": { "schema": { "type": "string", "format": "binary" } }
                            }
                        },
                        "403": { "description": "Parquet export disabled for this dataset (projection filter active)" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        })
        .as_object()
        .expect("parquet paths")
        .clone(),
    );

    // --- admin: per-dataset reload + config reload ---
    paths.extend(
        serde_json::json!({
            "/api/v1/datasets/{name}/reload": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Rebuild a dataset from its source",
                    "description": "Requires the configured reload/admin permission. Without OIDC, click the **Authorize** button (\u{1F512}) at the top of this page and enter your token in the **AdminToken** field — DataPress will send it as the `X-Admin-Token` header on every request.",
                    "parameters": [ dataset_name_param ],
                    "security": [ { "AdminToken": [] } ],
                    "responses": {
                        "200": { "description": "Reload succeeded" },
                        "401": { "description": "Missing or invalid admin token" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            },
            "/api/v1/config/reload": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Hot-reload the config and register new datasets",
                    "description": "Re-read the server's on-disk `datasets.toml` and register any `[[dataset]]` added since startup. Existing datasets are left untouched (use `/datasets/{name}/reload` to rebuild one) and server-level settings are not re-applied. Requires the reload/admin permission and only works when the server was started from a config file.",
                    "security": [ { "AdminToken": [] } ],
                    "responses": {
                        "200": {
                            "description": "Reload summary",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "registered": { "type": "array", "items": { "type": "string" } },
                                            "skipped":    { "type": "array", "items": { "type": "string" } },
                                            "errors":     { "type": "array", "items": { "type": "object" } }
                                        }
                                    }
                                }
                            }
                        },
                        "400": { "description": "Server has no on-disk config file, or the file failed to load" },
                        "401": { "description": "Missing or invalid admin token" }
                    }
                }
            }
        })
        .as_object()
        .expect("reload paths")
        .clone(),
    );

    // --- SQL endpoint ---
    paths.extend(
        serde_json::json!({
            "/api/v1/sql": {
                "post": {
                    "tags":    ["datasets"],
                    "summary": "Run a raw read-only SQL query",
                    "description": "Execute a single read-only `SELECT` (or `WITH \u{2026} SELECT`) referencing one or more registered datasets. Disabled unless `[sql].enabled = true`; returns 404 when off. The statement is parsed and validated before execution, and the result is capped at `[sql].max_rows` rows. Send `Accept: application/vnd.apache.arrow.stream` (or `?format=arrow`) to receive an Arrow IPC stream instead of JSON.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema":  { "$ref": "#/components/schemas/SqlRequest" },
                                "example": {
                                    "sql":      "SELECT state, COUNT(*) AS n FROM accidents GROUP BY state ORDER BY n DESC",
                                    "max_rows": 100
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Query result (JSON envelope, or an Arrow IPC stream when negotiated)",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "data":     { "type": "array", "items": { "type": "object" } },
                                            "max_rows": { "type": "integer", "format": "int64" }
                                        }
                                    }
                                },
                                "application/vnd.apache.arrow.stream": {
                                    "schema": { "type": "string", "format": "binary" }
                                }
                            }
                        },
                        "400": { "description": "Statement rejected by the validation gate (not read-only, multiple statements, unknown/file-function table, or more datasets than the server limit)" },
                        "404": { "description": "Endpoint disabled (`[sql].enabled = false`)" }
                    }
                }
            }
        })
        .as_object()
        .expect("sql path")
        .clone(),
    );

    // --- saved-queries endpoints (Phase 6) ---
    paths.extend(
        serde_json::json!({
            "/api/v1/queries": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Register a query as a runtime dataset",
                    "description": "Save a SQL query as a named dataset (`kind = temp` for ephemeral or `kind = query` for persisted). The server parses the SQL, infers `depends_on`, and materialises the result. Use `?async=true` to return 202 immediately with the dataset in `building` state.",
                    "security": [ { "AdminToken": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/CreateQueryRequest" },
                                "example": {
                                    "name": "ca_severe",
                                    "sql":  "SELECT * FROM accidents WHERE state = 'CA' AND severity >= 3",
                                    "kind": "temp",
                                    "ttl":  "2h"
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Dataset registered (sync build)" },
                        "202": { "description": "Dataset queued for background build (`?async=true`)" },
                        "400": { "description": "Invalid SQL, unknown table reference, or name conflict" },
                        "401": { "description": "Missing or invalid admin token" },
                        "409": { "description": "A dataset of that name already exists" }
                    }
                },
                "get": {
                    "tags":    ["admin"],
                    "summary": "List runtime-created datasets",
                    "description": "Returns only datasets created via `POST /api/v1/queries` (managed datasets). Config-file datasets are excluded.",
                    "security": [ { "AdminToken": [] } ],
                    "responses": {
                        "200": {
                            "description": "List of saved query entries",
                            "content": {
                                "application/json": { "schema": { "type": "array", "items": { "type": "object" } } }
                            }
                        },
                        "401": { "description": "Missing or invalid admin token" }
                    }
                }
            },
            "/api/v1/queries/{name}": {
                "delete": {
                    "tags":    ["admin"],
                    "summary": "Delete a runtime-created dataset",
                    "description": "Unregisters and drops a dataset created via `POST /api/v1/queries`. Config-file datasets return 403. Returns 409 when other datasets depend on this one.",
                    "security": [ { "AdminToken": [] } ],
                    "parameters": [
                        {
                            "name":        "name",
                            "in":          "path",
                            "required":    true,
                            "schema":      { "type": "string" },
                            "description": "Dataset name to delete."
                        }
                    ],
                    "responses": {
                        "200": { "description": "Dataset deleted" },
                        "401": { "description": "Missing or invalid admin token" },
                        "403": { "description": "Not a managed dataset (created via config file)" },
                        "404": { "description": "Unknown dataset" },
                        "409": { "description": "Other datasets depend on this one" }
                    }
                }
            }
        })
        .as_object()
        .expect("queries paths")
        .clone(),
    );

    // -----------------------------------------------------------------------
    // Build components/schemas map — each schema as its own json! call.
    // -----------------------------------------------------------------------
    let mut schemas = serde_json::Map::new();

    schemas.insert(
        "VersionInfo".to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "version": { "type": "string" },
                "backend": { "type": "string", "enum": ["DuckDB", "DataFusion"] },
                "storage_backend": {
                    "type":     "string",
                    "enum":     ["local", "s3"],
                    "nullable": true,
                    "description": "Active server-level materialization storage backend. null when [server.storage] is not configured."
                }
            }
        }),
    );

    schemas.insert(
        "DatasetStatusEntry".to_string(),
        serde_json::json!({
            "type": "object",
            "description": "Full per-dataset status including refresh observability fields (T5.1).",
            "properties": {
                "name":                     { "type": "string" },
                "state":                    { "type": "string", "enum": ["pending", "building", "published", "failed"] },
                "kind":                     { "type": "string", "enum": ["parquet", "delta", "query"] },
                "residency":                { "type": "string", "enum": ["memory", "lazy"] },
                "storage_bytes":            { "type": "integer", "nullable": true },
                "generation_id":            { "type": "string", "nullable": true },
                "last_refresh_at":          { "type": "string", "format": "date-time", "nullable": true, "description": "RFC-3339 timestamp of the last successful publish." },
                "last_refresh_duration_ms": { "type": "integer", "nullable": true, "description": "Build duration of the last successful publish in milliseconds." },
                "next_refresh_at":          { "type": "string", "format": "date-time", "nullable": true, "description": "RFC-3339 of the next scheduled refresh fire. null for non-scheduled datasets." },
                "refresh_source":           { "type": "string", "enum": ["startup", "manual", "schedule", "cascade"], "nullable": true },
                "consecutive_failures":     { "type": "integer", "description": "Consecutive scheduler failures since last success. 0 when no scheduler or last tick succeeded." },
                "last_error":               { "type": "string", "nullable": true, "description": "Error message from the last failed build/refresh, truncated to 500 characters." },
                "rows":                     { "type": "integer" },
                "columns":                  { "type": "integer" },
                "lazy":                     { "type": "boolean" },
                "depends_on":               { "type": "array", "items": { "type": "string" }, "description": "Upstream dataset names this dataset depends on (query kind only)." }
            }
        }),
    );

    schemas.insert(
        "DatasetSummary".to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "name":    { "type": "string" },
                "rows":    { "type": "integer", "format": "int64" },
                "columns": { "type": "integer", "format": "int64" }
            }
        }),
    );

    schemas.insert(
        "DatasetConfig".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["name", "source"],
            "description": "Runtime dataset definition, mirroring a `[[dataset]]` block in `datasets.toml`.",
            "properties": {
                "name":   { "type": "string", "description": "Dataset identifier. Alphanumeric plus `_ - .`" },
                "source": {
                    "type":     "object",
                    "required": ["kind", "location"],
                    "properties": {
                        "kind":     { "type": "string", "enum": ["parquet", "delta"] },
                        "location": { "type": "string", "description": "Local path or `s3://bucket/key` URL." }
                    }
                },
                "columns":     { "type": "array", "items": { "type": "string" }, "description": "Optional column projection." },
                "dict_encode": { "type": "boolean" },
                "lazy":        { "type": "boolean", "description": "Stream from source instead of materialising into RAM." },
                "predicate_filter": {
                    "type": "object",
                    "description": "Access control: restrict which columns may be used in filters (`where`/`having`). Set `include` (allowlist) or `exclude` (denylist), never both.",
                    "properties": {
                        "include": { "type": "array", "items": { "type": "string" } },
                        "exclude": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "projection_filter": {
                    "type": "object",
                    "description": "Access control: hide columns from projection, grouping, ordering and the schema everywhere. Set `include` (allowlist) or `exclude` (denylist), never both.",
                    "properties": {
                        "include": { "type": "array", "items": { "type": "string" } },
                        "exclude": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "index": {
                    "type": "object",
                    "properties": {
                        "mode":            { "type": "string", "enum": ["auto", "none", "list"] },
                        "columns":         { "type": "array", "items": { "type": "string" } },
                        "max_cardinality": { "type": "integer", "format": "int64" }
                    }
                },
                "s3": {
                    "type": "object",
                    "description": "S3 / object-store settings for `s3://` locations.",
                    "properties": {
                        "region":           { "type": "string" },
                        "endpoint":         { "type": "string" },
                        "addressing_style": { "type": "string", "enum": ["virtual", "path"] },
                        "allow_http":       { "type": "boolean" },
                        "partitioning":     { "type": "string", "enum": ["auto", "hive", "none"] }
                    }
                }
            }
        }),
    );

    schemas.insert(
        "Predicate".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["col", "op"],
            "description": "Filter clause. `val` is a scalar for eq/neq/cmp/like, an array for `in`, and omitted for `is_null` / `is_not_null`.",
            "properties": {
                "col": { "type": "string" },
                "op":  {
                    "type": "string",
                    "enum": ["eq", "neq", "gt", "gte", "lt", "lte",
                             "like", "ilike", "in", "is_null", "is_not_null"]
                }
            }
        }),
    );

    schemas.insert(
        "OrderBy".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["col"],
            "properties": {
                "col": { "type": "string" },
                "dir": { "type": "string", "enum": ["asc", "desc"] }
            }
        }),
    );

    schemas.insert(
        "Aggregation".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op":    { "type": "string", "enum": ["count", "sum", "avg", "min", "max"] },
                "col":   { "type": "string", "description": "Required for every op except `count`." },
                "alias": { "type": "string" }
            }
        }),
    );

    schemas.insert(
        "QueryRequest".to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "columns":      { "type": "array", "items": { "type": "string" } },
                "predicates":   { "type": "array", "items": { "$ref": "#/components/schemas/Predicate" } },
                "group_by":     { "type": "array", "items": { "type": "string" } },
                "aggregations": { "type": "array", "items": { "$ref": "#/components/schemas/Aggregation" } },
                "distinct":     { "type": "boolean" },
                "order_by":     { "type": "array", "items": { "$ref": "#/components/schemas/OrderBy" } },
                "limit":        { "type": "integer", "format": "int64" },
                "page":         { "type": "integer", "format": "int64", "default": 1 },
                "page_size":    { "type": "integer", "format": "int64", "default": 1000,
                                  "description": "Rows per page. Clamped to [1, server.max_page_size]; default cap is 100,000." }
            }
        }),
    );

    schemas.insert(
        "CountRequest".to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "predicates": { "type": "array", "items": { "$ref": "#/components/schemas/Predicate" } }
            }
        }),
    );

    schemas.insert(
        "SqlRequest".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["sql"],
            "description": "Raw-SQL request. `sql` must be a single read-only SELECT referencing one or more registered datasets.",
            "properties": {
                "sql":      { "type": "string", "description": "The SQL statement to execute." },
                "max_rows": { "type": "integer", "format": "int64",
                              "description": "Optional client row cap. Clamped to [1, sql.max_rows]; never raises the server cap." }
            }
        }),
    );

    schemas.insert(
        "CreateQueryRequest".to_string(),
        serde_json::json!({
            "type": "object",
            "required": ["name", "sql"],
            "description": "Body for `POST /api/v1/queries`.",
            "properties": {
                "name": { "type": "string", "description": "Dataset identifier for the saved query." },
                "sql":  { "type": "string", "description": "Read-only SELECT referencing registered datasets." },
                "kind": { "type": "string", "enum": ["temp", "query"], "default": "temp",
                          "description": "`temp` = ephemeral (lost on restart); `query` = persisted to datasets.d/." },
                "ttl":  { "type": "string", "nullable": true,
                          "description": "Optional expiry duration (e.g. `2h`). Temp-only; absent = lives until restart/delete." },
                "refresh": {
                    "type": "object",
                    "nullable": true,
                    "properties": {
                        "interval":           { "type": "string", "description": "Refresh interval, e.g. `15m`." },
                        "on_upstream_reload": { "type": "boolean", "description": "Cascade rebuild when an upstream dataset publishes." }
                    }
                },
                "materialize": {
                    "type": "object",
                    "nullable": true,
                    "properties": {
                        "residency": { "type": "string", "enum": ["auto", "memory", "lazy"] },
                        "sort_by":   { "type": "array", "items": { "type": "string" } }
                    }
                }
            }
        }),
    );

    // -----------------------------------------------------------------------
    // Assemble the top-level OpenAPI document.
    // -----------------------------------------------------------------------
    let mut json = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title":       "datapress HTTP API",
            "description": "Read-only query layer over Parquet / Delta datasets. \
                            Backed by DataFusion or DuckDB depending on the binary.",
            "version":     version
        },
        "servers": [
            { "url": server_url, "description": "This server" }
        ],
        "tags": [
            { "name": "probes",   "description": "Liveness / readiness / version" },
            { "name": "datasets", "description": "Dataset discovery + querying" },
            { "name": "admin",    "description": "Operator-only mutations" }
        ]
    });
    json["paths"] = serde_json::Value::Object(paths);
    json["components"] = serde_json::json!({
        "securitySchemes": {
            "AdminToken": {
                "type": "apiKey",
                "in":   "header",
                "name": "X-Admin-Token"
            }
        }
    });
    json["components"]["schemas"] = serde_json::Value::Object(schemas);

    // Wire up the OAuth2 security scheme if SSO is configured. The
    // *scheme object* is built with utoipa's typed API and inserted after
    // deserialisation (below) rather than as JSON: utoipa's OAuth2 `Flow`
    // is an untagged enum, so a hand-written `authorizationCode` object
    // round-trips into the `implicit` variant and silently drops
    // `tokenUrl`. Here we only adjust the *requirements* + remove the
    // admin-token scheme, which are plain JSON and safe to splice.
    //
    // We emit an `oauth2` scheme (not `openIdConnect`) because Swagger UI
    // renders the former's authorize/token URLs and scopes straight from
    // the spec, while the latter relies on a client-side discovery fetch
    // that yields an empty Authorize dialog when CORS/reachability blocks
    // it.
    if oauth2.is_some() {
        json["components"]["securitySchemes"]
            .as_object_mut()
            .expect("securitySchemes is an object")
            .remove("AdminToken");

        // Apply globally so every operation shows the lock icon. Scope
        // requirements per operation can be tightened later when the
        // server actually enforces tokens.
        let scopes = serde_json::Value::Array(
            oauth2
                .map(|o| o.scopes.clone())
                .unwrap_or_default()
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
        json["security"] = serde_json::json!([ { "OpenIdConnect": scopes } ]);
        json["paths"]["/api/v1/datasets/{name}/reload"]["post"]["security"] =
            json["security"].clone();
    }

    // The hand-written literal above is type-checked at runtime by
    // `serde`; if a future edit produces invalid OpenAPI, this panics
    // at server start (covered by the integration test below).
    let mut spec: OpenApi =
        serde_json::from_value(json).expect("hand-written OpenAPI spec is well-formed");

    if let Some(o) = oauth2 {
        use utoipa::openapi::security::{AuthorizationCode, Flow, OAuth2, Scopes, SecurityScheme};
        let scopes = Scopes::from_iter(o.scopes.iter().map(|s| (s.clone(), String::new())));
        let flow = Flow::AuthorizationCode(AuthorizationCode::new(
            o.authorization_url.clone(),
            o.token_url.clone(),
            scopes,
        ));
        let scheme = SecurityScheme::OAuth2(OAuth2::with_description(
            [flow],
            "Sign in with your identity provider. The Swagger UI will attach the \
             resulting access token as `Authorization: Bearer \u{2026}` to every \
             \"Try it out\" request.",
        ));
        spec.components
            .as_mut()
            .expect("spec always has components")
            .security_schemes
            .insert("OpenIdConnect".to_string(), scheme);
    }

    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_deserialises() {
        // Smoke test: the spec must be a valid OpenAPI 3 document.
        let _ = openapi(None, "");
    }

    #[test]
    fn openapi_with_oauth2_advertises_oauth2_scheme() {
        let resolved = ResolvedOAuth2 {
            client_id: "dp-swagger".into(),
            authorization_url: "https://issuer.example.com/authorize".into(),
            token_url: "https://issuer.example.com/token".into(),
            scopes: vec!["openid".into(), "datasets:read".into()],
            pkce: true,
        };
        let spec = openapi(Some(&resolved), "");
        let json = serde_json::to_value(&spec).unwrap();
        let scheme = &json["components"]["securitySchemes"]["OpenIdConnect"];
        assert_eq!(scheme["type"], "oauth2");
        assert_eq!(
            scheme["flows"]["authorizationCode"]["authorizationUrl"],
            "https://issuer.example.com/authorize"
        );
        assert_eq!(
            scheme["flows"]["authorizationCode"]["tokenUrl"],
            "https://issuer.example.com/token"
        );
        assert!(
            scheme["flows"]["authorizationCode"]["scopes"]["datasets:read"].is_string(),
            "configured scopes must appear in the authorizationCode flow"
        );
        assert!(json["components"]["securitySchemes"]["AdminToken"].is_null());
        assert!(json["security"][0]["OpenIdConnect"].is_array());
        assert_eq!(
            json["paths"]["/api/v1/datasets/{name}/reload"]["post"]["security"],
            json["security"]
        );
    }

    #[test]
    fn openapi_servers_url_reflects_prefix() {
        let empty = serde_json::to_value(openapi(None, "")).unwrap();
        assert_eq!(empty["servers"][0]["url"], "/");

        let prefixed = serde_json::to_value(openapi(None, "/dp")).unwrap();
        assert_eq!(prefixed["servers"][0]["url"], "/dp");
    }

    /// Verify the spec's path keys match the v1 ROUTES table so drift is
    /// caught at test time rather than silently omitting endpoints.
    ///
    /// The spec is allowed to contain additional paths (probes, parquet
    /// alternates) that are not in the ROUTES introspection table; but every
    /// ROUTES entry MUST appear in the spec as `/api/v1/<suffix>`.
    #[test]
    fn openapi_paths_cover_all_v1_routes() {
        use crate::handlers::v1::ROUTES;

        let spec = serde_json::to_value(openapi(None, "")).unwrap();
        let paths_obj = spec["paths"].as_object().expect("paths is an object");
        let spec_paths: std::collections::HashSet<&str> =
            paths_obj.keys().map(String::as_str).collect();

        // Assert the top-level shape is correct.
        assert_eq!(spec["openapi"], "3.1.0");
        assert!(spec["info"]["version"].is_string());
        assert!(spec["info"]["title"].is_string());

        // Every ROUTES entry (method, suffix) must have its path present.
        let mut missing: Vec<String> = Vec::new();
        for &(_method, suffix) in ROUTES {
            let full_path = format!("/api/v1{suffix}");
            if !spec_paths.contains(full_path.as_str()) {
                missing.push(full_path);
            }
        }
        assert!(
            missing.is_empty(),
            "spec is missing path entries for the following ROUTES:\n  {}",
            missing.join("\n  ")
        );
    }
}
