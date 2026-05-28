# Install

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

Wheels are published for macOS (arm64/x86_64), Linux (x86_64/aarch64)
and Windows (x86_64) against CPython 3.9+ (abi3).

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

See [Configuration › Documentation site](../configuration/docs-site.md)
for the runtime switch.
