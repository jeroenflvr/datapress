# Probes

Four endpoints answer "is the server alive and ready?". The
unprefixed three sit at the bare host root, independent of the
configured URL prefix.

| Method | Path                  | Code      | Body                                                       | Use                                     |
|--------|-----------------------|-----------|------------------------------------------------------------|-----------------------------------------|
| GET    | `/healthz`            | `200`     | `{"status":"ok"}`                                          | Kubernetes liveness probe.              |
| GET    | `/readyz`             | `200`/`503` | `{"status":"ready","datasets":N}` / `{"status":"not_ready","datasets":0}` | Kubernetes readiness probe.             |
| GET    | `/version`            | `200`     | Build metadata (see below)                                 | Manual / dashboard build identification.|
| GET    | `{prefix}/health`     | `200`     | `{"status":"ok"}`                                          | Legacy alias, honours `prefix`.         |

## `/healthz` — liveness

Returns `200` immediately. The handler does no work — it answers only
"is this process responsive?". Pair with a Kubernetes
`livenessProbe` that restarts the pod if the response stops arriving.

```bash
curl -s http://localhost:8080/healthz
# {"status":"ok"}
```

## `/readyz` — readiness

Returns `200` once at least one dataset is registered with the
backend:

```bash
curl -s http://localhost:8080/readyz
# {"status":"ready","datasets":3}
```

While dataset materialisation is still in progress (or if every
dataset failed to load), the same handler returns `503`:

```bash
curl -i http://localhost:8080/readyz
# HTTP/1.1 503 Service Unavailable
# {"status":"not_ready","datasets":0}
```

Use this from a Kubernetes `readinessProbe` so traffic is held back
until the server actually has data to serve.

## `/version` — build metadata

```bash
curl -s http://localhost:8080/version | jq
```

```json
{
  "name":       "datapress-core",
  "version":    "0.1.17",
  "backend":    "DataFusion",
  "git_sha":    "a1b2c3d4",
  "build_time": "2025-01-15T14:32:09Z",
  "profile":    "release",
  "target":     "x86_64-unknown-linux-gnu"
}
```

| Field        | Source                                          |
|--------------|-------------------------------------------------|
| `name`       | `CARGO_PKG_NAME`                                |
| `version`    | `CARGO_PKG_VERSION`                             |
| `backend`    | `"DuckDB"` / `"DataFusion"` / `"unknown"`       |
| `git_sha`    | `DATAPRESS_GIT_SHA` env var at build time (opt) |
| `build_time` | `DATAPRESS_BUILD_TIME` env var at build time (opt) |
| `profile`    | `debug` / `release` based on `cfg!(debug_assertions)` |
| `target`     | `DATAPRESS_TARGET` env var at build time (opt)  |

Optional fields are omitted from the JSON when not set, so the
response stays compact in a no-CI dev build.

To populate the optional fields in CI:

```bash
export DATAPRESS_GIT_SHA="$(git rev-parse --short HEAD)"
export DATAPRESS_BUILD_TIME="$(date -u +%FT%TZ)"
export DATAPRESS_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo build --release
```

## Kubernetes example

```yaml
livenessProbe:
  httpGet: { path: /healthz, port: http }
  periodSeconds: 10
  failureThreshold: 3

readinessProbe:
  httpGet: { path: /readyz, port: http }
  periodSeconds: 5
  failureThreshold: 2
  timeoutSeconds: 2
```
