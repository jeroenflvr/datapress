//! Prometheus metrics for dataset refresh and materialization (T5.3, Phase 5).
//!
//! All metrics are registered on the shared [`prometheus::Registry`] that the
//! `actix-web-prom` middleware exposes at the configured `/metrics` path.
//! This module is compiled only when the `metrics` feature is enabled.

use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry};

// ---------------------------------------------------------------------------
// Metric names (T5.3)
// ---------------------------------------------------------------------------
//
// datapress_refresh_total{dataset, trigger, outcome}
//   outcome: ok | failed | timeout | skipped
//   trigger: startup | manual | schedule | cascade
//
// datapress_refresh_duration_seconds{dataset, trigger}   histogram
//
// datapress_dataset_generation{dataset}                  gauge (monotonic publish counter)
//
// datapress_refresh_queue_depth                          gauge
//
// datapress_dataset_rows{dataset}                        gauge
//
// datapress_materialize_spill_total{dataset}             counter  (auto-demotions)
//
// datapress_memory_override_exceeded_total{dataset}      counter  (R2B.1 WARN case)
//
// datapress_dataset_storage_bytes{dataset}               gauge

/// All registered refresh / materialization metrics.
#[derive(Clone)]
pub struct DatapressMetrics {
    pub refresh_total: IntCounterVec,
    pub refresh_duration_seconds: HistogramVec,
    pub dataset_generation: IntGaugeVec,
    pub refresh_queue_depth: IntGaugeVec,
    pub dataset_rows: IntGaugeVec,
    pub materialize_spill_total: IntCounterVec,
    pub memory_override_exceeded_total: IntCounterVec,
    pub dataset_storage_bytes: IntGaugeVec,
}

