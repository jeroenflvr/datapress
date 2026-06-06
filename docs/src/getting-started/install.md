# Install

## Install script (Linux / macOS)

The fastest way to get the standalone `datapress` CLI without a Rust
toolchain. It downloads the prebuilt binary (both backends bundled) for your
platform, verifies its checksum, and installs it into `~/.local/bin` — no
`sudo`, and your shell profile is never edited:

```bash
curl -LsSf https://datap-rs.org/install.sh | sh
```

If `~/.local/bin` is not already on your `PATH`, the script prints the exact
line to add. Override the target directory or version with:

```bash
# Install a specific version into a custom directory.
DATAPRESS_INSTALL_DIR="$HOME/bin" DATAPRESS_VERSION=0.4.4 \
  sh -c "$(curl -LsSf https://datap-rs.org/install.sh)"
```

## Install script (Windows)

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://datap-rs.org/install.ps1 | iex"
```

This installs into `%LOCALAPPDATA%\datapress\bin` and adds it to your user
`PATH`. Open a new terminal afterwards.

## Homebrew (macOS / Linux)

```bash
brew install jeroenflvr/tap/datapress
```

Apple Silicon macOS and Linux (x86_64 / aarch64) are covered by prebuilt
bottles. On Intel Macs, use the crates.io install below.

## winget (Windows)

```powershell
winget install datap-rs.DataPress
```

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
git clone https://github.com/jeroenflvr/datapress.git
cd datapress

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
