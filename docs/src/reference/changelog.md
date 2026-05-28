# Changelog

The canonical changelog lives in the repo root:

[`CHANGELOG.md`](https://github.com/jeroenflvr/fast-api/blob/main/CHANGELOG.md)

It tracks both binary and Python-wheel releases.

## Recent highlights

- **0.1.17** — `/version` build-info endpoint; embedded MkDocs site
  behind the optional `docs` cargo feature.
- **0.1.16** — Graceful shutdown on `SIGTERM`/`SIGINT` with configurable
  `shutdown_timeout_secs`.
- **0.1.15** — DuckDB Arrow IPC responses via `query_arrow`.
- **0.1.14** — `/healthz` and `/readyz` probes.
- **0.1.13** — Per-request timeout middleware (`request_timeout_ms`,
  `504` envelope).
