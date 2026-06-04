---
description: >-
  Configure DataPress from a single TOML file: server settings, datasets,
  S3 / object storage, and equality indexing for low-latency filters.
---

# Configuration

Every DataPress instance reads a single TOML file at startup. By
convention it's called `datasets.toml`; override with the
`DATASETS_CONFIG` environment variable.

It has one `[server]` block and one `[[dataset]]` block per table you
want to expose.

```toml
[server]                  # optional; defaults shown below
backend = "datafusion"    # or "duckdb"
listen  = "127.0.0.1"
port    = 8080

[[dataset]]               # one block per dataset
name = "..."
# source, s3, index, lazy follow
```

## Pages

- [Server settings](server.md) — listen, port, workers, prefix,
  compression, body limits, timeouts, graceful shutdown.
- [Datasets](datasets.md) — `source`, `lazy`, parquet vs delta, local
  files, directories, globs.
- [S3 / object storage](s3.md) — credentials, endpoints, addressing
  styles, per-dataset env overrides.
- [Indexing](indexing.md) — DataFusion equality-index policy.
- [Documentation site](docs-site.md) — enabling the embedded MkDocs site.

## Optional feature blocks

A few features are opt-in and configured in their own block:

- `[sql]` — the [raw SQL endpoint](../query/sql.md) (`POST /api/v1/sql`).
  Disabled by default; set `enabled = true` to expose it.
