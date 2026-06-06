# datapress-duckdb

DuckDB-backed implementation of [datapress](https://github.com/jeroenflvr/datapress)
— a JSON HTTP API over Parquet / Delta datasets.

[Overview presentation](https://datap-rs.org) ·
[Documentation](https://docs.datap-rs.org)

It pairs [`datapress-core`](https://crates.io/crates/datapress-core) with an
in-memory DuckDB registry: datasets declared in `datasets.toml` are inferred at
startup and served over the shared v1 API (list / schema / query / count /
reload), including JSON and Arrow IPC responses.

## Usage

```bash
# Build and run the binary.
cargo run -p datapress-duckdb --release

# Talk to it.
curl http://localhost:8080/api/datasets
```

Datasets are configured in `datasets.toml`; see the
[documentation](https://github.com/jeroenflvr/datapress) for the full schema.

## Features

`docs`, `swagger`, `metrics`, and `auth` forward to the matching
[`datapress-core`](https://crates.io/crates/datapress-core) features.

## License

MIT — see [LICENSE](../../LICENSE).
