//! MCP (Model Context Protocol) server — streamable HTTP transport.
//!
//! Compiled only when the `mcp` cargo feature is enabled. Provides:
//! - JSON-RPC 2.0 types ([`protocol`])
//! - Tool registry and dispatch ([`tools`])
//! - Actix-web HTTP handlers ([`http`])
//!
//! Mount in `server.rs` inside the prefix scope alongside other optional
//! surfaces. Configure with the `[mcp]` block in `datasets.toml`.

pub mod http;
pub mod protocol;
pub mod tools;
