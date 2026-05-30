# DataPress

<img class="datapress-ascii-logo" src="assets/images/datapress-logo.svg" alt="DataPress ASCII wordmark">

A Rust **Cargo workspace** that exposes one or more **Parquet / Delta
datasets** over a JSON HTTP API. The same surface area is implemented
twice — once on top of **DuckDB**, once on top of **Apache Arrow +
DataFusion** — so you can A/B the engines under identical workloads.
A Python wheel (`datap-rs`, built with maturin + PyO3) bundles both
engines and lets you configure and launch the server from Python.

!!! tip "Two backends, one API"
    The HTTP request and response shapes are byte-for-byte identical
    across backends. Pick the engine via config, A/B-test, swap
    transparently.

## Highlights

- Built on [actix-web](https://actix.rs/) 4
- Datasets declared in a single TOML config (Rust binaries) or
  programmatically (Python wrapper)
- Dynamic schema inference at startup — no hard-coded columns
- JSON or **Arrow IPC** response formats on the same `/query` route
- Versioned API (`/api/v1/...`) with a legacy un-versioned alias
- Graceful shutdown on `SIGTERM` / `SIGINT`
- `/healthz`, `/readyz`, and `/version` probes for orchestrators
- Optional bundled documentation site (this one) served from the binary

## Where to go next

<div class="grid cards" markdown>

- **[Getting started](getting-started/index.md)**
  Install, edit `datasets.toml`, run a backend, hit it with `curl`.

- **[Configuration](configuration/index.md)**
  Every TOML field, with copy-pasteable examples.

- **[Querying](query/index.md)**
  The full predicate / aggregation / pagination DSL.

- **[Backends](backends/index.md)**
  When to pick DuckDB vs Arrow + DataFusion.

- **[Python](python/index.md)**
  `pip install datap-rs`, run the same server from Python.

- **[Operations](operations/index.md)**
  Probes, shutdown, logging, deployment.

</div>
