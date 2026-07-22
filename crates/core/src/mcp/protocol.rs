//! JSON-RPC 2.0 types for the MCP transport layer.
//!
//! Only the MCP 2025-11-25 revision is implemented here.  Version
//! negotiation, session handling, and protocol-version header parsing are
//! intentionally isolated in this file (and `http.rs`) so a future
//! 2026-07-28 revision can be added as a second accepted version without
//! touching `tools.rs`.
//!
//! # JSON-RPC rules enforced at deserialisation time
//! - `"jsonrpc"` field must equal `"2.0"` (else `-32600`).
//! - `id` must be a string, number, or **absent** (notification). A JSON
//!   `null` id is rejected with `-32600` per the MCP spec (stricter than
//!   base JSON-RPC).
//! - Batch arrays are not part of MCP; a top-level array body yields
//!   `-32600`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol revision this implementation supports.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Name + version served in `initialize` responses. Sourced from the binary
/// at the same place the `/version` endpoint uses.
pub const SERVER_NAME: &str = env!("CARGO_PKG_NAME");
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Id — JSON-RPC request id (string | number | absent)
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request id. Serialises as-is (string or number); the
/// `Serialize` impl is type-preserving — a numeric id comes back as a number,
/// a string id as a string.
///
/// Per MCP spec, `null` ids are rejected; this type therefore does not
/// represent `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Str(String),
}

// ---------------------------------------------------------------------------
// Request — inbound JSON-RPC message
// ---------------------------------------------------------------------------

/// A single inbound JSON-RPC 2.0 message. The `jsonrpc` field is validated
/// after deserialisation by [`validate_jsonrpc`].
#[derive(Debug, Deserialize)]
pub struct RpcMessage {
    /// Must be `"2.0"`. Validated separately so the error can carry the id.
    pub jsonrpc: Option<String>,
    /// Present on requests (and responses). Absent on notifications.
    /// We use `Value` here so we can distinguish absent (`null` in Value::Null
    /// after serde fills the missing-field default) from explicitly-present
    /// `null` (also `Value::Null`, but we check `id_present` to tell apart).
    #[serde(default)]
    pub id: Value,
    /// Whether the `id` key was present in the JSON at all. We use a custom
    /// default so serde sets this correctly via `id_present` field below.
    #[serde(skip)]
    pub id_present: bool,
    /// Method name.
    pub method: Option<String>,
    /// Method parameters (optional).
    #[serde(default)]
    pub params: Value,
}

impl RpcMessage {
    /// Parse from a `serde_json::Value` of the whole message, tracking
    /// whether `id` was explicitly present (even as null).
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let obj = match v {
            Value::Object(ref o) => o,
            _ => return serde_json::from_value(v),
        };
        let id_present = obj.contains_key("id");
        let mut msg: RpcMessage = serde_json::from_value(v)?;
        msg.id_present = id_present;
        Ok(msg)
    }

    /// True if this is a notification (no `id` key at all in the raw JSON).
    pub fn is_notification(&self) -> bool {
        !self.id_present
    }

    /// Extract and validate the `id` field.
    /// - Absent → `None` (notification).
    /// - String or number → `Some(Id)`.
    /// - Null or other type → error `-32600`.
    pub fn extract_id(&self) -> Result<Option<Id>, RpcError> {
        if !self.id_present {
            return Ok(None);
        }
        match &self.id {
            Value::String(s) => Ok(Some(Id::Str(s.clone()))),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Some(Id::Number(i)))
                } else if let Some(f) = n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15) {
                    // Tolerate whole-number floats (e.g. `1.0`) sent by some
                    // clients. Round-trip as an integer.
                    Ok(Some(Id::Number(f as i64)))
                } else {
                    Err(RpcError::invalid_request(
                        "request id must be an integer or string",
                    ))
                }
            }
            Value::Null => Err(RpcError::invalid_request(
                "request id must not be null (MCP spec §2.4)",
            )),
            _ => Err(RpcError::invalid_request("request id must be a string or number")),
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound — RpcResponse
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 response. Contains either `result` or `error`, never both.
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

impl RpcResponse {
    /// Successful response.
    pub fn ok(id: Option<Id>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.map(id_to_value),
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.map(id_to_value),
            result: None,
            error: Some(error.into_body()),
        }
    }

    /// Error response with a `null` id (used when the id cannot be determined).
    pub fn err_null_id(error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(Value::Null),
            result: None,
            error: Some(error.into_body()),
        }
    }
}

fn id_to_value(id: Id) -> Value {
    match id {
        Id::Number(n) => Value::Number(n.into()),
        Id::Str(s) => Value::String(s),
    }
}

// ---------------------------------------------------------------------------
// Errors — standard JSON-RPC error codes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct RpcErrorBody {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Helper for constructing JSON-RPC errors. Application code uses the
/// associated constructors; raw codes are only in this module.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self { code: -32700, message: msg.into(), data: None }
    }
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self { code: -32600, message: msg.into(), data: None }
    }
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self { code: -32602, message: msg.into(), data: None }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self { code: -32603, message: msg.into(), data: None }
    }

    pub fn into_body(self) -> RpcErrorBody {
        RpcErrorBody { code: self.code, message: self.message, data: self.data }
    }
}

