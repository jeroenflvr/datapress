# Operations

Day-2 concerns for running DataPress in production.

- [Probes](probes.md) — `/healthz`, `/readyz`, `/version`,
  `{prefix}/health`.
- [Graceful shutdown](graceful-shutdown.md) — `SIGTERM` handling and
  `shutdown_timeout_secs` tuning.
- [Logging](logging.md) — actix request log format, `RUST_LOG`.
- [Authentication](auth.md) — OIDC / OAuth2 bearer enforcement,
  Swagger UI SSO, free providers for testing.