impl DatapressMetrics {
    /// Register all metrics on `registry`.
    pub fn register(registry: &Registry) -> Result<Self, prometheus::Error> {
        let refresh_total = IntCounterVec::new(
            Opts::new(
                "datapress_refresh_total",
                "Total number of dataset refresh ticks by outcome",
            ),
            &["dataset", "trigger", "outcome"],
        )?;
        registry.register(Box::new(refresh_total.clone()))?;

        let refresh_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "datapress_refresh_duration_seconds",
                "Duration of dataset refresh builds in seconds",
            )
            .buckets(vec![0.05, 0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0]),
            &["dataset", "trigger"],
        )?;
        registry.register(Box::new(refresh_duration_seconds.clone()))?;

        let dataset_generation = IntGaugeVec::new(
            Opts::new(
                "datapress_dataset_generation",
                "Monotonically increasing publish counter per dataset",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(dataset_generation.clone()))?;

        let refresh_queue_depth = IntGaugeVec::new(
            Opts::new(
                "datapress_refresh_queue_depth",
                "Number of datasets currently building or queued",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(refresh_queue_depth.clone()))?;

        let dataset_rows = IntGaugeVec::new(
            Opts::new(
                "datapress_dataset_rows",
                "Current row count of the published generation",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(dataset_rows.clone()))?;

        let materialize_spill_total = IntCounterVec::new(
            Opts::new(
                "datapress_materialize_spill_total",
                "Count of auto-demotion events (result exceeded force_lazy_above_mb)",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(materialize_spill_total.clone()))?;

        let memory_override_exceeded_total = IntCounterVec::new(
            Opts::new(
                "datapress_memory_override_exceeded_total",
                "Count of times a memory-residency dataset exceeded force_lazy_above_mb (R2B.1 WARN)",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(memory_override_exceeded_total.clone()))?;

        let dataset_storage_bytes = IntGaugeVec::new(
            Opts::new(
                "datapress_dataset_storage_bytes",
                "Bytes of the current storage-backed generation",
            ),
            &["dataset"],
        )?;
        registry.register(Box::new(dataset_storage_bytes.clone()))?;

        Ok(Self {
            refresh_total,
            refresh_duration_seconds,
            dataset_generation,
            refresh_queue_depth,
            dataset_rows,
            materialize_spill_total,
            memory_override_exceeded_total,
            dataset_storage_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Public helpers called from the refresh scheduler and backends
// ---------------------------------------------------------------------------

/// Record a completed refresh tick for `dataset`.
///
/// `trigger`:  `"startup"` | `"manual"` | `"schedule"` | `"cascade"`
/// `outcome`:  `"ok"` | `"failed"` | `"timeout"` | `"skipped"`
/// `elapsed_ms`: build wall time in milliseconds (ignored when `outcome != "ok"`)
/// `rows`:     row count of the published generation (0 when not applicable)
pub fn record_refresh(
    metrics: &DatapressMetrics,
    dataset: &str,
    trigger: &str,
    outcome: &str,
    elapsed_ms: u128,
    rows: usize,
) {
    metrics
        .refresh_total
        .with_label_values(&[dataset, trigger, outcome])
        .inc();

    if outcome == "ok" {
        metrics
            .refresh_duration_seconds
            .with_label_values(&[dataset, trigger])
            .observe(elapsed_ms as f64 / 1000.0);
        metrics
            .dataset_generation
            .with_label_values(&[dataset])
            .inc();
        metrics
            .dataset_rows
            .with_label_values(&[dataset])
            .set(rows as i64);
    }
}

/// Update the `datapress_refresh_queue_depth` gauge for `dataset`.
pub fn set_queue_depth(metrics: &DatapressMetrics, dataset: &str, depth: i64) {
    metrics
        .refresh_queue_depth
        .with_label_values(&[dataset])
        .set(depth);
}

/// Increment `datapress_materialize_spill_total` — called when a `residency = auto`
/// build crossed `force_lazy_above_mb` and was demoted to the storage backend (R2B.2).
pub fn record_spill(metrics: &DatapressMetrics, dataset: &str) {
    metrics
        .materialize_spill_total
        .with_label_values(&[dataset])
        .inc();
}

/// Increment `datapress_memory_override_exceeded_total` — called when a
/// `residency = memory` build crossed `force_lazy_above_mb` (R2B.1 WARN case).
pub fn record_memory_override(metrics: &DatapressMetrics, dataset: &str) {
    metrics
        .memory_override_exceeded_total
        .with_label_values(&[dataset])
        .inc();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_register_on_fresh_registry_and_record_refresh() {
        let reg = Registry::new();
        let m = DatapressMetrics::register(&reg).expect("register should succeed");

        // Before any calls, the counter for this label set does not exist yet;
        // with_label_values creates it lazily with value 0.
        let before = m
            .refresh_total
            .with_label_values(&["accidents", "schedule", "ok"])
            .get();
        assert_eq!(before, 0);

        // After a successful refresh the counter increments.
        record_refresh(&m, "accidents", "schedule", "ok", 1500, 42_000);

        let after = m
            .refresh_total
            .with_label_values(&["accidents", "schedule", "ok"])
            .get();
        assert_eq!(after, 1);

        // Row gauge should be updated.
        let rows = m.dataset_rows.with_label_values(&["accidents"]).get();
        assert_eq!(rows, 42_000);

        // Generation counter should have ticked.
        let gen_val = m.dataset_generation.with_label_values(&["accidents"]).get();
        assert_eq!(gen_val, 1);

        // A failed tick increments the failed counter but not duration/gen.
        let gen_before = m.dataset_generation.with_label_values(&["accidents"]).get();
        record_refresh(&m, "accidents", "schedule", "failed", 0, 0);
        let failed = m
            .refresh_total
            .with_label_values(&["accidents", "schedule", "failed"])
            .get();
        assert_eq!(failed, 1);
        // Generation counter unchanged on failure.
        let gen_after = m.dataset_generation.with_label_values(&["accidents"]).get();
        assert_eq!(gen_before, gen_after);
    }

    #[test]
    fn metrics_scrape_contains_all_series() {
        // Verify that after registering + firing a record_refresh,
        // all eight metric family names appear in the registry gather output.
        let reg = Registry::new();
        let m = DatapressMetrics::register(&reg).expect("register should succeed");

        // Prime each family with at least one observation.
        record_refresh(&m, "ds", "startup", "ok", 100, 5);
        set_queue_depth(&m, "ds", 0);
        m.materialize_spill_total.with_label_values(&["ds"]).inc();
        m.memory_override_exceeded_total
            .with_label_values(&["ds"])
            .inc();
        m.dataset_storage_bytes.with_label_values(&["ds"]).set(1024);

        // All expected names must appear in the gather output.
        let expected = [
            "datapress_refresh_total",
            "datapress_refresh_duration_seconds",
            "datapress_dataset_generation",
            "datapress_refresh_queue_depth",
            "datapress_dataset_rows",
            "datapress_materialize_spill_total",
            "datapress_memory_override_exceeded_total",
            "datapress_dataset_storage_bytes",
        ];

        // Use Text format to avoid protobuf API details.
        use prometheus::{Encoder, TextEncoder};
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&reg.gather(), &mut buf)
            .expect("encode");
        let text = String::from_utf8(buf).unwrap();
        for name in &expected {
            assert!(
                text.contains(name),
                "metric '{}' not found in scrape output",
                name
            );
        }
    }

    #[test]
    fn metrics_double_register_fails() {
        let reg = Registry::new();
        DatapressMetrics::register(&reg).expect("first register succeeds");
        let result = DatapressMetrics::register(&reg);
        assert!(result.is_err(), "double-register should fail");
    }
}