/// Validate that `msg.jsonrpc == "2.0"`. Call after parsing. Returns
/// the appropriate `RpcError` on failure so callers can build a response.
pub fn validate_jsonrpc(msg: &RpcMessage) -> Result<(), RpcError> {
    match msg.jsonrpc.as_deref() {
        Some("2.0") => Ok(()),
        _ => Err(RpcError::invalid_request(
            r#""jsonrpc" field must be "2.0""#,
        )),
    }
}
// ---------------------------------------------------------------------------

/// `initialize` request params.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: Option<String>,
    // clientInfo and capabilities are accepted but ignored.
}

/// `initialize` result (returned inside `RpcResponse::result`).
pub fn initialize_result() -> Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        }
    })
}

// ---------------------------------------------------------------------------
// tools/list result shape
// ---------------------------------------------------------------------------

/// One MCP tool descriptor, serialisable as part of `tools/list` result.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema as a raw JSON string. Must be a valid JSON object
    /// with `"type":"object"`.
    pub input_schema_json: &'static str,
}

impl serde::Serialize for ToolInfo {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Deserialise the schema so it serialises as a proper JSON object.
        let schema: serde_json::Value = serde_json::from_str(self.input_schema_json)
            .unwrap_or(serde_json::json!({"type":"object"}));
        let mut st = s.serialize_struct("ToolInfo", 3)?;
        st.serialize_field("name", self.name)?;
        st.serialize_field("description", self.description)?;
        st.serialize_field("inputSchema", &schema)?;
        st.end()
    }
}

// ---------------------------------------------------------------------------
// tools/call result shape
// ---------------------------------------------------------------------------

/// MCP content item — only `text` type used here.
#[derive(Debug, Serialize)]
pub struct ContentItem {
    pub r#type: &'static str,
    pub text: String,
}

impl ContentItem {
    pub fn text(text: impl Into<String>) -> Self {
        Self { r#type: "text", text: text.into() }
    }
}

/// `tools/call` result value. Serialises into
/// `{"content":[...],"isError":bool}`.
pub fn tool_result(content: Vec<ContentItem>, is_error: bool) -> Value {
    serde_json::json!({
        "content": content,
        "isError": is_error,
    })
}

/// Convenience: build a successful tool result from a JSON string.
pub fn tool_ok(json_text: impl Into<String>) -> Value {
    tool_result(vec![ContentItem::text(json_text)], false)
}

/// Convenience: build an error tool result from an error message.
pub fn tool_err(msg: impl Into<String>) -> Value {
    tool_result(vec![ContentItem::text(msg)], true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip_number() {
        let id = Id::Number(42);
        let v = id_to_value(id.clone());
        assert_eq!(v, Value::Number(42i64.into()));
        // Serialise a response and check the id field.
        let resp = RpcResponse::ok(Some(id), serde_json::json!({}));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""id":42"#), "got: {s}");
    }

    #[test]
    fn id_roundtrip_string() {
        let id = Id::Str("req-1".into());
        let resp = RpcResponse::ok(Some(id), serde_json::json!({}));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""id":"req-1""#), "got: {s}");
    }

    #[test]
    fn response_has_result_not_error_on_success() {
        let resp = RpcResponse::ok(Some(Id::Number(1)), serde_json::json!({"x":1}));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("result"), "got: {s}");
        assert!(!s.contains("error"), "got: {s}");
    }

    #[test]
    fn response_has_error_not_result_on_failure() {
        let resp =
            RpcResponse::err(Some(Id::Number(1)), RpcError::method_not_found("foo"));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("error"), "got: {s}");
        assert!(!s.contains(r#""result""#), "got: {s}");
        // Code -32601
        assert!(s.contains("-32601"), "got: {s}");
    }

    #[test]
    fn null_id_rejected() {
        let msg = RpcMessage::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "method": "ping"
        }))
        .unwrap();
        assert!(msg.extract_id().is_err());
    }

    #[test]
    fn absent_id_is_notification() {
        let msg = RpcMessage::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        assert!(msg.is_notification());
        assert_eq!(msg.extract_id().unwrap(), None);
    }

    #[test]
    fn validate_jsonrpc_rejects_wrong_version() {
        let msg = RpcMessage::from_value(serde_json::json!({
            "jsonrpc": "1.0"
        }))
        .unwrap();
        assert!(validate_jsonrpc(&msg).is_err());
    }

    #[test]
    fn validate_jsonrpc_rejects_missing() {
        let msg = RpcMessage::from_value(serde_json::json!({})).unwrap();
        assert!(validate_jsonrpc(&msg).is_err());
    }

    #[test]
    fn err_null_id_serialises_null() {
        let resp = RpcResponse::err_null_id(RpcError::parse_error("bad json"));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""id":null"#), "got: {s}");
    }

    #[test]
    fn tool_result_shape() {
        let v = tool_ok(r#"{"count":3}"#);
        assert_eq!(v["isError"], false);
        let content = v["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn initialize_result_has_required_fields() {
        let v = initialize_result();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert!(v["capabilities"]["tools"].is_object());
        assert!(v["serverInfo"]["name"].is_string());
        assert!(v["serverInfo"]["version"].is_string());
    }
}
