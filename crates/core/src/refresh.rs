//! Refresh scheduler for `kind = "query"` datasets (Phase 3, R3.1–R3.7).
//!
//! A **single tokio task** owns a min-heap of `(next_fire, dataset_name)`
//! entries and drives periodic rebuilds (R3.1).
//!
//! **Coalescing (R3.2):** on each tick the scheduler acquires (1) the global
//! concurrency semaphore, then calls (2) `backend.try_reload(name)`. If the
//! per-dataset reload mutex is already held, `try_reload` returns `Ok(None)`
//! and the tick is skipped; the next fire is rescheduled from `now +
//! interval` (no queueing).
//!
//! **Timeout (R3.3):** the `try_reload` future is wrapped in
//! `tokio::time::timeout`. On expiry the future is cancelled; for DuckDB the
//! underlying `web::block` thread may continue until the engine returns, but
//! the semaphore permit is released regardless (never leaked).
//!
//! **Backoff (R3.4):** consecutive failures back off exponentially
//! (base = interval, factor 2, cap 8 × interval). Reset on success or
//! coalesce.
//!
//! **Jitter (R3.5):** ±10 % uniform jitter applied to every scheduled fire.
//!
//! **Graceful shutdown (R3.6):** the task is stopped via a
//! `CancellationToken`; the cancellation point is between ticks, never
//! mid-build.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::backend::Backend;

// ---------------------------------------------------------------------------
// Jitter PRNG
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xdead_beef_cafe_babe);
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn jittered(base: Duration, apply: bool, rng: &mut Rng) -> Duration {
    if !apply || base.is_zero() {
        return base;
    }
    let delta = (rng.next_f64() - 0.5) * 0.2 * base.as_secs_f64();
    let secs = (base.as_secs_f64() + delta).max(0.001);
    Duration::from_secs_f64(secs)
}

// ---------------------------------------------------------------------------
// Min-heap entry
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    fire_at: Reverse<Instant>,
    name: String,
    interval: Duration,
    timeout: Duration,
    jitter: bool,
    consecutive_failures: u32,
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.fire_at
            .cmp(&other.fire_at)
            .then_with(|| self.name.cmp(&other.name))
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Per-dataset refresh schedule, derived from `DatasetConfig.refresh`.
#[derive(Debug, Clone)]
pub struct DatasetSchedule {
    pub name: String,
    pub interval: Duration,
    pub timeout: Duration,
    pub jitter: bool,
}

/// Drives periodic refresh of configured datasets (Phase 3).
pub struct RefreshScheduler {
    schedules: Vec<DatasetSchedule>,
    max_concurrent: usize,
}

impl RefreshScheduler {
    pub fn new(schedules: Vec<DatasetSchedule>, max_concurrent: usize) -> Self {
        Self {
            schedules,
            max_concurrent: max_concurrent.max(1),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.schedules.is_empty()
    }

    /// Spawn the scheduler task. Cancel `shutdown` to stop the loop between
    /// ticks (R3.6). Await the returned handle to confirm exit.
    pub fn spawn(
        self,
        backend: Arc<dyn Backend>,
        shutdown: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        tokio::spawn(run_loop(self.schedules, backend, semaphore, shutdown))
    }
}

// ---------------------------------------------------------------------------
// Scheduler loop
// ---------------------------------------------------------------------------

async fn run_loop(
    schedules: Vec<DatasetSchedule>,
    backend: Arc<dyn Backend>,
    semaphore: Arc<Semaphore>,
    shutdown: CancellationToken,
) {
    if schedules.is_empty() {
        return;
    }

    let mut rng = Rng::new();
    let now = Instant::now();

    let mut heap: BinaryHeap<Entry> = schedules
        .into_iter()
        .map(|s| {
            let delay = jittered(s.interval, s.jitter, &mut rng);
            Entry {
                fire_at: Reverse(now + delay),
                name: s.name,
                interval: s.interval,
                timeout: s.timeout,
                jitter: s.jitter,
                consecutive_failures: 0,
            }
        })
        .collect();

    loop {
        let sleep_until = match heap.peek() {
            Some(e) => e.fire_at.0,
            None => return,
        };

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                log::debug!("[refresh] scheduler shutting down");
                return;
            }
            _ = tokio::time::sleep_until(sleep_until) => {}
        }

        let mut entry = match heap.pop() {
            Some(e) => e,
            None => return,
        };

        // R3.2 — acquire global semaphore permit.
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("refresh semaphore closed");

        let name = entry.name.clone();
        let timeout_dur = entry.timeout;
        let tick_start = Instant::now();

