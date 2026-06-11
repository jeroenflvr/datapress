---
description: >-
  Run raw read-only SQL over a single registered dataset with
  POST /api/v1/sql. Parsed and validated before execution, capped row
  counts, single-dataset scope, and identical safety on DuckDB and
  DataFusion.
---

# Raw SQL

The structured [`/query`](request-body.md) endpoint covers projection,
predicates, sorting, grouping, and pagination with a JSON body. When you
need the full expressiveness of SQL — window functions, `CASE`,
sub-selects, arithmetic, string functions — use the raw-SQL endpoint:

```
POST /api/v1/sql
Content-Type: application/json
```

It is **disabled by default**. Raw SQL is a much larger attack surface
than the structured query API, so you opt in explicitly and the server
parses and validates every statement before any engine sees it.

!!! info "Phase 1: one dataset per query"
    Today a statement may reference **exactly one** registered dataset —
    no cross-dataset joins yet. The validation gate is built so that
    raising this limit is the only change needed to allow joins later;
    see [Roadmap](#roadmap).

## Enabling the endpoint

Add a `[sql]` block to your config (see
[Configuration](../configuration/index.md)):

```toml
[sql]
enabled  = true      # default false — endpoint returns 404 when off
max_rows = 100000    # hard cap on rows returned by one query
```

From the Python API, set the equivalent fields on
[`DataPressConfig`](../python/config.md):

```python
from datap_rs.datapress import DataPressConfig

cfg = DataPressConfig(
    backend="datafusion",
    port=8000,
    sql_enabled=True,     # exposes POST /api/v1/sql
    sql_max_rows=100_000,
)
```

| Field      | Default     | Notes                                                                                   |
|------------|-------------|------------------------------------------------------------------------------------------|
| `enabled`  | `false`     | When `false`, the route responds `404` so its presence isn't even revealed.              |
| `max_rows` | `100000`    | Server-side hard cap. The result is wrapped in an outer `LIMIT` so this always applies.  |

While disabled, `POST /api/v1/sql` returns `404 Not Found` — identical to
an unmounted route — so probing for it leaks nothing.

## Request body

```json
{
  "sql": "SELECT state, COUNT(*) AS n FROM accidents GROUP BY state ORDER BY n DESC",
  "max_rows": 500
}
```

| Field      | Type            | Required | Notes                                                                         |
|------------|-----------------|----------|-------------------------------------------------------------------------------|
| `sql`      | string          | yes      | A single read-only `SELECT` / `WITH … SELECT`, or a `DESCRIBE`/`DESC <table>`, referencing one dataset. |
| `max_rows` | integer         | no       | Client row cap. **Clamped** into `[1, [sql].max_rows]`; it can never raise the server cap. Omit to use the server cap. |

The dataset is named directly in the SQL `FROM` clause using its
configured `name` (the same slug used in `/api/v1/datasets/{name}/...`).
Matching is case-insensitive.

## Response

```json
{
  "data": [
    { "state": "CA", "n": 1234 },
    { "state": "TX", "n": 987 }
  ],
  "max_rows": 500
}
```

`data` is the result set as a JSON array of row objects; `max_rows` echoes
the effective row cap that was applied. Column types follow the engine's
inferred output schema.

### Arrow IPC

Like the structured [`/query`](request-body.md) endpoint, the response is
**content-negotiated**. Ask for Arrow and you get an [Arrow IPC
stream](../backends/comparison.md) instead of the JSON envelope — proper
typed columns, no JSON stringification, and the body is streamed as it is
encoded. The same `[sql].max_rows` cap still applies.

Opt in with either the `Accept` header or a `?format=arrow` query param:

```
POST /api/v1/sql?format=arrow
Accept: application/vnd.apache.arrow.stream
```

The response carries `Content-Type: application/vnd.apache.arrow.stream`
and an `X-Max-Rows` header echoing the applied cap.

=== "curl"

    ```bash
    curl -s http://localhost:8080/api/v1/sql \
      -H 'Content-Type: application/json' \
      -H 'Accept: application/vnd.apache.arrow.stream' \
      -d '{"sql": "SELECT state, COUNT(*) AS n FROM accidents GROUP BY state"}' \
      -o result.arrows
    ```

=== "Python (pandas)"

    ```python
    import io
    import requests
    import pyarrow.ipc as ipc

    resp = requests.post(
        "http://localhost:8080/api/v1/sql",
        headers={"Accept": "application/vnd.apache.arrow.stream"},
        json={"sql": "SELECT state, COUNT(*) AS n FROM accidents GROUP BY state"},
    )
    resp.raise_for_status()
    table = ipc.open_stream(io.BytesIO(resp.content)).read_all()
    df = table.to_pandas()
    ```

There is no separate paging for raw SQL: a statement returns a single
result bounded by `max_rows`, so the Arrow stream already delivers the
whole (capped) result in one response.

## Examples

=== "curl"

    ```bash
    curl -s http://localhost:8080/api/v1/sql \
      -H 'Content-Type: application/json' \
      -d '{
        "sql": "SELECT severity, COUNT(*) AS n FROM accidents GROUP BY severity ORDER BY severity",
        "max_rows": 100
      }'
    ```

=== "Python (client)"

    ```python
    from datap_rs import DataPressClient

    c = DataPressClient("http://localhost:8080")
    rows = c.sql(
        "SELECT severity, COUNT(*) AS n "
        "FROM accidents GROUP BY severity ORDER BY severity",
        max_rows=100,
    )
    # rows -> [{"severity": 1, "n": 123}, ...]
    ```

=== "Python (requests)"

    ```python
    import requests

    resp = requests.post(
        "http://localhost:8080/api/v1/sql",
        json={
            "sql": "SELECT severity, COUNT(*) AS n "
                   "FROM accidents GROUP BY severity ORDER BY severity",
            "max_rows": 100,
        },
    )
    resp.raise_for_status()
    data = resp.json()["data"]
    ```

=== "WITH (CTE)"

    A CTE name is local to the query and is **not** treated as a dataset,
    so this still references only `accidents`:

    ```json
    {
      "sql": "WITH bad AS (SELECT * FROM accidents WHERE severity >= 3) SELECT state, COUNT(*) AS n FROM bad GROUP BY state"
    }
    ```

=== "Window function"

    Window functions run over a single dataset. This ranks states by
    accident count without a self-join:

    ```json
    {
      "sql": "SELECT state, COUNT(*) AS n, RANK() OVER (ORDER BY COUNT(*) DESC) AS rnk FROM accidents GROUP BY state ORDER BY rnk LIMIT 10"
    }
    ```

=== "CASE buckets"

    Use `CASE` to derive categories on the fly, then aggregate by them:

    ```json
    {
      "sql": "SELECT CASE WHEN severity >= 3 THEN 'serious' WHEN severity = 2 THEN 'moderate' ELSE 'minor' END AS band, COUNT(*) AS n FROM accidents GROUP BY band ORDER BY n DESC"
    }
    ```

=== "Multiple CTEs"

    Several CTEs can be chained and joined together. None of the CTE names
    (`by_state`, `totals`) count as datasets, so the query still references
    only `accidents`:

    ```json
    {
      "sql": "WITH by_state AS (SELECT state, COUNT(*) AS n FROM accidents GROUP BY state), totals AS (SELECT SUM(n) AS total FROM by_state) SELECT s.state, s.n, ROUND(100.0 * s.n / t.total, 2) AS pct FROM by_state s CROSS JOIN totals t ORDER BY s.n DESC LIMIT 10"
    }
    ```

=== "CTE + window"

    A CTE feeds a window function to keep, per state, only the worst
    severity rows above the state's own average:

    ```json
    {
      "sql": "WITH ranked AS (SELECT state, severity, AVG(severity) OVER (PARTITION BY state) AS avg_sev FROM accidents) SELECT state, severity, ROUND(avg_sev, 2) AS avg_sev FROM ranked WHERE severity > avg_sev ORDER BY state, severity DESC LIMIT 50"
    }
    ```

=== "Scalar expressions"

    ```json
    { "sql": "SELECT 1 + 1 AS two, upper('datapress') AS name" }
    ```

    Table-less queries reference zero datasets and are always allowed.

=== "DESCRIBE"

    Inspect a dataset's columns and types. `DESCRIBE` (and its `DESC`
    alias) is allowed and subject to the same single-dataset allowlist as
    a query:

    ```json
    { "sql": "DESCRIBE accidents" }
    ```

    The result is one row per column (`column_name`, `column_type`, …).

