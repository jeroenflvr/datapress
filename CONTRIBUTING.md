# Contributing to datapress

Thanks for your interest in contributing! This project is a Rust
[Cargo workspace](Cargo.toml) that serves Parquet / Delta datasets over a JSON
HTTP API on two interchangeable engines (DuckDB and Apache DataFusion), plus a
Python wheel (`datap-rs`) that bundles both.

## Getting set up

You'll need:

- A recent stable **Rust** toolchain (the workspace uses edition 2024).
- [**Task**](https://taskfile.dev/) for the convenience targets (optional but
  recommended) — run `task --list` to see everything.
- [**uv**](https://docs.astral.sh/uv/) for the Python wheel and docs
  (this repo never calls `pip` directly).

```bash
# Type-check the whole workspace.
task check          # or: cargo check --workspace --all-targets

# Build / run a backend.
task run:duckdb     # or: task run:datafusion

# Build + install the Python wheel into ./.venv.
task py:develop
```

## Before you open a PR

Please make sure the following pass locally — CI runs the same checks
(see [.github/workflows/ci.yml](.github/workflows/ci.yml)):

```bash
cargo clippy --workspace --all-targets --locked   # lints (CI treats warnings as errors)
cargo test  --workspace                           # unit + integration tests
cargo audit                                        # RUSTSEC advisory scan
```

When touching the docs site, build it in strict mode from the repo root:

```bash
uv tool run --with mkdocs-material mkdocs build -f docs/mkdocs.yml -d site --strict
```

### Formatting

CI does **not** currently enforce `cargo fmt --check` because parts of the
codebase rely on hand-aligned formatting that conflicts with rustfmt's
defaults. If you run `cargo fmt`, keep the diff scoped to the lines you're
already changing.

## Commit messages & changelog

The changelog is generated from git history with
[git-cliff](https://git-cliff.org/) (see [cliff.toml](cliff.toml)), so please
use [Conventional Commits](https://www.conventionalcommits.org/) — e.g.
`feat(duckdb): …`, `fix(core): …`, `docs: …`. Don't hand-edit
[CHANGELOG.md](CHANGELOG.md); it's regenerated via `task changelog`.

## Tests

Add or update tests alongside behavioral changes. Backend changes should be
covered by the integration suites in
[crates/duckdb/tests/end_to_end.rs](crates/duckdb/tests/end_to_end.rs) and
[crates/datafusion/tests/end_to_end.rs](crates/datafusion/tests/end_to_end.rs);
keep the request/response behavior identical across both engines.

## Reporting bugs & requesting features

Use the [issue templates](.github/ISSUE_TEMPLATE). For anything
security-related, please follow [SECURITY.md](SECURITY.md) instead of opening a
public issue.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE).
