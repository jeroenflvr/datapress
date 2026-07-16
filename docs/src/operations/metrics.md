# Prometheus metrics

DataPress can expose an HTTP endpoint in [Prometheus][prom] text format
so a scraper can track request volume, latency, and status-code
distribution. Metrics are produced by the
[`actix-web-prom`][actix-web-prom] middleware and cover every HTTP
request the server handles.

The endpoint is **unauthenticated** and sits in front of the auth layer,
just like the health probes — scrapers rarely carry bearer tokens, and
the endpoint exposes only aggregate request counters, never row data.
Isolate it at the network layer (bind to a private interface, restrict
via firewall / NetworkPolicy, or scrape over the pod network only).

[prom]: https://prometheus.io/
[actix-web-prom]: https://crates.io/crates/actix-web-prom

## Build

Metrics are opt-in at compile time so binaries without them stay slim:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,metrics
```

When the binary is built without `metrics` but `[metrics] enabled = true`
in the TOML, the server logs a warning at startup and skips the endpoint.

## Configuration

```toml
[metrics]
enabled = true
path    = "/metrics"
```

| Key       | Default      | Notes                                                          |
|-----------|--------------|----------------------------------------------------------------|
| `enabled` | `false`      | Master switch. When false the endpoint is not served.          |
| `path`    | `"/metrics"` | Endpoint path. Must start with `/` and not end with `/`. Served at `{prefix}{path}` — include the configured `server.prefix` in your scrape config. |

The `path` must not collide with reserved mounts (`/`, `/api`, `/health`,
`/healthz`, `/readyz`, `/version`) or with the `docs` / `swagger` paths;
startup validation rejects the config otherwise.

## From Python

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

config = DataPressConfig(
    backend="duckdb",
    listen="0.0.0.0",
    port=8080,
    metrics_enabled=True,
    metrics_path="/metrics",
)
```

This requires a wheel built with the `metrics` feature.

## What it exposes

Metric names are prefixed with the `datapress` namespace:

### HTTP request metrics (actix-web-prom)

| Metric                                      | Type      | Labels                          |
|---------------------------------------------|-----------|---------------------------------|
| `datapress_http_requests_total`             | counter   | `endpoint`, `method`, `status`  |
| `datapress_http_requests_duration_seconds`  | histogram | `endpoint`, `method`, `status`  |

### Dataset refresh metrics (Phase 5 / T5.3)

These metrics are registered on the same Prometheus registry as the HTTP metrics:

| Metric                                              | Type      | Labels                          | Description |
|-----------------------------------------------------|-----------|---------------------------------|-------------|
| `datapress_refresh_total`                           | counter   | `dataset`, `trigger`, `outcome` | Refresh ticks. `outcome`: `ok`, `failed`, `timeout`, `skipped`. `trigger`: `startup`, `manual`, `schedule`, `cascade`. |
| `datapress_refresh_duration_seconds`                | histogram | `dataset`, `trigger`            | Build wall time in seconds (on `ok` outcomes only). |
| `datapress_dataset_generation`                      | gauge     | `dataset`                       | Monotonically increasing publish counter. |
| `datapress_refresh_queue_depth`                     | gauge     | `dataset`                       | In-progress or queued builds for a dataset. |
| `datapress_dataset_rows`                            | gauge     | `dataset`                       | Row count of the current published generation. |
| `datapress_materialize_spill_total`                 | counter   | `dataset`                       | Auto-demotion events (result exceeded `force_lazy_above_mb`). |
| `datapress_memory_override_exceeded_total`          | counter   | `dataset`                       | Times a `memory`-residency dataset exceeded `force_lazy_above_mb`. |
| `datapress_dataset_storage_bytes`                   | gauge     | `dataset`                       | Bytes of the current storage-backed generation. |

```bash
curl -s http://localhost:8080/metrics | grep datapress_refresh
# # HELP datapress_refresh_total Total number of dataset refresh ticks by outcome
# # TYPE datapress_refresh_total counter
# datapress_refresh_total{dataset="accidents",outcome="ok",trigger="schedule"} 12
# datapress_refresh_total{dataset="accidents",outcome="skipped",trigger="schedule"} 1
```

All workers share a single registry, so counts aggregate across the
worker pool.

## Scrape config

```yaml
scrape_configs:
  - job_name: datapress
    metrics_path: /metrics
    static_configs:
      - targets: ["datapress.internal:8080"]
```
