# datapress-core

Backend-agnostic core for [datapress](https://github.com/jeroenflvr/datapress) —
a Rust workspace that exposes Parquet / Delta datasets over a JSON HTTP API.

This crate holds the pieces shared by every backend:

- Configuration model (`datasets.toml` parsing, server/auth/docs/swagger/metrics
  blocks).
- The `Backend` trait and request/response models (`QueryRequest`, predicates,
  aggregation plans).
- Centralized `AppError` with HTTP status mapping.
- actix-web routing and v1 handlers, plus optional Swagger UI, Prometheus
  metrics, and OIDC auth (all feature-gated).

It is consumed by the backend crates
[`datapress-duckdb`](https://crates.io/crates/datapress-duckdb) and
[`datapress-datafusion`](https://crates.io/crates/datapress-datafusion); you
typically depend on one of those rather than on this crate directly.

## Features

| Feature      | Effect                                                        |
|--------------|---------------------------------------------------------------|
| `duckdb`     | Enables the DuckDB `From` error conversions.                  |
| `datafusion` | Enables the Arrow/Parquet/DataFusion `From` error conversions.|
| `docs`       | Embed and serve the MkDocs site.                              |
| `swagger`    | Embed Swagger UI + OpenAPI spec.                              |
| `metrics`    | Expose a Prometheus `/metrics` endpoint.                      |
| `auth`       | OIDC bearer-token authentication + scope enforcement.         |

## License

MIT — see [LICENSE](../../LICENSE).
