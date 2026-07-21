//! Actix-web HTTP handlers for the MCP streamable-HTTP transport
//! (MCP 2025-11-25).
//!
//! Transport-level concerns (session header, origin validation, protocol-
//! version header, Accept checking) all live here so `protocol.rs` and
//! `tools.rs` stay free of HTTP types.
//!
//! # Single endpoint, three methods
//!
//! - **POST** `{path}` — receive a JSON-RPC message, dispatch, respond.
//! - **GET** `{path}` → 405 (we do not open an SSE stream).
//! - **DELETE** `{path}` → 200 if `Mcp-Session-Id` present, else 405.
//!
//! # Conformance deviations (noted per spec)
//!
//! - Session id tracking: we generate a fresh session id on every
//!   `initialize` response but do **not** store it. Any subsequent request
//!   is accepted with or without the header. This is permitted by the spec
//!   for stateless servers.
//! - Request id reuse within a session is not tracked (SHOULD in spec).
//! - We always respond with `application/json` (never SSE), which the spec
//!   allows when the server does not offer server-push streams.

use std::sync::Arc;

use actix_web::http::header::{self, HeaderValue};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::Value;

use crate::backend::Backend;
use crate::config::{McpConfig, SqlConfig};

use super::protocol::{
    InitializeParams, PROTOCOL_VERSION, RpcError, RpcMessage, RpcResponse, initialize_result,
    validate_jsonrpc,
};
use super::tools::{ToolSettings, dispatch, tool_list};

// ---------------------------------------------------------------------------
// App-data types for the MCP handler
// ---------------------------------------------------------------------------

/// Runtime settings injected as `web::Data` for the MCP handlers.
#[derive(Debug, Clone)]
pub struct McpSettings {
    pub enabled: bool,
    pub mcp: McpConfig,
    pub sql: SqlConfig,
    pub max_page_size: u64,
    /// Own host string (host[:port]) extracted from the server bind address,
    /// for origin validation.
    pub own_host: String,
}

impl McpSettings {
    /// Return `true` if `origin_header` passes the allow-list.
    ///
    /// Accepts:
    /// - Requests with no `Origin` header (curl, server-to-server).
    /// - Requests whose origin host is `localhost`, `127.0.0.1`, or `[::1]`.
    /// - Requests whose origin matches `own_host`.
    /// - Requests whose origin matches any entry in `mcp.allowed_origins`.
    pub fn origin_allowed(&self, req: &HttpRequest) -> bool {
        let origin = match req.headers().get("Origin") {
            Some(v) => match v.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => return false,
            },
            None => return true, // no Origin header → allow
        };

        // Extract just the host[:port] part from the Origin URL.
        let origin_host = extract_host(&origin);

        // Always allow localhost variants (with or without port).
        let hostname = origin_host.split(':').next().unwrap_or(&origin_host);
        if matches!(hostname, "localhost" | "127.0.0.1" | "[::1]") {
            return true;
        }
        // Allow same host.
        if origin_host == self.own_host {
            return true;
        }
        // Extra allowed origins (full origin URL comparison).
        if self
            .mcp
            .allowed_origins
            .iter()
            .any(|allowed| allowed == &origin || extract_host(allowed) == origin_host)
        {
            return true;
        }
        false
    }
}

/// Extract the `host[:port]` part from an origin URL like
/// `https://example.com:8080`.
fn extract_host(origin: &str) -> String {
    // Strip scheme.
    let s = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or(origin);
    // Strip path.
    s.split('/').next().unwrap_or(s).to_lowercase()
}

// ---------------------------------------------------------------------------
// Route configuration
// ---------------------------------------------------------------------------

/// Register MCP routes on `cfg` at `path`.
pub fn configure(path: &str, settings: web::Data<McpSettings>, cfg: &mut web::ServiceConfig) {
    let path = path.to_string();
    cfg.service(
        web::resource(path)
            .app_data(settings)
            .route(web::post().to(handle_post_with_session))
            .route(web::get().to(handle_get))
            .route(web::delete().to(handle_delete))
            .route(web::method(actix_web::http::Method::OPTIONS).to(handle_options)),
    );
}

// ---------------------------------------------------------------------------
// POST handler
// ---------------------------------------------------------------------------

