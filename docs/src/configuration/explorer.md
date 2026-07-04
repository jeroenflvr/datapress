---
description: >-
  Configure the DataPress dataset explorer UI — the built-in server-rendered
  web app for browsing datasets, schemas, and running queries in the browser.
---

# Explorer UI

DataPress ships an optional browser-based explorer that lets you browse
registered datasets, inspect schemas, run structured API queries, and open
a live DuckDB-WASM terminal — all without leaving the browser.

It is served at a configurable path (default `/explore`) directly by the
DataPress process. No separate web server is needed.

## Build

The explorer is opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,explorer
```

When the binary is built without the `explorer` feature but
`[explorer] enabled = true` is set in the TOML, the server logs a warning
at startup and skips the mount. The feature flag has no effect on the HTTP
API.

## Configuration

```toml
[explorer]
enabled = true        # default; set false to hide the UI at runtime
path    = "/explore"  # mount point
```

| Key       | Default      | Notes                                                                                          |
|-----------|--------------|-----------------------------------------------------------------------------------------------|
| `enabled` | `true`       | Master switch. Set `false` to suppress the UI even when the feature is compiled in.            |
| `path`    | `"/explore"` | Mount point. Must start with `/` and not end with `/`. Cannot collide with `/api`, `/health*`, `/version`, or other reserved mounts. |

## What it shows

Open `http://localhost:8080/explore` (or your configured path) in a browser.

### Discovery tab

Lists every registered dataset with:

- row count, column count, and backing file size
- source kind (parquet / delta / lazy) and location
- schema — column names and inferred types
- equality index configuration (when enabled)

Datasets are sortable by name, row count, or column count.

### API Query tab

An in-browser query builder with two modes:

**Structured JSON** — builds a `POST /api/v1/datasets/<name>/query` request
(the same structured `QueryRequest` body used by the HTTP API). Supports
column projection, predicates, grouping, aggregation, sorting, pagination,
and result export as CSV, JSON, or Parquet (Parquet export uses a bundled
DuckDB-WASM instance entirely in-browser — no server round-trip).

**Raw SQL** — sends a `POST /api/v1/sql` request. This tab is only active
when `[sql] enabled = true`; otherwise it shows a warning banner. A
"To JSON query" button translates the SQL to a structured `QueryRequest`
and switches back to JSON mode — useful when the SQL endpoint is disabled
but you want to draft a query interactively.

Both modes support an **Arrow IPC** response toggle for faster large
results, custom request headers, and a timing readout showing time-to-first-
byte, total transfer time, body size, and row count.

### DuckDB terminal tab

An embedded DuckDB-WASM terminal that queries the Parquet export of each
dataset directly in the browser. The terminal connects to the server's
Parquet download endpoint for each dataset; no SQL is executed server-side
and no credentials are transmitted to the WASM sandbox. The DuckDB-WASM
bundle is self-hosted and served by DataPress itself (no CDN).

## Security note

The explorer uses the same session / cookie context as the browser. If
`[auth]` is enabled, the explorer's API requests inherit the browser's
`Authorization` header from the page that opened the explorer — or the
session cookie for same-origin requests. There is no separate explorer
credential. Disable the explorer (`enabled = false`) or restrict network
access if you do not want the UI reachable from untrusted networks.

## From Python

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

config = DataPressConfig(
    backend="duckdb",
    port=8080,
    explorer_enabled=True,
    explorer_path="/explore",
)
```