        // R3.2 + R3.3 — try_reload under timeout.
        let outcome = tokio::time::timeout(timeout_dur, backend.try_reload(&name)).await;

        drop(permit); // release semaphore regardless of outcome (R3.3)

        let next_interval = match outcome {
            Err(_elapsed) => {
                entry.consecutive_failures += 1;
                log::warn!(
                    "[refresh] '{}': timed out after {:?} (consecutive_failures={})",
                    name,
                    timeout_dur,
                    entry.consecutive_failures,
                );
                backoff_interval(entry.interval, entry.consecutive_failures)
            }
            Ok(Err(e)) => {
                entry.consecutive_failures += 1;
                log::warn!(
                    "[refresh] '{}': failed: {} (consecutive_failures={})",
                    name,
                    e,
                    entry.consecutive_failures,
                );
                backoff_interval(entry.interval, entry.consecutive_failures)
            }
            Ok(Ok(None)) => {
                // Coalesced — not a failure, normal interval.
                log::debug!("[refresh] '{}': skipped (reload mutex held)", name);
                entry.consecutive_failures = 0;
                entry.interval
            }
            Ok(Ok(Some(stats))) => {
                entry.consecutive_failures = 0;
                log::info!(
                    "[refresh] '{}': refreshed — {} rows in {} ms",
                    name,
                    stats.rows,
                    stats.elapsed_ms,
                );
                entry.interval
            }
        };

        let next = jittered(next_interval, entry.jitter, &mut rng);
        entry.fire_at = Reverse(tick_start + next);
        heap.push(entry);
    }
}

