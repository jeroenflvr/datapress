# Quick tour

Every endpoint, end-to-end. Replace `accidents` with your dataset name.

## Discovery

```bash
# List configured datasets.
curl -s http://localhost:8080/api/v1/datasets | jq

# Schema + sample row.
curl -s http://localhost:8080/api/v1/datasets/accidents/schema | jq
```

## Querying

```bash
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns":   ["ID", "State", "Severity", "Start_Time"],
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3   }
    ],
    "order_by":  [{ "col": "Severity", "dir": "desc" }],
    "page":      1,
    "page_size": 50
  }' | jq
```

Full DSL reference: [Querying › Request body](../query/request-body.md).

## Counting

```bash
curl -s -X POST http://localhost:8080/api/v1/datasets/accidents/count \
  -H 'Content-Type: application/json' \
  -d '{
    "predicates": [
      { "col": "State", "op": "in", "val": ["TX","CA"] }
    ]
  }' | jq
# → { "count": 2_159_851 }
```

## Arrow IPC for bulk pulls

```bash
curl -X POST 'http://localhost:8080/api/v1/datasets/accidents/query?format=arrow' \
  -H 'Content-Type: application/json' \
  --output page.arrow \
  -d '{ "columns": ["ID","State"], "page_size": 10000 }'
```

Load it in Python:

```python
import pyarrow.ipc as ipc, polars as pl
with open("page.arrow", "rb") as fh:
    table = ipc.open_stream(fh).read_all()
df = pl.from_arrow(table)
```

See [Querying › Arrow IPC vs JSON](../query/arrow-ipc.md) for the
trade-offs.

## Probes

```bash
curl http://localhost:8080/healthz   # always 200
curl http://localhost:8080/readyz    # 200 once a dataset is registered
curl http://localhost:8080/version   # build/version metadata
```

Details in [Operations › Probes](../operations/probes.md).

## Admin: hot reload

Set `ADMIN_TOKEN` in the environment to enable
`POST /api/v1/datasets/{name}/reload`:

```bash
ADMIN_TOKEN=$(openssl rand -hex 32) task run:duckdb &
# ...
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/datasets/accidents/reload | jq
# → { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```

If `ADMIN_TOKEN` is unset, the endpoint returns `403`. See
[Reference › Endpoints](../reference/endpoints.md) for the full table.
