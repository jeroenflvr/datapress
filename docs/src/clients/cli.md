---
description: >-
  datapress-cli: a standalone command-line client for a DataPress server.
  List datasets, run structured queries, aggregate, count, run SQL, and reload.
---

# Command-line client (`datapress-cli`)

A standalone CLI for talking to a running DataPress server, built on the
[`datapress-client`](rust.md) crate. It is separate from the `datapress`
*server* binary — install both side by side if you want.

## Install

### Install script (Linux / macOS)

```bash
curl -LsSf https://datap-rs.org/install-cli.sh | sh
```

Downloads the prebuilt `datapress-cli` binary for your platform, verifies its
checksum, and installs it into `~/.local/bin` — no `sudo`, and your shell
profile is never edited. If that directory is not on your `PATH`, the script
prints the exact line to add. Override the target directory or version with
`DATAPRESS_CLI_INSTALL_DIR` and `DATAPRESS_CLI_VERSION`.

### Install script (Windows)

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://datap-rs.org/install-cli.ps1 | iex"
```

Installs into `%LOCALAPPDATA%\datapress-cli\bin` and adds it to your user
`PATH`. Open a new terminal afterwards.

### cargo

```bash
cargo install datapress-cli
```

## Connection

Set the target once via environment variables, or pass per-command flags
(flags win):

| Env var                 | Flag             | Default                  |
| ----------------------- | ---------------- | ------------------------ |
| `DATAPRESS_URL`         | `--url`          | `http://127.0.0.1:8000`  |
| `DATAPRESS_TOKEN`       | `--bearer-token` | —                        |
| `DATAPRESS_ADMIN_TOKEN` | `--admin-token`  | —                        |
| —                       | `--api-base`     | `/api/v1`                |
| —                       | `--timeout`      | — (seconds)              |

JSON output is compact (single-line) by default; add `--pretty` to
pretty-print.

## Commands

```bash
# List datasets
datapress-cli datasets

# Schema
datapress-cli schema accidents

# Count (repeatable --where "col:op[:val]")
datapress-cli count accidents --where Severity:gte:3

# Structured query
datapress-cli query accidents \
  --select State,Severity \
  --where Severity:gte:3 \
  --order-by Severity:desc \
  --page-size 1000

# Group-by with aggregation + HAVING
datapress-cli query accidents \
  --group-by State \
  --agg count:n \
  --agg avg:Severity:mean_sev \
  --having n:gt:1000 \
  --order-by n:desc

# Render an ASCII table (fetches via Arrow IPC)
datapress-cli query accidents --select State,Severity --page-size 20 --table

# Save raw Arrow IPC stream to a file ( - for stdout)
datapress-cli query accidents --select State,Severity --arrow-out out.arrow

# Raw SQL (endpoint must be enabled server-side)
datapress-cli sql "SELECT State, COUNT(*) AS n FROM accidents GROUP BY State" --max-rows 100

# Admin
datapress-cli reload accidents --admin-token "$DATAPRESS_ADMIN_TOKEN"

# Probes
datapress-cli health
datapress-cli ready
```

## Mini-syntax

- **`--where` / `--having`**: `col:op[:val]`, e.g. `Severity:gte:3`,
  `State:eq:CA`, `Notes:is_null`. The value is parsed as JSON when possible
  (numbers, booleans, arrays for `in`), otherwise treated as a string.
  Ops: `eq | neq | gt | gte | lt | lte | like | ilike | in | is_null | is_not_null`.
- **`--agg`**: `op:col[:alias]` (e.g. `avg:Severity:mean_sev`) or
  `count[:alias]`. Ops: `count | sum | avg | min | max`.
- **`--order-by`**: `col`, `col:asc`, or `col:desc`.
- **`--select` / `--group-by`**: comma-separated and/or repeatable.
