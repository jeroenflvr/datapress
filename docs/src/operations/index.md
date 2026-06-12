---
description: >-
  Operate DataPress in production: health and readiness probes, version metadata,
  graceful shutdown, structured logging, Prometheus metrics, hot reloads, and OIDC/OAuth2.
---

# Operations

Day-2 concerns for running DataPress in production.

- [Probes](probes.md) — `/healthz`, `/readyz`, `/version`,
  `{prefix}/health`.
- [Dataset reload](reload.md) — backend-specific reload semantics,
  DataFusion double-buffering, and DuckDB transactional replacement.
- [Graceful shutdown](graceful-shutdown.md) — `SIGTERM` handling and
  `shutdown_timeout_secs` tuning.
- [Logging](logging.md) — actix request log format, `RUST_LOG`.
- [Prometheus metrics](metrics.md) — opt-in `/metrics` endpoint,
  request counters and latency histograms.
- [Authentication](auth.md) — OIDC / OAuth2 bearer enforcement,
  Swagger UI SSO, free providers for testing.
- [Troubleshooting](troubleshooting.md) — OOM kills during dataset load,
  cold-cache queries, reload 403s, and the DuckDB CXX ABI build error.
