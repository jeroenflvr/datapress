# datapress-core

Backend-agnostic core for [datapress](https://github.com/jeroenflvr/datapress) —
a Rust workspace that exposes Parquet / Delta datasets over a JSON HTTP API.

[Overview presentation](https://datap-rs.org) ·
[Documentation](https://docs.datap-rs.org)

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
| `explorer`   | Embed and serve the browser explorer UI at `/explore`.        |

> **Note:** The explorer's API Query tab decodes Arrow IPC responses in the
> browser using a vendored Apache Arrow JS bundle. We currently build this
> bundle from source (`apache/arrow-js`, pinned commit) rather than using a
> published `apache-arrow` npm release, because DataFusion emits `Utf8View` for
> Parquet string columns and `Utf8View`/`BinaryView` read support
> ([apache/arrow-js#320](https://github.com/apache/arrow-js/pull/320)) is merged
> on `main` but not yet in any published release. See the `docs:vendor-arrow`
> task. Bump the pinned commit to a release once one ships including #320.

## License

MIT — see [LICENSE](../../LICENSE).
