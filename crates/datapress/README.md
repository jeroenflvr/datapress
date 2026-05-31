# datapress

A single `datapress` binary that bundles **both** dataset HTTP backends —
DuckDB and DataFusion — and picks the active one at runtime from
`server.backend` in your `datasets.toml`.

```sh
cargo install datapress    # installs the `datapress` binary
datapress                  # reads ./datasets.toml (or $DATASETS_CONFIG)
```

## Choosing a backend

```toml
# datasets.toml
[server]
backend = "duckdb"      # or "datafusion"
```

## Slimmer single-backend builds

Both backends are compiled in by default. To build just one:

```sh
cargo install datapress --no-default-features --features duckdb
# or
cargo install datapress --no-default-features --features datafusion
```

## Optional features

`docs`, `swagger`, `metrics`, and `auth` are forwarded to whichever
backends are enabled, e.g.:

```sh
cargo install datapress --features swagger,auth,metrics
```

See the [workspace README](https://github.com/jeroenflvr/fast-api) for full
configuration details.
