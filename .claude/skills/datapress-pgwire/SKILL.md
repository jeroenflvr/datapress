---
name: datapress-pgwire
description: Repo conventions, verified facts, and token-economy rules for implementing PostgreSQL wire-protocol (pgwire) support in the jeroenflvr/datapress Rust workspace using datafusion-postgres. Use this skill whenever working on datapress pgwire integration, the datapress-datafusion crate, PgwireConfig, serve_pgwire, or BI-tool (Power BI/psql/Npgsql) connectivity for DataPress — read it BEFORE opening any repo file.
---

# DataPress pgwire integration

## Verified facts — trust these, do not re-derive

- Workspace pins: `datafusion = "53"`, `arrow = "58"`, `tokio = "1.52.3"`,
  actix-web 4.12, edition 2024. Deps are centralized in root
  `[workspace.dependencies]`; member crates use `foo.workspace = true`.
- `datafusion-postgres 0.17.0` requires `datafusion ^53`, `pgwire ^0.40`,
  `tokio ^1.52` → drop-in compatible. Earlier versions (≤0.15) need
  datafusion 52 — do not use them.
- Crates: `core` (config, Backend trait, HTTP handlers, server), `datafusion`,
  `duckdb`, `datapress` (dispatcher bin), `python` (pyo3), `client*`.
- `crates/datafusion/src/store.rs` (~3.4k lines): `Store` holds ONE shared
  `SessionContext` (private field `ctx`); every dataset is
  `ctx.register_table()`-ed at load; `Store::reload` swaps providers via
  `deregister_table` + `register_table` — a cloned SessionContext therefore
  always sees current tables. `SessionContext::clone()` is cheap and shares state.
- Datasets land in DataFusion's default catalog/schema (`public` to pg clients).
- Existing compat hook: `register_compat_udfs()` in store.rs (adds
  `current_schema()`); the natural home for future pg-dialect shims.
- Precedent for an optional second protocol listener: `[server.quack]` /
  `QuackConfig` in `crates/core/src/config.rs` (struct, defaults,
  `validate_enabled()`, called from `AppConfig` validation). Mirror it.
- Feature-gating convention: `docs`/`swagger`/`metrics`/`auth`/`explorer` are
  cargo features mapping to `dep:` optionals; compiled-out + config-enabled →
  runtime `log::warn!`, not an error.
- Entry points to modify: `crates/datafusion/src/lib.rs` — `serve()` and
  `serve_with_shutdown()` (the latter is the Python-embedding path; its
  shutdown future must also stop the pgwire task).
- **Feature forwarding is manual**: cargo features do NOT propagate upward.
  A feature on `datapress-datafusion` is invisible to the `datapress`
  dispatcher crate and the `crates/python` wheel until each declares its own
  `pgwire = ["datapress-datafusion/pgwire"]` forward. `task build:cli`
  failing with "package does not contain this feature" means a missing
  forward.
- **Config-struct changes ripple regardless of features**: adding a field to
  `ServerConfig`/`AppConfig` in `datapress-core` breaks every crate that
  constructs the struct literally (notably `crates/python`), feature flags
  notwithstanding. Grep `ServerConfig {` workspace-wide before calling a
  change backend-only; prefer `..Default::default()` at construction sites.
- **Ship-everywhere policy**: pgwire is part of the published feature set
  (`docs,swagger,auth,metrics,explorer,pgwire`) in ALL artifacts — unified
  CLI (`build:cli`), static release binaries (`build:static:linux`), and the
  Python wheel (`py:develop`/`py:build`) — and in the CI publish workflows
  if they spell out features directly. Do not scope new listener features to
  the standalone binary only.

## Token-economy rules (binding)

1. **Never read `store.rs` whole.** Use grep for symbols
   (`pub struct Store`, `register_compat_udfs`, `session_context`) and view
   ±30 lines around hits. Same for `config.rs` (search `QuackConfig`).
2. **Read the datafusion-postgres 0.17 API once, then write code once.**
   Fetch docs.rs / the repo README example for 0.17 specifically before
   writing `pgwire.rs`. Its builder API shifts between minors; coding from
   memory causes expensive compile-fix loops.
3. **Check, don't build.** Iterate with
   `cargo check -p datapress-datafusion --features pgwire`. NEVER run a
   workspace-wide build or test: the duckdb crate compiles bundled DuckDB
   from C++ source and takes many minutes of CPU for zero signal here.
   `-p datapress-datafusion` avoids it entirely.
4. Run the e2e test with `cargo test -p datapress-datafusion --features pgwire
   --test pgwire_e2e` — not the whole suite — until final verification.
5. First `cargo check` downloads/compiles the dep tree (minutes). Run it once
   early (right after the Cargo.toml edits) in the background if possible,
   so later checks are incremental.
6. Don't reformat or "improve" untouched code; keep diffs surgical. Match the
   existing comment style (explanatory, why-not-what) but don't over-comment.
7. Consult existing files as templates instead of inventing shapes:
   `QuackConfig` for config, `tests/end_to_end.rs` for test fixtures,
   the `docs`/`auth` features for gating.

## Design invariants

- pgwire is an additional front door to the SAME SessionContext the SQL HTTP
  endpoint uses. Pass `store.session_context().clone()` — never a new context.
- pgwire queries bypass the eq-index/ArcSwap fast path by design (they go
  through DataFusion SQL). Do not try to route them through it.
- Off by default; zero cost when the feature is compiled out; loopback-only
  unless a password is configured.
- TLS is required for the target client, not a nice-to-have: Power BI's
  PostgreSQL connector (Npgsql) defaults to "Encrypt connection = on" and
  fails against plaintext-only servers out of the box. Ship and test the TLS
  path; treat plaintext as the dev-only mode. A no-TLS server must still
  refuse the SSLRequest probe with `N` (no hang).
- Rely on datafusion-postgres's built-in pg_catalog emulation. Gaps found with
  real BI clients are follow-ups (upstream PRs or `register_compat_udfs`
  shims), not scope creep in this task.

## Definition of done

See `instructions.md` acceptance criteria. Summary: feature-gated compile
clean both with and without the feature, clippy clean, e2e test over
tokio-postgres passes (simple + extended/prepared path), config validation
covered, README/CONFIG.md/Taskfile/CI updated minimally.