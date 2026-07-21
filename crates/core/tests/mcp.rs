//! Integration tests for the MCP endpoint.
//!
//! Tests cover: JSON-RPC conformance, lifecycle, tools/list, tools/call for
//! every tool, error paths, origin validation, and the disabled-route (404).
//!
//! Compiled only when the `mcp` feature is enabled.

#![cfg(feature = "mcp")]

use std::sync::Arc;

use actix_web::{App, http::StatusCode, test, web};
use async_trait::async_trait;
use serde_json::{Value, json};

use datapress_core::backend::{
    Backend, DatasetSummary, ReloadStats,
};
use datapress_core::config::{McpConfig, SqlConfig};
use datapress_core::errors::AppError;
use datapress_core::mcp::http::{McpSettings, configure};
use datapress_core::models::{CountRequest, QueryRequest};
use datapress_core::schema::{ColumnInfo, DatasetSchema, LogicalType};

// ---------------------------------------------------------------- mock ----

struct MockBackend;

#[async_trait]
impl Backend for MockBackend {
    fn names(&self) -> Vec<String> {
        vec!["events".into()]
    }

    fn summary(&self, name: &str) -> Result<DatasetSummary, AppError> {
        match name {
            "events" => Ok(DatasetSummary {
                name: "events".into(),
                columns: 2,
                rows: 10,
                lazy: false,
            }),
            _ => Err(AppError::NotFound(format!("dataset '{name}' not found"))),
        }
    }

    fn schema(&self, name: &str) -> Result<Arc<DatasetSchema>, AppError> {
        match name {
            "events" => Ok(Arc::new(DatasetSchema::new(
                "events",
                vec![
                    ColumnInfo {
                        name: "id".into(),
                        logical: LogicalType::Int,
                        sql_type: "BIGINT".into(),
                        nullable: false,
                    },
                    ColumnInfo {
                        name: "msg".into(),
                        logical: LogicalType::Utf8,
                        sql_type: "VARCHAR".into(),
                        nullable: true,
                    },
                ],
            ))),
            _ => Err(AppError::NotFound(format!("dataset '{name}' not found"))),
        }
    }

    async fn sample(&self, _name: &str) -> Result<String, AppError> {
        Ok(r#"{"id":1,"msg":"hello"}"#.into())
    }

    async fn query(&self, name: &str, _req: &QueryRequest) -> Result<String, AppError> {
        match name {
            "events" => Ok(r#"[{"id":1,"msg":"hello"},{"id":2,"msg":"world"}]"#.into()),
            _ => Err(AppError::NotFound(format!("dataset '{name}' not found"))),
        }
    }

    async fn count(&self, name: &str, _req: &CountRequest) -> Result<i64, AppError> {
        match name {
            "events" => Ok(10),
            _ => Err(AppError::NotFound(format!("dataset '{name}' not found"))),
        }
    }

    async fn reload(&self, _name: &str) -> Result<ReloadStats, AppError> {
        Ok(ReloadStats { rows: 10, elapsed_ms: 1, ..Default::default() })
    }

    async fn query_sql(
        &self,
        _sql: &str,
        _datasets: &[String],
        _max_rows: u64,
    ) -> Result<String, AppError> {
        Ok(r#"[{"id":1}]"#.into())
    }
}

// ---------------------------------------------------------------- helpers -

fn default_settings() -> McpSettings {
    McpSettings {
        enabled: true,
        mcp: McpConfig { enabled: true, expose_sql: true, page_size: 10, ..Default::default() },
        sql: SqlConfig { enabled: true, max_rows: 1000 },
        max_page_size: 1000,
        own_host: "localhost:8080".into(),
    }
}

macro_rules! mk_app {
    ($settings:expr) => {{
        let s = web::Data::new($settings);
        test::init_service(
            App::new()
                .app_data(web::Data::<Arc<dyn Backend>>::new(Arc::new(MockBackend)))
                .configure(|c| configure("/mcp", s, c)),
        )
        .await
    }};
}

// ---------------------------------------------------------------- tests ---

#[actix_web::test]
async fn test_initialize_returns_protocol_version() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert!(body["result"]["capabilities"]["tools"].is_object());
    assert!(body["result"]["serverInfo"]["name"].is_string());
}

#[actix_web::test]
async fn test_initialize_negotiates_unknown_version() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"3000-01-01"}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
}

#[actix_web::test]
async fn test_initialize_response_has_mcp_session_id() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("mcp-session-id"));
}

#[actix_web::test]
async fn test_ping() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":"p1","method":"ping","params":{}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], "p1"); // string id preserved
    assert!(body["result"].is_object());
    assert!(body.get("error").is_none() || body["error"].is_null());
}

#[actix_web::test]
async fn test_notification_returns_202() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body = test::read_body(resp).await;
    assert!(body.is_empty());
}

#[actix_web::test]
async fn test_unknown_method_returns_32601() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["error"]["code"], -32601);
}

#[actix_web::test]
async fn test_malformed_json_returns_32700() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(b"{this is not json".as_ref())
        .to_request();
    let resp = test::call_service(&app, req).await;
    let status = resp.status();
    let body: Value = serde_json::from_slice(&test::read_body(resp).await).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32700);
}

#[actix_web::test]
async fn test_batch_array_returns_32600() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(
            b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]".as_ref(),
        )
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = serde_json::from_slice(&test::read_body(resp).await).unwrap();
    assert_eq!(body["error"]["code"], -32600);
}

#[actix_web::test]
async fn test_wrong_jsonrpc_version_returns_32600() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"1.0","id":1,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["error"]["code"], -32600);
}

#[actix_web::test]
async fn test_null_id_returns_32600() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":null,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["error"]["code"], -32600);
}