pub async fn handle_post(
    req: HttpRequest,
    body: web::Bytes,
    settings: web::Data<McpSettings>,
    backend: web::Data<Arc<dyn Backend>>,
) -> HttpResponse {
    // Runtime-disabled check.
    if !settings.enabled {
        return HttpResponse::NotFound().finish();
    }

    // Origin validation (DNS-rebinding guard).
    if !settings.origin_allowed(&req) {
        return HttpResponse::Forbidden()
            .body("Origin not allowed for MCP endpoint");
    }

    // MCP-Protocol-Version header: if present and unsupported, reject.
    if let Some(ver_hdr) = req.headers().get("MCP-Protocol-Version")
        && let Ok(ver) = ver_hdr.to_str()
        && ver != PROTOCOL_VERSION
    {
        return rpc_400(RpcError::invalid_request(format!(
            "unsupported MCP protocol version: {ver}; server supports {PROTOCOL_VERSION}"
        )));
    }

    // Accept header: we only produce `application/json`.
    if let Some(accept) = req.headers().get(header::ACCEPT)
        && let Ok(accept_str) = accept.to_str()
        && !accept_header_allows_json(accept_str)
    {
        return HttpResponse::NotAcceptable()
            .body("only application/json responses are supported");
    }

    // Body size is already limited by the actix PayloadConfig. Parse the body.
    let raw: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            // Top-level array → batch → -32600.
            if body.first() == Some(&b'[') {
                return rpc_json(RpcResponse::err_null_id(RpcError::invalid_request(
                    "JSON-RPC batches are not supported by MCP",
                )));
            }
            return rpc_json(RpcResponse::err_null_id(RpcError::parse_error(
                "could not parse request body as JSON",
            )));
        }
    };
    // Top-level array check (after parse succeeds).
    if raw.is_array() {
        return rpc_json(RpcResponse::err_null_id(RpcError::invalid_request(
            "JSON-RPC batches are not supported by MCP",
        )));
    }
    let msg: RpcMessage = match RpcMessage::from_value(raw) {
        Ok(m) => m,
        Err(_) => {
            return rpc_json(RpcResponse::err_null_id(RpcError::parse_error(
                "could not parse JSON-RPC message",
            )));
        }
    };

    // Validate jsonrpc field first; extract id for error responses.
    let id_val = match validate_jsonrpc(&msg) {
        Ok(_) => match msg.extract_id() {
            Ok(id) => id,
            Err(e) => {
                return rpc_json(RpcResponse::err_null_id(e));
            }
        },
        Err(e) => {
            // Try to get id even from an invalid message.
            let best_id = msg.extract_id().ok().flatten();
            return rpc_json(RpcResponse::err(best_id, e));
        }
    };

    // Notifications: accept, respond 202, no body.
    if msg.is_notification() {
        return HttpResponse::Accepted().finish();
    }

    let id = match id_val {
        Some(id) => id,
        None => {
            // id absent but method is present → this was a notification.
            // Shouldn't reach here but handle defensively.
            return HttpResponse::Accepted().finish();
        }
    };

    let method = match msg.method.as_deref() {
        Some(m) => m,
        None => {
            return rpc_json(RpcResponse::err(
                Some(id),
                RpcError::invalid_request("missing method field"),
            ));
        }
    };

    // Dispatch.
    let result = dispatch_method(method, &msg.params, &req, &settings, &backend).await;
    match result {
        Ok(val) => rpc_json(RpcResponse::ok(Some(id), val)),
        Err(e) => rpc_json(RpcResponse::err(Some(id), e)),
    }
}

// ---------------------------------------------------------------------------
// GET handler
// ---------------------------------------------------------------------------

pub async fn handle_get(settings: web::Data<McpSettings>) -> HttpResponse {
    if !settings.enabled {
        return HttpResponse::NotFound().finish();
    }
    HttpResponse::MethodNotAllowed()
        .insert_header(("Allow", "POST, DELETE"))
        .finish()
}

// ---------------------------------------------------------------------------
// DELETE handler
// ---------------------------------------------------------------------------

