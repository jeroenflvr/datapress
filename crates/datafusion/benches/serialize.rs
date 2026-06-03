//! Micro-benchmark for the JSON row encoder (`datapress_datafusion::serialize`).
//!
//! Run with: `cargo bench -p datapress-datafusion`
//!
//! The encoder is on the hot path of every `/query?format=json` response, so
//! its per-cell cost dominates large pages. This bench builds a mixed-type
//! `RecordBatch` (ints, floats, strings, bools, nulls) and measures end-to-end
//! serialization throughput.

use std::sync::Arc;

use arrow::array::{BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use criterion::{Criterion, criterion_group, criterion_main};
use datapress_datafusion::store::serialize;

fn make_batch(rows: usize) -> RecordBatch {
    let ids: Int64Array = (0..rows as i64).map(Some).collect();
    let scores: Float64Array = (0..rows)
        .map(|i| {
            if i % 7 == 0 {
                None
            } else {
                Some(i as f64 * 1.5)
            }
        })
        .collect();
    let names: StringArray = (0..rows)
        .map(|i| match i % 4 {
            0 => Some("Anna".to_string()),
            1 => Some("Bob \"the builder\"".to_string()),
            2 => Some("Cara\nNewline".to_string()),
            _ => None,
        })
        .collect();
    let flags: BooleanArray = (0..rows).map(|i| Some(i % 2 == 0)).collect();

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("score", DataType::Float64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("flag", DataType::Boolean, false),
    ]));

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(ids),
            Arc::new(scores),
            Arc::new(names),
            Arc::new(flags),
        ],
    )
    .unwrap()
}

fn bench_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialize_json");
    for &rows in &[1_000usize, 10_000, 100_000] {
        let batch = make_batch(rows);
        group.throughput(criterion::Throughput::Elements(rows as u64));
        group.bench_function(format!("{rows}_rows"), |b| {
            b.iter(|| {
                let out = serialize(std::hint::black_box(&batch)).unwrap();
                std::hint::black_box(out);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_serialize);
criterion_main!(benches);