#[actix_web::test]
async fn test_tools_list_contains_all_tools() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"list_datasets"));
    assert!(names.contains(&"describe_dataset"));
    assert!(names.contains(&"describe_all_datasets"));
    assert!(names.contains(&"query_dataset"));
    assert!(names.contains(&"count_rows"));
    assert!(names.contains(&"sql")); // expose_sql=true, sql.enabled=true
    for tool in tools {
        assert!(
            tool["inputSchema"]["type"].as_str() == Some("object"),
            "tool: {tool}"
        );
    }
}

#[actix_web::test]
async fn test_sql_tool_absent_when_expose_sql_false() {
    let mut settings = default_settings();
    settings.mcp.expose_sql = false;
    let app = mk_app!(settings);
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(!tools.iter().any(|t| t["name"] == "sql"));
}

#[actix_web::test]
async fn test_call_list_datasets() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_datasets","arguments":{}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert!(data["datasets"].is_array());
}

#[actix_web::test]
async fn test_call_describe_dataset() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"describe_dataset","arguments":{"name":"events"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert_eq!(data["name"], "events");
    assert!(data["columns"].is_array());
}

#[actix_web::test]
async fn test_call_describe_dataset_unknown_returns_is_error() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"describe_dataset","arguments":{"name":"nope"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);
}

#[actix_web::test]
async fn test_call_describe_all_datasets() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"describe_all_datasets","arguments":{}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert!(data["events"]["columns"].is_array());
}

#[actix_web::test]
async fn test_call_query_dataset() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"query_dataset","arguments":{"name":"events"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert!(data["data"].is_array());
    assert_eq!(data["page"], 1);
}

#[actix_web::test]
async fn test_call_query_dataset_default_page_size_injected() {
    let mut settings = default_settings();
    settings.mcp.page_size = 50;
    let app = mk_app!(settings);
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"query_dataset","arguments":{"name":"events"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert_eq!(data["page_size"], 50);
}

#[actix_web::test]
async fn test_call_query_dataset_unknown_returns_is_error() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"query_dataset","arguments":{"name":"ghost"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);
}

#[actix_web::test]
async fn test_call_count_rows() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"count_rows","arguments":{"name":"events"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let data: Value = serde_json::from_str(text).unwrap();
    assert_eq!(data["count"], 10);
}

#[actix_web::test]
async fn test_call_sql_tool() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"sql","arguments":{"sql":"SELECT id FROM events LIMIT 1"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);
}

#[actix_web::test]
async fn test_call_sql_multi_statement_is_error() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"sql","arguments":{"sql":"SELECT 1; SELECT 2"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);
}

#[actix_web::test]
async fn test_call_sql_unregistered_table_is_error() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"sql","arguments":{"sql":"SELECT * FROM secrets"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);
}

#[actix_web::test]
async fn test_call_sql_denied_function_is_error() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"sql","arguments":{"sql":"SELECT read_text('/etc/passwd') FROM events"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);
}

#[actix_web::test]
async fn test_call_unknown_tool_returns_32602() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":18,"method":"tools/call","params":{"name":"nonexistent","arguments":{}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["error"]["code"], -32602);
}

#[actix_web::test]
async fn test_get_returns_405() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::get().uri("/mcp").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[actix_web::test]
async fn test_delete_with_session_id_returns_200() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::delete()
        .uri("/mcp")
        .insert_header(("Mcp-Session-Id", "abc123"))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[actix_web::test]
async fn test_delete_without_session_id_returns_405() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::delete().uri("/mcp").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[actix_web::test]
async fn test_disabled_endpoint_returns_404() {
    let mut settings = default_settings();
    settings.enabled = false;
    settings.mcp.enabled = false;
    let app = mk_app!(settings);
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":19,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn test_origin_denied_unknown_host() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("Origin", "https://evil.example.org"))
        .set_json(json!({"jsonrpc":"2.0","id":20,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
async fn test_unsupported_protocol_version_header_returns_400() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("MCP-Protocol-Version", "9999-01-01"))
        .set_json(json!({"jsonrpc":"2.0","id":21,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn test_accept_json_only_rejects_html() {
    let app = mk_app!(default_settings());
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("Accept", "text/html"))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".as_ref())
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
}

/// Conformance session replay (§4.4 of the brief).
#[actix_web::test]
async fn test_canonical_session_replay() {
    let app = mk_app!(default_settings());

    // 1. initialize
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2025-11-25"}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");

    // 2. notifications/initialized → 202, empty body
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(test::read_body(resp).await.is_empty());

    // 3. ping
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert!(body["result"].is_object());

    // 4. tools/list
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert!(body["result"]["tools"].is_array());

    // 5. tools/call list_datasets
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_datasets","arguments":{}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], false);

    // 6. tools/call unknown tool → -32602
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["error"]["code"], -32602);

    // 7. tools/call query_dataset bad dataset → isError: true, HTTP 200
    let req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"query_dataset","arguments":{"name":"ghost"}}}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["result"]["isError"], true);

    // 8. batch array → -32600
    let req = test::TestRequest::post()
        .uri("/mcp")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(
            b"[{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"ping\"}]".as_ref(),
        )
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = serde_json::from_slice(&test::read_body(resp).await).unwrap();
    assert_eq!(body["error"]["code"], -32600);

    // 9. GET → 405
    let req = test::TestRequest::get().uri("/mcp").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

    // 10. DELETE with session header → 200
    let req = test::TestRequest::delete()
        .uri("/mcp")
        .insert_header(("Mcp-Session-Id", "test-session-id"))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}


// ---------------------------------------------------------------- mock ----