pub async fn handle_delete(req: HttpRequest, settings: web::Data<McpSettings>) -> HttpResponse {
    if !settings.enabled {
        return HttpResponse::NotFound().finish();
    }
    if req.headers().contains_key("Mcp-Session-Id") {
        // Session "terminated" — no-op for our stateless server.
        HttpResponse::Ok().finish()
    } else {
        HttpResponse::MethodNotAllowed()
            .insert_header(("Allow", "POST, DELETE"))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// OPTIONS handler (CORS preflight support)
// ---------------------------------------------------------------------------

pub async fn handle_options(settings: web::Data<McpSettings>) -> HttpResponse {
    if !settings.enabled {
        return HttpResponse::NotFound().finish();
    }
    HttpResponse::Ok()
        .insert_header(("Allow", "POST, GET, DELETE, OPTIONS"))
        .finish()
}

// ---------------------------------------------------------------------------
// Method dispatch
// ---------------------------------------------------------------------------

async fn dispatch_method(
    method: &str,
    params: &Value,
    req: &HttpRequest,
    settings: &McpSettings,
    backend: &Arc<dyn Backend>,
) -> Result<Value, RpcError> {
    // Auth enforcement (feature-gated).
    #[cfg(feature = "auth")]
    check_auth(method, req, settings)?;
    #[cfg(not(feature = "auth"))]
    let _ = req; // suppress unused warning

    match method {
        "initialize" => handle_initialize(params),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => handle_tools_list(settings),
        "tools/call" => handle_tools_call(params, backend, settings).await,
        _ => Err(RpcError::method_not_found(method)),
    }
}

fn handle_initialize(params: &Value) -> Result<Value, RpcError> {
    // Accept any string protocol version; echo it if supported, else respond
    // with our version (negotiation, not rejection — per spec).
    let _params: InitializeParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::invalid_params(format!("invalid initialize params: {e}")))?;
    Ok(initialize_result())
}

fn handle_tools_list(settings: &McpSettings) -> Result<Value, RpcError> {
    let tools = tool_list(&settings.mcp, &settings.sql);
    Ok(serde_json::json!({ "tools": tools }))
}

async fn handle_tools_call(
    params: &Value,
    backend: &Arc<dyn Backend>,
    settings: &McpSettings,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("tools/call requires params.name"))?;

    let args = params.get("arguments").unwrap_or(&Value::Null);

    let tool_settings = ToolSettings {
        default_page_size: settings.mcp.page_size.clamp(1, settings.max_page_size),
        max_page_size: settings.max_page_size,
        sql_enabled: settings.sql.enabled,
        expose_sql: settings.mcp.expose_sql,
        sql_max_rows: settings.sql.max_rows,
    };

    dispatch(name, args, backend, &tool_settings).await
}

// ---------------------------------------------------------------------------
// Auth enforcement (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "auth")]
fn check_auth(
    method: &str,
    req: &HttpRequest,
    _settings: &McpSettings,
) -> Result<(), RpcError> {
    use std::sync::Arc;

    let cfg = match req.app_data::<web::Data<Arc<crate::config::AuthConfig>>>() {
        Some(c) => c,
        None => return Ok(()), // auth not configured
    };

    if !cfg.enabled {
        return Ok(());
    }
    if cfg.anonymous_read {
        return Ok(());
    }

    // For lifecycle methods we only require a valid token (no scope check).
    // For tool calls we require read_scopes.
    match method {
        "initialize" | "ping" | "tools/list" => {
            crate::auth::require_scopes(req, &[])
                .map_err(|e| RpcError::internal(format!("auth: {e}")))?;
        }
        _ => {
            crate::auth::require_scopes(req, &cfg.read_scopes)
                .map_err(|e| RpcError::internal(format!("auth: {e}")))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP response helpers
// ---------------------------------------------------------------------------

/// Build a `200 OK` JSON-RPC response.
fn rpc_json(resp: RpcResponse) -> HttpResponse {
    let body = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec());
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body)
}

/// Build a `400 Bad Request` with a JSON-RPC error body (for transport-level
/// failures like an unsupported protocol-version header).
fn rpc_400(error: RpcError) -> HttpResponse {
    let body = serde_json::to_vec(&RpcResponse::err_null_id(error))
        .unwrap_or_else(|_| b"{}".to_vec());
    HttpResponse::BadRequest()
        .content_type("application/json")
        .body(body)
}

/// Check whether the `Accept` header allows `application/json` or `*/*`.
fn accept_header_allows_json(accept: &str) -> bool {
    accept.split(',').any(|part| {
        let trimmed = part.split(';').next().unwrap_or(part).trim();
        trimmed == "application/json"
            || trimmed == "*/*"
            || trimmed == "text/event-stream" // SSE clients also accept our JSON
    })
}

// ---------------------------------------------------------------------------
// Initialize response: generate and inject Mcp-Session-Id
// ---------------------------------------------------------------------------

/// Wrap `handle_post` output so that on `initialize` responses we inject an
/// `Mcp-Session-Id` header. This is handled by post-processing the response
/// in the handler rather than a separate middleware to keep things simple.
pub async fn handle_post_with_session(
    req: HttpRequest,
    body: web::Bytes,
    settings: web::Data<McpSettings>,
    backend: web::Data<Arc<dyn Backend>>,
) -> HttpResponse {
    // We peek at the body to know if it's an `initialize` request.
    let is_initialize = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|m| m == "initialize"))
        .unwrap_or(false);

    let mut resp = handle_post(req, body, settings, backend).await;

    if is_initialize && resp.status().is_success() {
        let session_id = generate_session_id();
        if let Ok(val) = HeaderValue::from_str(&session_id) {
            resp.headers_mut().insert(
                header::HeaderName::from_static("mcp-session-id"),
                val,
            );
        }
    }
    resp
}

