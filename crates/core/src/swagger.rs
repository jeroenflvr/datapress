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

/// OIDC endpoints + UI parameters resolved from a [`SwaggerOAuth2Config`]
/// at server startup.
///
/// We emit an OpenAPI `oauth2` security scheme with an explicit
/// `authorizationCode` flow (authorize + token URLs) rather than a bare
/// `openIdConnect` scheme. Swagger UI renders an `oauth2` flow directly
/// from the spec — scope checkboxes, the Authorize button, PKCE — whereas
/// an `openIdConnect` scheme forces the browser to fetch the issuer's
/// discovery document client-side, which silently yields an *empty*
/// Authorize dialog when that fetch is blocked by CORS or unreachable.
///
/// The authorize/token URLs are discovered once at boot via
/// [`resolve_oauth2`]; the caller falls back to skipping the UI login
/// (no Authorize button) if discovery fails, rather than shipping a
/// broken dialog.
#[derive(Debug, Clone)]
pub struct ResolvedOAuth2 {
    /// Public OAuth2 client id registered for the Swagger UI.
    pub client_id: String,
    /// `authorization_endpoint` from the issuer's OIDC metadata.
    pub authorization_url: String,
    /// `token_endpoint` from the issuer's OIDC metadata.
    pub token_url: String,
    /// Scopes offered in the Authorize dialog (`openid` always included).
    pub scopes: Vec<String>,
    /// Whether to drive the authorization-code flow with PKCE.
    pub pkce: bool,
}

/// Run OIDC discovery for the Swagger UI's login flow: GET
/// `{issuer}/.well-known/openid-configuration` and pull out the
/// `authorization_endpoint` and `token_endpoint`. Scopes come from the
/// operator's config (with `openid` ensured); the issuer's
/// `scopes_supported` is only used as a fallback when none are
/// configured.
///
/// Returns `Err` on a network failure, a non-success HTTP status, or a
/// metadata body that lacks either endpoint. The issuer's trailing
/// slash (if any) is trimmed so the well-known URL never doubles up.
pub async fn resolve_oauth2(cfg: &SwaggerOAuth2Config) -> Result<ResolvedOAuth2, String> {
    #[derive(serde::Deserialize)]
    struct OidcMetadata {
        authorization_endpoint: Option<String>,
        token_endpoint: Option<String>,
        #[serde(default)]
        scopes_supported: Vec<String>,
    }

    let disco_url = format!(
        "{}/.well-known/openid-configuration",
        cfg.issuer.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
    let resp = client
        .get(&disco_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("discovery {disco_url} → HTTP {}", resp.status()));
    }
    let meta = resp
        .json::<OidcMetadata>()
        .await
        .map_err(|e| format!("discovery {disco_url} body: {e}"))?;
    let authorization_url = meta
        .authorization_endpoint
        .ok_or_else(|| format!("discovery {disco_url}: missing authorization_endpoint"))?;
    let token_url = meta
        .token_endpoint
        .ok_or_else(|| format!("discovery {disco_url}: missing token_endpoint"))?;

    let mut scopes = if cfg.scopes.is_empty() {
        meta.scopes_supported
    } else {
        cfg.scopes.clone()
    };
    if !scopes.iter().any(|s| s == "openid") {
        scopes.insert(0, "openid".to_string());
    }

    log::info!(
        "swagger: OIDC discovery ok (authorize={authorization_url}, token={token_url})"
    );
    Ok(ResolvedOAuth2 {
        client_id: cfg.client_id.clone(),
        authorization_url,
        token_url,
        scopes,
        pkce: cfg.pkce,
    })
}

/// Build the [`SwaggerUi`] actix service for the given mount path.
///
/// Visiting `<mount>/` (e.g. `/docs/`) loads the interactive UI;
/// `<mount>/openapi.json` returns the raw OpenAPI 3.0 document.
///
/// The mount is registered with a tail-capture (`{_:.*}`) so Swagger
/// UI's nested assets resolve correctly.
///
/// When `oauth2` is `Some`, the spec advertises an `oauth2`
/// security scheme (`authorizationCode` flow with the issuer's
/// discovered authorize/token endpoints) and the UI's `initOAuth` is
/// preconfigured with `client_id`, scopes, and PKCE so users can sign
/// in directly from the docs page.
pub fn service(
    mount: &str,
    oauth2: Option<&ResolvedOAuth2>,
) -> impl HttpServiceFactory + use<> {
    let ui = SwaggerUi::new(format!("{mount}/{{_:.*}}"))
        .url(format!("{mount}/openapi.json"), openapi(oauth2));
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
pub fn configure(mount: &str, oauth2: Option<&ResolvedOAuth2>, cfg: &mut web::ServiceConfig) {
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
fn openapi(oauth2: Option<&ResolvedOAuth2>) -> OpenApi {
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
                                    "columns":    ["state", "severity"],
                                    "predicates": [
                                        { "col": "state", "op": "eq", "val": "CA" }
                                    ],
                                    "limit": 100000
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Arrow IPC stream for the full query result",
                            "content": {
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
                    "description": "Requires the configured reload/admin permission. Without OIDC, pass the configured `X-Admin-Token` header.",
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
                        "page_size":    { "type": "integer", "format": "int64", "default": 1000, "description": "Rows per page. Clamped to [1, server.max_page_size]; default cap is 100,000." }
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
        use utoipa::openapi::security::{
            AuthorizationCode, Flow, OAuth2, Scopes, SecurityScheme,
        };
        let scopes =
            Scopes::from_iter(o.scopes.iter().map(|s| (s.clone(), String::new())));
        let flow = Flow::AuthorizationCode(AuthorizationCode::new(
            o.authorization_url.clone(),
            o.token_url.clone(),
            scopes,
        ));
        let scheme = SecurityScheme::OAuth2(OAuth2::with_description(
            [flow],
            "Sign in with your identity provider. The Swagger UI will attach the \
             resulting access token as `Authorization: Bearer …` to every \
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
        let _ = openapi(None);
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
        let spec = openapi(Some(&resolved));
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
}
