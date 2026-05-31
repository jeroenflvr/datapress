# Install

## Prebuilt binary (crates.io)

The quickest way to get a server without cloning the repo. The unified
`datapress` binary bundles **both** the DuckDB and DataFusion backends and
selects one at runtime from `server.backend` in your `datasets.toml`:

```bash
cargo install datapress
datapress                  # reads ./datasets.toml (or $DATASETS_CONFIG)
```

For a slimmer single-backend build, or to opt into the docs / Swagger /
metrics / auth features:

```bash
cargo install datapress --no-default-features --features duckdb
cargo install datapress --features swagger,auth,metrics
```

### Configuration discovery

The installed `datapress` binary finds its config in this order (first
match wins):

1. `--config <FILE>`
2. `$DATAPRESS_CONFIG_FILE`
3. `./datasets.toml`
4. `$HOME/datasets.toml`

Generate a commented starter template with `datapress init` (writes to
`$HOME` when no directory is given):

```bash
datapress init                 # ~/datasets.toml.template
datapress init ./config        # ./config/datasets.toml.template
cp ~/datasets.toml.template ~/datasets.toml   # then edit and run `datapress`
```

## From source (Rust binaries)

Two binaries live in the workspace, one per backend. Both build from a
checkout of the repo:

```bash
git clone https://github.com/jeroenflvr/fast-api.git
cd fast-api

cargo build --release -p datapress-duckdb
cargo build --release -p datapress-datafusion
```

The release binaries land in `target/release/datapress-{duckdb,datafusion}`.

If you have [`task`](https://taskfile.dev/) installed:

```bash
task build:duckdb          # DuckDB binary
task build:datafusion      # DataFusion binary
task build                 # both
```

## Python wheel

The Python wheel `datap-rs` bundles **both** engines and lets you pick
one at runtime via `DataPressConfig(backend=...)`.

```bash
pip install datap-rs
# or
uv pip install datap-rs
```

Wheels are published for Linux (x86_64/aarch64), macOS (arm64), and
Windows (x86_64) against CPython 3.9+ (abi3).

Building the wheel from source:

```bash
task py:develop     # editable install into ./.venv (uses uv + maturin)
task py:build       # release wheel into ./target/wheels/
```

## Optional features

| Feature   | Crate              | Purpose                                                                  |
|-----------|--------------------|--------------------------------------------------------------------------|
| `docs`    | `datapress-core`   | Embed this documentation site into the binary. Disabled by default.      |

Enable at build time:

```bash
task docs:build
cargo build --release -p datapress-duckdb --features docs
```

The same features are also forwarded by the unified `datapress` crate, so
they work with `cargo install datapress --features docs` too.

See [Configuration › Documentation site](../configuration/docs-site.md)
for the runtime switch.
