# Graceful shutdown

DataPress catches `SIGTERM` and `SIGINT` (Ctrl+C) and drains in-flight
requests before exiting. The grace period is configurable.

## How it works

1. The signal handler closes the listening socket — no new
   connections.
2. Already-accepted requests get up to `shutdown_timeout_secs`
   seconds to finish.
3. After the grace period (or once the last in-flight request
   completes), workers are stopped and the process exits cleanly.

The startup log records which signal triggered shutdown:

```
INFO  Received SIGTERM, shutting down gracefully (up to 30s for in-flight requests)...
INFO  Shutdown complete.
```

On non-Unix platforms (Windows), only Ctrl+C is honoured — `SIGTERM`
isn't a thing there.

## Configure the grace period

=== "TOML"

    ```toml
    [server]
    shutdown_timeout_secs = 30    # default
    ```

=== "Python"

    ```python
    DataPressConfig(backend="datafusion", port=8000,
                    shutdown_timeout_secs=30)
    ```

Choose the value based on your workload:

- **Short** (`5`–`10` s) — interactive UI workloads where every
  handler returns quickly. Faster pod restarts during rolling
  deploys.
- **Default** (`30` s) — bulk queries, medium aggregations.
- **Long** (`120`+ s) — large parquet exports, expensive
  aggregations on big datasets. Match this to the longest realistic
  handler duration.

## Tuning with the orchestrator

The orchestrator's "terminationGracePeriodSeconds" must be **strictly
greater** than `shutdown_timeout_secs`, otherwise the pod gets
`SIGKILL`'d before DataPress can finish draining.

```yaml
spec:
  terminationGracePeriodSeconds: 45    # > shutdown_timeout_secs (30)
  containers:
    - name: datapress
      # ...
      env:
        - name: RUST_LOG
          value: info
```

For zero-downtime rolling deploys, also ensure:

- The readiness probe (`/readyz`) is wired up so the load balancer
  stops sending new traffic the moment a pod begins shutting down.
- The new pod is fully ready before the old one is torn down.
