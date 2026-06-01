# Python

`datap-rs` is the Python wheel: a maturin-built PyO3 binding that
bundles **both** backends and lets you configure, launch and talk to
a DataPress server from Python.

```bash
pip install datap-rs
# or
uv pip install datap-rs
```

Wheels are published for Linux (x86_64/aarch64), macOS (arm64), and
Windows (x86_64) against CPython 3.9+ (abi3).

## Surface

Six classes, no module-level state:

| Class             | Purpose                                                            |
|-------------------|--------------------------------------------------------------------|
| `DataPressConfig` | Server tuning. See [Server config](config.md).                     |
| `DatasetConfig`   | One dataset.                                                       |
| `S3Config`        | S3 / S3-compatible credentials and endpoint.                       |
| `HMACKeyPair`     | Access/secret key pair returned by an `S3Config` credentials provider. |
| `DataPress`       | Built from a config + datasets. `await .run()`.                    |
| `DataPressClient` | Sync HTTP client (stdlib + lazy `pyarrow`). See [Client](client.md). |

## Pages

- [Configuration](config.md) — `DataPressConfig`, `DatasetConfig`,
  `S3Config`.
- [Running a server](server.md) — `DataPress(...).run()` lifecycle.
- [Client](client.md) — `DataPressClient` reference.
- [Examples](examples.md) — `example.py`, Jupyter recipe.
