# datapress-cli

Command-line client for a [DataPress](https://github.com/jeroenflvr/datapress)
dataset server, built on [`datapress-client`](../client).

## Install

```sh
cargo install datapress-cli
```

## Connection

Set once via environment, or pass per-command flags:

| Env var                 | Flag             | Default                  |
| ----------------------- | ---------------- | ------------------------ |
| `DATAPRESS_URL`         | `--url`          | `http://127.0.0.1:8000`  |
| `DATAPRESS_TOKEN`       | `--bearer-token` | —                        |
| `DATAPRESS_ADMIN_TOKEN` | `--admin-token`  | —                        |
| —                       | `--api-base`     | `/api/v1`                |
| —                       | `--timeout`      | — (seconds)              |

Add `--pretty` to pretty-print JSON (default is compact, single-line).

## Commands

```sh
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

### Mini-syntax

- **`--where` / `--having`**: `col:op[:val]`, e.g. `Severity:gte:3`,
  `State:eq:CA`, `Notes:is_null`. The value is parsed as JSON when
  possible (numbers, booleans, arrays for `in`), else treated as a string.
  Ops: `eq | neq | gt | gte | lt | lte | like | ilike | in | is_null | is_not_null`.
- **`--agg`**: `op:col[:alias]` (e.g. `avg:Severity:mean_sev`) or
  `count[:alias]`. Ops: `count | sum | avg | min | max`.
- **`--order-by`**: `col`, `col:asc`, or `col:desc`.
- **`--select` / `--group-by`**: comma-separated and/or repeatable.

## License

MIT