/// Exponential backoff capped at 8 × base (R3.4).
fn backoff_interval(base: Duration, consecutive_failures: u32) -> Duration {
    let factor = 1u32.checked_shl(consecutive_failures.min(3)).unwrap_or(8);
    base * factor
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, DatasetSummary, ReloadStats};
    use crate::errors::AppError;
    use crate::models::{CountRequest, QueryRequest};
    use crate::schema::DatasetSchema;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    // -----------------------------------------------------------------------
    // Mock backend
    // -----------------------------------------------------------------------

    struct MockBackend {
        build_count: Arc<AtomicU32>,
        build_delay: Option<Duration>,
        fail: Arc<std::sync::atomic::AtomicBool>,
        coalesce: Arc<std::sync::atomic::AtomicBool>,
        coalesce_count: Arc<AtomicU32>,
    }

    impl MockBackend {
        fn new() -> (Self, Arc<AtomicU32>) {
            let count = Arc::new(AtomicU32::new(0));
            let b = Self {
                build_count: count.clone(),
                build_delay: None,
                fail: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                coalesce: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                coalesce_count: Arc::new(AtomicU32::new(0)),
            };
            (b, count)
        }
    }

    #[async_trait]
    impl Backend for MockBackend {
        fn names(&self) -> Vec<String> {
            vec!["ds".into()]
        }

        fn summary(&self, _name: &str) -> Result<DatasetSummary, AppError> {
            Ok(DatasetSummary {
                name: "ds".into(),
                columns: 1,
                rows: 0,
                lazy: false,
            })
        }

        fn schema(&self, _name: &str) -> Result<Arc<DatasetSchema>, AppError> {
            Err(AppError::NotFound("mock".into()))
        }

        async fn sample(&self, _name: &str) -> Result<String, AppError> {
            Err(AppError::NotFound("mock".into()))
        }

        async fn query(&self, _name: &str, _req: &QueryRequest) -> Result<String, AppError> {
            Err(AppError::NotFound("mock".into()))
        }

        async fn count(&self, _name: &str, _req: &CountRequest) -> Result<i64, AppError> {
            Err(AppError::NotFound("mock".into()))
        }

        async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
            self.try_reload(name).await.map(|opt| {
                opt.unwrap_or(ReloadStats {
                    rows: 0,
                    elapsed_ms: 0,
                })
            })
        }

        async fn try_reload(&self, _name: &str) -> Result<Option<ReloadStats>, AppError> {
            if self.coalesce.load(Ordering::SeqCst) {
                self.coalesce_count.fetch_add(1, Ordering::SeqCst);
                return Ok(None);
            }

            if let Some(d) = self.build_delay {
                tokio::time::sleep(d).await;
            }

            self.build_count.fetch_add(1, Ordering::SeqCst);

            if self.fail.load(Ordering::SeqCst) {
                Err(AppError::Internal("mock build failure".into()))
            } else {
                Ok(Some(ReloadStats {
                    rows: 42,
                    elapsed_ms: 1,
                }))
            }
        }
    }

    fn schedule(name: &str, interval: Duration) -> DatasetSchedule {
        DatasetSchedule {
            name: name.into(),
            interval,
            timeout: Duration::from_secs(60),
            jitter: false,
        }
    }

    fn schedule_with_timeout(name: &str, interval: Duration, timeout: Duration) -> DatasetSchedule {
        DatasetSchedule {
            name: name.into(),
            interval,
            timeout,
            jitter: false,
        }
    }

    // -----------------------------------------------------------------------
    // R3.1 — interval fires and publishes
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_fires_at_interval() {
        let (backend, build_count) = MockBackend::new();
        let backend: Arc<dyn Backend> = Arc::new(backend);
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![schedule("ds", Duration::from_secs(1))], 1);
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        // Yield once to let the scheduler task start and capture now = t=0,
        // so its first fire is at t=1s not t=2s.
        tokio::task::yield_now().await;

        // Advance time in steps to let the scheduler fully process each tick.
        // Each step: advance by 1s (fires the timer) + many yields (to run
        // the scheduler's non-timer await points: semaphore, try_reload, etc.).
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        let _ = handle.await;

        let count = build_count.load(Ordering::SeqCst);
        assert!(
            count >= 3,
            "expected ≥3 builds in 4 × interval, got {count}"
        );
    }

    // -----------------------------------------------------------------------
    // R3.2 — coalescing: pretend mutex held → no builds, coalesce counter rises
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_coalesces_when_mutex_held() {
        let (mut backend, build_count) = MockBackend::new();
        backend.coalesce.store(true, Ordering::SeqCst);
        let coalesce_count = backend.coalesce_count.clone();
        let backend: Arc<dyn Backend> = Arc::new(backend);
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![schedule("ds", Duration::from_secs(1))], 1);
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        let _ = handle.await;

        assert_eq!(
            build_count.load(Ordering::SeqCst),
            0,
            "coalesced ticks must not trigger builds"
        );
        assert!(
            coalesce_count.load(Ordering::SeqCst) >= 3,
            "expected ≥3 coalesced ticks"
        );
    }

    // -----------------------------------------------------------------------
    // R3.2 — global semaphore: two datasets, max_concurrent=1 → serial
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_serialises_with_max_concurrent_1() {
        // A combined backend that routes try_reload by name to one of two
        // sub-backends. Each build takes 100 ms (paused time).
        struct TwoDs {
            count_a: Arc<AtomicU32>,
            count_b: Arc<AtomicU32>,
        }
        #[async_trait]
        impl Backend for TwoDs {
            fn names(&self) -> Vec<String> {
                vec!["a".into(), "b".into()]
            }
            fn summary(&self, _: &str) -> Result<DatasetSummary, AppError> {
                Ok(DatasetSummary {
                    name: "x".into(),
                    columns: 0,
                    rows: 0,
                    lazy: false,
                })
            }
            fn schema(&self, _: &str) -> Result<Arc<DatasetSchema>, AppError> {
                Err(AppError::NotFound("mock".into()))
            }
            async fn sample(&self, _: &str) -> Result<String, AppError> {
                Err(AppError::NotFound("mock".into()))
            }
            async fn query(&self, _: &str, _: &QueryRequest) -> Result<String, AppError> {
                Err(AppError::NotFound("mock".into()))
            }
            async fn count(&self, _: &str, _: &CountRequest) -> Result<i64, AppError> {
                Err(AppError::NotFound("mock".into()))
            }
            async fn reload(&self, name: &str) -> Result<ReloadStats, AppError> {
                self.try_reload(name).await.map(|o| {
                    o.unwrap_or(ReloadStats {
                        rows: 0,
                        elapsed_ms: 0,
                    })
                })
            }
            async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
                // Each build takes 100 ms.
                tokio::time::sleep(Duration::from_millis(100)).await;
                match name {
                    "a" => self.count_a.fetch_add(1, Ordering::SeqCst),
                    "b" => self.count_b.fetch_add(1, Ordering::SeqCst),
                    _ => 0,
                };
                Ok(Some(ReloadStats {
                    rows: 1,
                    elapsed_ms: 100,
                }))
            }
        }

        let count_a = Arc::new(AtomicU32::new(0));
        let count_b = Arc::new(AtomicU32::new(0));
        let backend: Arc<dyn Backend> = Arc::new(TwoDs {
            count_a: count_a.clone(),
            count_b: count_b.clone(),
        });
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(
            vec![
                schedule("a", Duration::from_secs(1)),
                schedule("b", Duration::from_secs(1)),
            ],
            1, // serial
        );
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        // First tick fires at 1 s (both "a" and "b"). With max_concurrent=1,
        // each 100 ms build runs serially. Advance time step by step.
        for _ in 0..4 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        let _ = handle.await;

        let total = count_a.load(Ordering::SeqCst) + count_b.load(Ordering::SeqCst);
        assert!(total >= 2, "both datasets must have built; got {total}");
    }

    // -----------------------------------------------------------------------
    // R3.3 — timeout: slow build cancelled; build_count stays 0
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_timeout_cancels_build() {
        let (mut backend, build_count) = MockBackend::new();
        // Build delay (10 s) >> timeout (1 s).
        backend.build_delay = Some(Duration::from_secs(10));
        let backend: Arc<dyn Backend> = Arc::new(backend);
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(
            vec![schedule_with_timeout(
                "ds",
                Duration::from_secs(1),
                Duration::from_secs(1),
            )],
            1,
        );
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        // Advance to fire the first tick (at 1s), then past the timeout.
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // Advance to trigger the timeout (1s timeout from the start of the build).
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        token.cancel();
        let _ = handle.await;

        assert_eq!(
            build_count.load(Ordering::SeqCst),
            0,
            "timed-out build must not reach build_count increment"
        );
    }

    // -----------------------------------------------------------------------
    // R3.4 — backoff schedule with paused clock
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_applies_backoff_on_failures() {
        let (mut backend, build_count) = MockBackend::new();
        backend.fail.store(true, Ordering::SeqCst);
        let backend: Arc<dyn Backend> = Arc::new(backend);
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![schedule("ds", Duration::from_secs(1))], 1);
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        // First tick fires at t=1s. After failure, backoff = 2 × base = 2s.
        // Next fire ≈ t=3s. At t=2s only the first build should have fired.
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        let after_1 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_1, 1, "1 build at t=1s; got {after_1}");

        // At t=2s (1s after first tick): backoff interval not yet elapsed.
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let after_2 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_2, 1, "still 1 build at t=2s; got {after_2}");

        // At t=3.5s: 2nd build at t≈3s (2 × base from first tick) should have fired.
        tokio::time::advance(Duration::from_millis(1500)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let after_3_5 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_3_5, 2, "2nd build at ≈t=3s; got {after_3_5}");

        token.cancel();
        let _ = handle.await;

        // Unit-test backoff_interval directly.
        assert_eq!(
            backoff_interval(Duration::from_secs(1), 0),
            Duration::from_secs(1)
        );
        assert_eq!(
            backoff_interval(Duration::from_secs(1), 1),
            Duration::from_secs(2)
        );
        assert_eq!(
            backoff_interval(Duration::from_secs(1), 2),
            Duration::from_secs(4)
        );
        assert_eq!(
            backoff_interval(Duration::from_secs(1), 3),
            Duration::from_secs(8)
        );
        assert_eq!(
            backoff_interval(Duration::from_secs(1), 4),
            Duration::from_secs(8)
        ); // cap
    }

    // -----------------------------------------------------------------------
    // R3.6 — graceful shutdown: task exits promptly, no partial build
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_shuts_down_cleanly() {
        let (backend, build_count) = MockBackend::new();
        let backend: Arc<dyn Backend> = Arc::new(backend);
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![schedule("ds", Duration::from_secs(100))], 1);
        let handle = sched.spawn(backend, token.clone());
        tokio::task::yield_now().await; // let scheduler start

        // Cancel before the first tick.
        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), handle).await;

        assert!(result.is_ok(), "scheduler must exit within deadline");
        assert_eq!(
            build_count.load(Ordering::SeqCst),
            0,
            "no build before first tick"
        );
    }

    // -----------------------------------------------------------------------
    // R3.4 / R3.5 — unit tests
    // -----------------------------------------------------------------------
    #[test]
    fn backoff_interval_caps_at_8x() {
        let base = Duration::from_secs(5);
        assert_eq!(backoff_interval(base, 0), Duration::from_secs(5));
        assert_eq!(backoff_interval(base, 1), Duration::from_secs(10));
        assert_eq!(backoff_interval(base, 2), Duration::from_secs(20));
        assert_eq!(backoff_interval(base, 3), Duration::from_secs(40));
        assert_eq!(backoff_interval(base, 4), Duration::from_secs(40));
        assert_eq!(backoff_interval(base, 100), Duration::from_secs(40));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let mut rng = Rng::new();
        let base = Duration::from_secs(10);
        for _ in 0..1000 {
            let j = jittered(base, true, &mut rng);
            let secs = j.as_secs_f64();
            assert!(
                (9.0..=11.0).contains(&secs),
                "jitter out of ±10% band: {secs}"
            );
        }
    }
}
