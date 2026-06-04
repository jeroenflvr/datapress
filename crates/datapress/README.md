# datapress

A single `datapress` binary that bundles **both** dataset HTTP backends —
DuckDB and DataFusion — and picks the active one at runtime from
`server.backend` in your `datasets.toml`.

```sh
cargo install datapress    # installs the `datapress` binary
datapress                  # serves using the resolved datasets.toml
```

## Configuration file

`datapress` resolves its config in this order (first match wins):

1. `--config <FILE>` flag
2. `$DATAPRESS_CONFIG_FILE` environment variable
3. `./datasets.toml` (current directory)
4. `$HOME/datasets.toml`

Generate a starter config with the `init` subcommand. It writes a
commented `datasets.toml.template` to the given directory, or your home
directory when omitted:

```sh
datapress init                 # writes ~/datasets.toml.template
datapress init ./config        # writes ./config/datasets.toml.template
datapress init --force         # overwrite an existing template

# then:
cp ~/datasets.toml.template ~/datasets.toml   # and edit
datapress
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

See the [workspace README](https://github.com/jeroenflvr/datapress) for full
configuration details.
