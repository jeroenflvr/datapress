---
description: >-
  Configure the MCP (Model Context Protocol) endpoint to expose DataPress
  datasets as AI agent tools over JSON-RPC 2.0 streamable HTTP.
---

# MCP (AI Agent Tools)

DataPress can serve an [MCP (Model Context Protocol)][mcp] server at a
configurable path (default `/mcp`). AI agents and LLM-powered applications
connect to it and call DataPress datasets as structured tools — discovering
schemas, running queries, and counting rows — using any MCP-compatible client.

The protocol layer is hand-rolled JSON-RPC 2.0 with no extra runtime
dependencies. Target revision: **MCP 2025-11-25**.

[mcp]: https://modelcontextprotocol.io/

## Build

MCP is opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features mcp
```

When the binary is built without the `mcp` feature but `[mcp] enabled = true`
is set in the TOML, the server logs a warning at startup and skips the mount.
You can disable it at runtime without recompiling by setting `enabled = false`.

## Configuration

```toml
[mcp]
enabled    = false    # default FALSE — opt in explicitly
path       = "/mcp"   # mount point; must start with "/"
expose_sql = false    # also offer the raw-SQL tool (requires [sql].enabled = true)
page_size  = 100      # default rows per page for query_dataset calls
# allowed_origins = []  # extra origins accepted (see Security below)
```

| Key               | Default   | Notes |
|-------------------|-----------|-------|
| `enabled`         | `false`   | Master switch. **Off by default** — MCP exposes the full query surface. |
| `path`            | `"/mcp"`  | Mount point. Must start with `/`, not end with `/`, not collide with `/api`, `/healthz`, or other special routes. |
| `expose_sql`      | `false`   | Offer the `sql` tool only when this AND `[sql].enabled = true`. |
| `page_size`       | `100`     | Injected into `query_dataset` calls that omit `page_size`. Clamped to `server.max_page_size`. |
| `allowed_origins` | `[]`      | Extra `Origin` values accepted in addition to same-host and localhost. |

## Tools

When connected, an MCP client can call these tools:

| Tool | Description |
|------|-------------|
| `list_datasets` | List all datasets with name and column count. Call first to discover available data. |
| `describe_dataset` | Schema + sample row for one dataset. Call before writing predicates. |
| `describe_all_datasets` | Column schemas for every dataset in one call. Use when planning a join. |
| `query_dataset` | Structured query: projection, predicates, group-by, aggregations, sorting, pagination. |
| `count_rows` | Row count with optional predicates. Cheap — call before querying unknown-size datasets. |
| `sql` | Read-only SELECT across any registered datasets. Only available when `expose_sql = true` AND `[sql].enabled = true`. |

## Security

### Origin validation

The MCP endpoint validates the `Origin` header (when present) to prevent
DNS-rebinding attacks. Requests are accepted from:

- No `Origin` header (curl, server-to-server clients).
- `localhost`, `127.0.0.1`, `[::1]` (any port).
- The server's own bind host.
- Entries in `allowed_origins`.

Browser-based MCP clients connected through a reverse proxy must add the
proxy's origin:

```toml
[mcp]
allowed_origins = ["https://my-proxy.example.com"]
```

### Authentication

When the `auth` feature is compiled in and `[auth].enabled = true`, the MCP
endpoint enforces the same token requirements as the API:

- `anonymous_read = true` → full MCP surface is public.
- Otherwise, every tool call requires a valid bearer token with `read_scopes`.

On `401`, the server responds with:
```
WWW-Authenticate: Bearer resource_metadata="{origin}/.well-known/oauth-protected-resource"
```

The `/.well-known/oauth-protected-resource` endpoint (RFC 9728) is served at
the root (not under the prefix) when both `mcp` and `auth` features are on and
both are runtime-enabled.

## Connecting clients

See [AI agents / MCP](../clients/mcp.md) for connection examples with Claude
Desktop, Claude Code, and VS Code.
