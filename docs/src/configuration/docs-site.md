# Documentation site

DataPress can serve **this site** directly from the running binary.
Disabled by default; opt in with a feature flag at build time **and** a
config switch at runtime.

## Build with the `docs` feature

```bash
task docs:build                                        # build the MkDocs site
cargo build --release -p datapress-duckdb --features docs
# or:
cargo build --release -p datapress-datafusion --features docs
```

The feature pulls in [`include_dir`](https://crates.io/crates/include_dir)
and [`mime_guess`](https://crates.io/crates/mime_guess), and embeds the
built `docs/site/` directory into the binary.

!!! note "Why a feature flag?"
    Embedded HTML/CSS/JS adds a few hundred KB to the binary. Users
    who don't want it pay nothing.

## Enable at runtime

Add a `[docs]` section to `datasets.toml`:

```toml
[docs]
enabled = true           # default: false
path    = "/mkdocs"      # default: /mkdocs
```

| Field     | Default     | Notes                                                                          |
|-----------|-------------|--------------------------------------------------------------------------------|
| `enabled` | `false`     | Master switch. The whole section can be omitted to disable.                    |
| `path`    | `/mkdocs`   | Mount point. Must start with `/`, not end with `/`. Reserved paths are rejected (`/api`, `/api/v1`, `/health*`, `/readyz`, `/version`, `/docs` is reserved for the Swagger UI). |

When the **feature is off** but `enabled = true`, the server logs a
warning at startup and continues — it does not refuse to start:

```
WARN  [docs] enabled = true in config, but this binary was built without --features docs; skipping
```

When both the feature and the switch are on, the startup log records
the mount:

```
Routes:
  /mkdocs (docs site):
    GET    /mkdocs/
    GET    /mkdocs/{path}
```

## Local development

While editing Markdown sources under `docs/src/`, use MkDocs' built-in
hot-reload server:

```bash
task docs:serve
# → http://127.0.0.1:8001
```

Once you're happy, rebuild and re-`cargo build --features docs` to pick
up the changes in the binary.
