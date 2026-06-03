//! Micro-benchmark for the equality-index lookup (`store::try_index`).
//!
//! Run with: `cargo bench -p datapress-datafusion --bench index`
//!
//! Every predicate request on a materialised dataset goes through `try_index`.
//! The common case is a single `eq` predicate, which resolves to exactly one
//! index bucket. This bench measures that path against bucket sizes spanning
//! low- to high-cardinality matches, so the cost of returning the matched row
//! ids (borrow vs. clone) is visible.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use datapress_core::models::Predicate;
use datapress_datafusion::store::bench::{lookup, lookup_cloning, single_bucket_index};
use serde_json::json;

fn bench_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_eq_single");
    for &bucket in &[100usize, 10_000, 1_000_000] {
        let val = json!("target");
        let idx = single_bucket_index("name", &val, (0..bucket as u32).collect());
        let preds = vec![Predicate {
            col: "name".to_string(),
            op: "eq".to_string(),
            val: Some(val),
        }];

        group.throughput(Throughput::Elements(bucket as u64));
        // After: single-`eq` borrows the bucket (Cow::Borrowed).
        group.bench_with_input(BenchmarkId::new("borrow", bucket), &bucket, |b, _| {
            b.iter(|| {
                let rows = lookup(black_box(&idx), black_box(&preds));
                black_box(rows.map(|r| r.len()))
            });
        });
        // Before: the bucket is cloned into an owned Vec on every lookup.
        group.bench_with_input(BenchmarkId::new("clone", bucket), &bucket, |b, _| {
            b.iter(|| {
                let rows = lookup_cloning(black_box(&idx), black_box(&preds));
                black_box(rows.map(|r| r.len()))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_index);
criterion_main!(benches);
