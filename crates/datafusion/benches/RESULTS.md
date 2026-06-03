# Benchmark results — `serialize` (JSON row encoder)

Tracked results for the `datapress_datafusion::store::serialize` hot path.
Criterion's raw data lives in `target/criterion/` (gitignored / local-only),
so this file is the portable, committed record. Update it whenever you save a
new baseline.

## How to run

```sh
EMSDK_QUIET=1 cargo bench -p datapress-datafusion --bench serialize
```

Save / compare against a named baseline:

```sh
# save
cargo bench -p datapress-datafusion --bench serialize -- --save-baseline <name>
# compare a later run against it (prints % change + p-value)
cargo bench -p datapress-datafusion --bench serialize -- --baseline <name>
```

## Workload

- 4-column mixed `RecordBatch`: `Int64`, `Float64` (with nulls), `Utf8`
  (with `"` / `\n` escape cases), `Boolean`.
- Sizes: 1,000 / 10,000 / 100,000 rows.
- Release build.

## Baseline: `opt-2025-06`

Machine: Apple M1, macOS. After the per-column `ColEnc` dispatch +
`ahash`-backed equality index. Median of 100 samples:

| rows    | time      | throughput      |
|---------|-----------|-----------------|
| 1,000   | ~95.3 µs  | ~10.5 Melem/s   |
| 10,000  | ~924.9 µs | ~10.8 Melem/s   |
| 100,000 | ~9.13 ms  | ~10.9 Melem/s   |

Throughput is flat across sizes (~10.5–10.9 Melem/s), confirming the
per-cell `downcast_ref` cost was removed (it no longer grows with row count).

## Before/after — per-column `ColEnc` dispatch

The pre-optimization code (`git` HEAD, per-cell `data_type()` match +
`downcast_ref` for every cell) was captured as the `before` baseline by
reverting `serialize`, then the optimized version was compared against it
(`--baseline before`). Same machine, same workload, median of 100 samples:

| rows    | before (`time`) | after (`time`) | before (thrpt) | after (thrpt) | change   |
|---------|-----------------|----------------|----------------|---------------|----------|
| 1,000   | ~117.0 µs       | ~97.1 µs       | ~8.55 Melem/s  | ~10.30 Melem/s | **−18.3% time / +22.4% thrpt** |
| 10,000  | ~1.136 ms       | ~933.9 µs      | ~8.80 Melem/s  | ~10.71 Melem/s | **−17.8% time / +21.6% thrpt** |
| 100,000 | ~11.27 ms       | ~9.17 ms       | ~8.88 Melem/s  | ~10.91 Melem/s | **−18.6% time / +22.9% thrpt** |

All three deltas are statistically significant (criterion `p = 0.00 < 0.05`,
"Performance has improved"). The win is consistent across batch sizes,
confirming it's the per-cell dynamic dispatch — not a fixed setup cost — that
was eliminated.

### Reading the numbers

- **time** — wall-clock to serialize the whole batch once (median of 100
  samples). Lower is better.
- **throughput (Melem/s)** — *millions of elements per second*, where one
  "element" is one row (criterion is told the row count via
  `Throughput::Elements`). So `10.9 Melem/s` ≈ 10.9 million rows/s, i.e.
  ~92 ns per row. Higher is better. Because it normalises out the batch
  size, it's the value to watch when comparing runs at different row counts —
  a flat throughput means cost scales linearly with rows (good); a falling
  throughput as rows grow signals super-linear overhead.

> Note: this bench was introduced alongside the optimization. To capture the
> pre-optimization numbers (the `before` column above), the `serialize` change
> was reverted (`git restore`), benched with `--save-baseline before`, then the
> optimized file was restored and run with `--baseline before`.
