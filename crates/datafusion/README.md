# datapress-datafusion

Apache Arrow + DataFusion-backed implementation of
[datapress](https://github.com/jeroenflvr/datapress) — a JSON HTTP API over
Parquet / Delta datasets.

It pairs [`datapress-core`](https://crates.io/crates/datapress-core) with a
DataFusion `SessionContext`: datasets declared in `datasets.toml` are inferred
at startup and served over the shared v1 API (list / schema / query / count /
reload), including JSON and Arrow IPC responses. Object-store sources (S3) and
Delta tables are supported.

This crate exposes the same request/response shapes as
[`datapress-duckdb`](https://crates.io/crates/datapress-duckdb), so you can A/B
the two engines under identical workloads.

## Usage

```bash
# Build and run the binary.
cargo run -p datapress-datafusion --release

# Talk to it.
curl http://localhost:8080/api/datasets
```

## Features

`docs`, `swagger`, `metrics`, and `auth` forward to the matching
[`datapress-core`](https://crates.io/crates/datapress-core) features.

## License

MIT — see [LICENSE](../../LICENSE).
