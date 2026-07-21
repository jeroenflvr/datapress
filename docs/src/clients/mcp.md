---
description: >-
  Connect AI agents and LLM applications to DataPress via MCP (Model Context
  Protocol) — with examples for Claude Desktop, Claude Code, and VS Code.
---

# AI Agents / MCP

DataPress exposes its dataset query surface as [MCP (Model Context
Protocol)][mcp] tools, so any MCP-compatible AI agent or LLM application can
discover, describe, and query your datasets over a standard JSON-RPC 2.0
streamable-HTTP connection.

Protocol revision: **MCP 2025-11-25**.

[mcp]: https://modelcontextprotocol.io/

## Prerequisites

1. Build DataPress with the `mcp` feature:

   ```bash
   cargo build --release -p datapress-duckdb --features mcp
   ```

2. Enable the endpoint in `datasets.toml`:

   ```toml
   [mcp]
   enabled = true
   # path = "/mcp"   # default
   ```

3. Restart the server. The startup log will include:

   ```
     /mcp (MCP endpoint):
       POST   /mcp
       DELETE /mcp
   ```

## Claude Desktop

Add a server entry in `~/Library/Application Support/Claude/claude_desktop_config.json`
(macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "datapress": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-fetch"],
      "env": {
        "MCP_SERVER_URL": "http://localhost:8080/mcp"
      }
    }
  }
}
```

> **Authentication**: if `[auth]` is enabled with `anonymous_read = false`,
> add `"Authorization": "Bearer <token>"` to the `headers` env var supported
> by your fetch server wrapper, or configure Claude Desktop's bearer token.

## Claude Code (CLI)

```bash
claude mcp add datapress --transport http http://localhost:8080/mcp
```

With a bearer token:

```bash
claude mcp add datapress --transport http http://localhost:8080/mcp \
  --header "Authorization: Bearer $DATAPRESS_TOKEN"
```

## VS Code (GitHub Copilot)

Add to your VS Code `settings.json`:

```json
{
  "github.copilot.chat.mcp.servers": {
    "datapress": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

## Verifying the connection

Use the [MCP Inspector][inspector] to test the endpoint manually:

```bash
npx @modelcontextprotocol/inspector http://localhost:8080/mcp
```

The inspector shows the `initialize` handshake, `tools/list` result, and lets
you invoke each tool interactively.

[inspector]: https://github.com/modelcontextprotocol/inspector

## Available tools

| Tool | When |
|------|------|
| `list_datasets` | Always |
| `describe_dataset` | Always |
| `describe_all_datasets` | Always |
| `query_dataset` | Always |
| `count_rows` | Always |
| `sql` | Only when `[mcp].expose_sql = true` AND `[sql].enabled = true` |

## Typical agent workflow

1. Call `list_datasets` → discover what data exists.
2. Call `describe_dataset` (or `describe_all_datasets` for joins) → get column names and types.
3. Call `count_rows` with predicates → check result size before paginating.
4. Call `query_dataset` → run structured queries with filters, sorting, and pagination.
5. Call `sql` (if enabled) → express joins or complex expressions the structured tool cannot.
