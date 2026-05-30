# Dataset reload

`POST /api/v1/datasets/{name}/reload` rebuilds a configured dataset from
its existing `source` and publishes the new contents without a service
restart. The endpoint is admin-only: it requires `X-Admin-Token` to match
`ADMIN_TOKEN`, or a bearer token with the configured reload scope when
OIDC auth is enabled.

```bash
curl -s -X POST \
  -H "X-Admin-Token: $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/datasets/accidents/reload | jq
# { "dataset": "accidents", "rows": 7728394, "elapsed_ms": 1842 }
```

Reloads are serialized per dataset name, so two reloads of `accidents`
queue behind each other. Reloads of different datasets may run in
parallel. If a reload fails, the previously published dataset stays live.

## DataFusion backend

For materialized DataFusion datasets, reload uses a service-level
double-buffer:

1. The backend reads the dataset source and builds a fresh `DatasetState`
   off to the side, including Arrow `RecordBatch` chunks and any equality
   indexes.
2. The new provider is registered in the shared `SessionContext`.
3. An `ArcSwap` publication step swaps the dataset snapshot map.
4. Requests that already captured the old `Arc<DatasetState>` keep
   running against it; new requests see the new state.
5. The old Arrow buffers are freed once the last in-flight request drops
   its reference.

This gives zero-downtime publication and failure safety, but it has a
memory trade-off: while reload is building, the old and new copies of the
dataset coexist. For large materialized datasets, peak RSS can approach
roughly twice the dataset size plus index overhead. Lazy DataFusion
datasets avoid resident Arrow buffers, but still re-register their table
provider and publish new metadata through the same snapshot mechanism.

## DuckDB backend

DuckDB does not need DataPress to hold a second full Arrow copy of the
dataset. Reload is delegated to DuckDB as an ACID transaction with:

```sql
CREATE OR REPLACE TABLE dataset AS SELECT * FROM read_parquet(...);
```

or the equivalent scan for Delta/S3 sources. DuckDB executes that as a
transactional catalog/table replacement: if the source read or table
creation fails, the existing table remains available; if it succeeds, the
replacement becomes visible atomically to later queries. In-flight
queries continue against the snapshot they started with, using DuckDB's
own transaction and MVCC semantics.

After DuckDB publishes the replacement table, DataPress refreshes the
cached schema and row count used by `/schema` and `/api/v1/datasets`.
Those metadata maps are small and are swapped under short-lived Rust
locks. The heavy data path is handled by DuckDB rather than by a
DataPress-owned double buffer.

The practical result is similar at the HTTP API level: clients either
see the old dataset or the new dataset, never a partially loaded one. The
resource profile differs: DuckDB relies on its engine and buffer manager,
whereas materialized DataFusion temporarily keeps old and new Arrow
resident data in process memory.
