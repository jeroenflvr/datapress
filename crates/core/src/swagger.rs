//! Swagger UI + embedded OpenAPI specification.
//!
//! Compiled in only when the `swagger` cargo feature is enabled.
//! Builds an [`utoipa::openapi::OpenApi`] by hand from a `serde_json`
//! literal (no per-handler annotations — the curated spec lives here)
//! and hands it to [`utoipa_swagger_ui::SwaggerUi`] for rendering.
//!
//! The UI is mounted at `[swagger].path` (default `/docs`); the raw
//! spec is exposed at `<path>/openapi.json` so external tooling
//! (Postman, code generators, …) can consume it directly.

use actix_web::dev::HttpServiceFactory;
use actix_web::{HttpResponse, http::header, web};
use utoipa::openapi::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::config::SwaggerOAuth2Config;

/// Build the [`SwaggerUi`] actix service for the given mount path.
///
/// Visiting `<mount>/` (e.g. `/docs/`) loads the interactive UI;
/// `<mount>/openapi.json` returns the raw OpenAPI 3.0 document.
///
/// The mount is registered with a tail-capture (`{_:.*}`) so Swagger
/// UI's nested assets resolve correctly.
///
/// When `oauth2` is `Some`, the spec advertises an `OpenIdConnect`
/// security scheme (auto-discovered from the issuer's well-known URL)
/// and the UI's `initOAuth` is preconfigured with `client_id`, scopes,
/// and PKCE so users can sign in directly from the docs page.
pub fn service(
    mount: &str,
    oauth2: Option<&SwaggerOAuth2Config>,
) -> impl HttpServiceFactory + use<> {
    let ui = SwaggerUi::new(format!("{mount}/{{_:.*}}"))
        .url(format!("{mount}/openapi.json"), openapi(oauth2));
    if let Some(o) = oauth2 {
        let mut scopes: Vec<String> = o.scopes.clone();
        if !scopes.iter().any(|s| s == "openid") {
            scopes.insert(0, "openid".to_string());
        }
        let oauth_cfg = utoipa_swagger_ui::oauth::Config::new()
            .client_id(&o.client_id)
            .scopes(scopes)
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
pub fn configure(mount: &str, oauth2: Option<&SwaggerOAuth2Config>, cfg: &mut web::ServiceConfig) {
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
    .service(service(mount, oauth2));
}

/// Build the OpenAPI document. The spec is authored as a JSON literal
/// here rather than via `#[utoipa::path]` macros on every handler:
/// the API surface is small and stable, and a hand-written spec gives
/// us full control over examples + descriptions without scattering
/// attributes across the handler tree.
fn openapi(oauth2: Option<&SwaggerOAuth2Config>) -> OpenApi {
    let version = env!("CARGO_PKG_VERSION");
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
    let mut json = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title":       "datapress HTTP API",
            "description": "Read-only query layer over Parquet / Delta datasets. \
                            Backed by DataFusion or DuckDB depending on the binary.",
            "version":     version,
        },
        "servers": [
            { "url": "/", "description": "This server" }
        ],
        "tags": [
            { "name": "probes",   "description": "Liveness / readiness / version" },
            { "name": "datasets", "description": "Dataset discovery + querying" },
            { "name": "admin",    "description": "Operator-only mutations" }
        ],
        "paths": {
            "/healthz": {
                "get": {
                    "tags":    ["probes"],
                    "summary": "Liveness probe",
                    "description": "Returns 200 once the process is up. Does not touch the backend.",
                    "responses": {
                        "200": { "description": "OK" }
                    }
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
            },
            "/api/v1/datasets": {
                "get": {
                    "tags":    ["datasets"],
                    "summary": "List registered datasets",
                    "responses": {
                        "200": {
                            "description": "Dataset summaries",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "datasets": {
                                                "type":  "array",
                                                "items": { "$ref": "#/components/schemas/DatasetSummary" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
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
            },
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
                                    "predicates": [
                                        { "col": "state", "op": "eq", "val": "CA" }
                                    ],
                                    "page":      1,
                                    "page_size": 100
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Query result (JSON or Arrow IPC)",
                            "content": {
                                "application/json": { "schema": { "type": "object" } },
                                "application/vnd.apache.arrow.stream": { "schema": { "type": "string", "format": "binary" } }
                            }
                        },
                        "400": { "description": "Invalid query" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            },
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
                                    "predicates": [
                                        { "col": "state", "op": "eq", "val": "CA" }
                                    ]
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
                                        "type":       "object",
                                        "properties": { "count": { "type": "integer", "format": "int64" } }
                                    }
                                }
                            }
                        },
                        "400": { "description": "Invalid request" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            },
            "/api/v1/datasets/{name}/reload": {
                "post": {
                    "tags":    ["admin"],
                    "summary": "Rebuild a dataset from its source",
                    "description": "Requires the `X-Admin-Token` header to match the configured admin token.",
                    "parameters": [ dataset_name_param ],
                    "security": [ { "AdminToken": [] } ],
                    "responses": {
                        "200": { "description": "Reload succeeded" },
                        "401": { "description": "Missing or invalid admin token" },
                        "404": { "description": "Unknown dataset" }
                    }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "AdminToken": {
                    "type": "apiKey",
                    "in":   "header",
                    "name": "X-Admin-Token"
                }
            },
            "schemas": {
                "VersionInfo": {
                    "type": "object",
                    "properties": {
                        "version": { "type": "string" },
                        "backend": { "type": "string", "enum": ["DuckDB", "DataFusion"] }
                    }
                },
                "DatasetSummary": {
                    "type": "object",
                    "properties": {
                        "name":     { "type": "string" },
                        "rows":     { "type": "integer", "format": "int64" },
                        "columns":  { "type": "integer", "format": "int64" }
                    }
                },
                "Predicate": {
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
                },
                "OrderBy": {
                    "type": "object",
                    "required": ["col"],
                    "properties": {
                        "col": { "type": "string" },
                        "dir": { "type": "string", "enum": ["asc", "desc"] }
                    }
                },
                "Aggregation": {
                    "type": "object",
                    "required": ["op"],
                    "properties": {
                        "op":    { "type": "string", "enum": ["count", "sum", "avg", "min", "max"] },
                        "col":   { "type": "string", "description": "Required for every op except `count`." },
                        "alias": { "type": "string" }
                    }
                },
                "QueryRequest": {
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
                        "page_size":    { "type": "integer", "format": "int64", "default": 1000, "description": "Rows per page. Clamped to [1, server.max_page_size]; default cap is 1,000,000." }
                    }
                },
                "CountRequest": {
                    "type": "object",
                    "properties": {
                        "predicates": { "type": "array", "items": { "$ref": "#/components/schemas/Predicate" } }
                    }
                }
            }
        }
    });

    // Splice in the OIDC security scheme if SSO is configured. Done
    // outside the json! literal so the macro stays static and the
    // optional bits live in plain Rust.
    if let Some(o) = oauth2 {
        let scheme = serde_json::json!({
            "type":             "openIdConnect",
            "openIdConnectUrl": format!("{}/.well-known/openid-configuration", o.issuer),
            "description":      "Sign in with your identity provider. The Swagger UI \
                                 will attach the resulting access token as \
                                 `Authorization: Bearer …` to every \"Try it out\" \
                                 request.",
        });
        json["components"]["securitySchemes"]["OpenIdConnect"] = scheme;
        // Apply globally so every operation shows the lock icon. Scope
        // requirements per operation can be tightened later when the
        // server actually enforces tokens.
        let scopes = serde_json::Value::Array(
            o.scopes
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        );
        json["security"] = serde_json::json!([ { "OpenIdConnect": scopes } ]);
    }

    // The hand-written literal above is type-checked at runtime by
    // `serde`; if a future edit produces invalid OpenAPI, this panics
    // at server start (covered by the integration test below).
    serde_json::from_value(json).expect("hand-written OpenAPI spec is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_deserialises() {
        // Smoke test: the spec must be a valid OpenAPI 3 document.
        let _ = openapi(None);
    }

    #[test]
    fn openapi_with_oauth2_advertises_openid_connect_scheme() {
        let cfg = SwaggerOAuth2Config {
            issuer: "https://issuer.example.com".into(),
            client_id: "dp-swagger".into(),
            scopes: vec!["openid".into(), "datasets:read".into()],
            pkce: true,
        };
        let spec = openapi(Some(&cfg));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["components"]["securitySchemes"]["OpenIdConnect"]["type"],
            "openIdConnect"
        );
        assert_eq!(
            json["components"]["securitySchemes"]["OpenIdConnect"]["openIdConnectUrl"],
            "https://issuer.example.com/.well-known/openid-configuration"
        );
        assert!(json["security"][0]["OpenIdConnect"].is_array());
    }
}