fn generate_session_id() -> String {
    // Use a timestamp-based id with a counter for uniqueness without pulling in
    // the `rand` crate. Visible ASCII range 0x21-0x7E per spec.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts:x}-{cnt:04x}")
}

// ---------------------------------------------------------------------------
// OAuth2 protected-resource metadata (RFC 9728) — only with `auth` feature
// ---------------------------------------------------------------------------

/// Settings for the `/.well-known/oauth-protected-resource` endpoint.
/// Only mounted when both `mcp` and `auth` features are on and both are
/// runtime-enabled.
#[cfg(feature = "auth")]
#[derive(Debug, Clone)]
pub struct OAuthProtectedResourceSettings {
    /// The resource URI (typically `https://{host}/`).
    pub resource: String,
    /// OIDC issuer — goes into `authorization_servers`.
    pub issuer: String,
    /// Scopes advertised as supported.
    pub scopes_supported: Vec<String>,
}

#[cfg(feature = "auth")]
pub async fn handle_oauth_protected_resource(
    settings: web::Data<OAuthProtectedResourceSettings>,
) -> HttpResponse {
    let body = serde_json::json!({
        "resource": settings.resource,
        "authorization_servers": [settings.issuer],
        "bearer_methods_supported": ["header"],
        "scopes_supported": settings.scopes_supported,
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .json(body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_header_json_allowed() {
        assert!(accept_header_allows_json("application/json"));
        assert!(accept_header_allows_json("application/json, text/event-stream"));
        assert!(accept_header_allows_json("*/*"));
        assert!(accept_header_allows_json("text/html, */*;q=0.9"));
    }

    #[test]
    fn accept_header_html_only_rejected() {
        assert!(!accept_header_allows_json("text/html"));
        assert!(!accept_header_allows_json("text/plain, text/html"));
    }

    #[test]
    fn extract_host_strips_scheme_and_port() {
        assert_eq!(extract_host("https://example.com:8080/path"), "example.com:8080");
        assert_eq!(extract_host("http://localhost:3000"), "localhost:3000");
        assert_eq!(extract_host("https://example.com"), "example.com");
    }

    #[test]
    fn origin_allowed_no_header() {
        let s = McpSettings {
            enabled: true,
            mcp: McpConfig::default(),
            sql: SqlConfig::default(),
            max_page_size: 1000,
            own_host: "example.com".into(),
        };
        // Build a test request with no Origin header.
        let req = actix_web::test::TestRequest::get().to_http_request();
        assert!(s.origin_allowed(&req));
    }

    #[test]
    fn origin_allowed_localhost() {
        let s = McpSettings {
            enabled: true,
            mcp: McpConfig::default(),
            sql: SqlConfig::default(),
            max_page_size: 1000,
            own_host: "example.com".into(),
        };
        let req = actix_web::test::TestRequest::get()
            .insert_header(("Origin", "http://localhost:3000"))
            .to_http_request();
        assert!(s.origin_allowed(&req));
    }

    #[test]
    fn origin_denied_unknown_host() {
        let s = McpSettings {
            enabled: true,
            mcp: McpConfig::default(),
            sql: SqlConfig::default(),
            max_page_size: 1000,
            own_host: "example.com".into(),
        };
        let req = actix_web::test::TestRequest::get()
            .insert_header(("Origin", "https://evil.example.org"))
            .to_http_request();
        assert!(!s.origin_allowed(&req));
    }

    #[test]
    fn session_id_is_ascii() {
        let id = generate_session_id();
        assert!(id.chars().all(|c| c.is_ascii() && c > ' '));
    }
}
