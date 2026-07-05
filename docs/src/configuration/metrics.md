---
description: >-
  Configure the Prometheus metrics endpoint — enable it, set the scrape path,
  and understand what counters and histograms DataPress exposes.
---

# Prometheus metrics

DataPress can expose an HTTP endpoint in [Prometheus][prom] text format for
request-volume, latency, and status-code tracking.

Full setup and scrape configuration is documented in
[Operations › Prometheus metrics](../operations/metrics.md). This page covers
the `[metrics]` TOML block.

[prom]: https://prometheus.io/

## Build

Metrics are opt-in at compile time:

```bash
cargo build --release -p datapress-duckdb --features docs,swagger,metrics
```

When the binary is built without the `metrics` feature but
`[metrics] enabled = true` is set, the server logs a warning at startup and
skips the endpoint.

## Configuration

```toml
[metrics]
enabled = true       # off by default
path    = "/metrics" # scrape path
```

| Key       | Default      | Notes                                                                                                    |
|-----------|--------------|----------------------------------------------------------------------------------------------------------|
| `enabled` | `false`      | Master switch.                                                                                           |
| `path`    | `"/metrics"` | Scrape path. Served at `{prefix}{path}` — scrape configs must include the configured `server.prefix`. Must start with `/` and not end with `/`. |

The endpoint is **unauthenticated** and sits in front of the `[auth]` layer,
just like the health probes. It exposes only aggregate request counters, never
row data. Isolate it at the network layer (private interface, firewall rule, or
pod-network-only scraping).

## From Python

```python
from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig

config = DataPressConfig(
    backend="duckdb",
    port=8080,
    metrics_enabled=True,
    metrics_path="/metrics",
)
```

## Available metrics

| Metric                                     | Type      | Labels                         |
|--------------------------------------------|-----------|--------------------------------|
| `datapress_http_requests_total`            | counter   | `endpoint`, `method`, `status` |
| `datapress_http_requests_duration_seconds` | histogram | `endpoint`, `method`, `status` |

See [Operations › Prometheus metrics](../operations/metrics.md) for a full
example, scrape config, and Grafana dashboard notes.