## What is rejected

The shared validation gate runs identically for the DuckDB and DataFusion
backends. A request is rejected with `400 Bad Request` when the statement:

- is **not** a single read-only statement — multiple statements, or
  anything other than `SELECT` / `WITH … SELECT` / `DESCRIBE` / `DESC`
  (no `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `DROP`, `ALTER`, `COPY`,
  `ATTACH`, `INSTALL`, `PRAGMA`, `EXPLAIN`, …);
- references an **unknown table** — every relation must be a registered
  dataset (or a CTE defined in the same query);
- references **more than one** dataset (Phase 1 limit);
- uses a **file-reading or external-access function** in any position —
  `read_parquet`, `read_csv`, `read_json`, `read_text`, `read_blob`,
  `glob`, `parquet_scan`, and similar are denied even in scalar position
  (e.g. `SELECT read_text('/etc/passwd')`).

```json
// 400 — DML is not allowed
{ "error": "only read-only SELECT and DESCRIBE statements are allowed" }

// 400 — more than one statement
{ "error": "exactly one SQL statement is allowed" }

// 400 — unknown / file-function table
{ "error": "could not parse SQL: ..." }

// 400 — too many datasets (Phase 1)
{ "error": "this endpoint allows at most 1 dataset(s) per query; the statement references 2" }
```

See [Reference › Errors](../reference/errors.md) for the full status-code
table.

## Security model

- **Off by default.** No `[sql]` block, no endpoint.
- **Parse-then-allowlist.** Statements are parsed with `sqlparser` and
  every referenced relation is checked against the set of registered
  datasets *before* execution — the engine never sees an unvalidated
  string.
- **No file access.** File-reading table and scalar functions are denied,
  so a query can't escape the configured datasets to read arbitrary paths
  or URLs.
- **Bounded results.** Every query is wrapped in an outer `LIMIT`
  (`[sql].max_rows`), so a runaway `SELECT` can't stream unbounded rows.
- **Read scopes apply.** When [authentication](../operations/auth.md) is
  enabled, the endpoint enforces the same `read` scopes as the structured
  query API.

The legacy un-versioned alias `POST /api/sql` is also mounted and behaves
identically.

## Roadmap

The validation gate already tracks **which** datasets a statement touches
and enforces a configurable maximum (Phase 1 passes `1`). Cross-dataset
joins become available by raising that bound — the allowlist, file-function
denial, single-statement, and read-only guarantees all stay in force. No
isolated per-dataset connections are used, so a multi-dataset `JOIN` is an
additive change rather than a rewrite.
