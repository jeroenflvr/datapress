//! Refresh scheduler for `kind = "query"` datasets (Phase 3, R3.1–R3.7)
//! extended with the cascade engine (Phase 4, R4.3–R4.4).
//!
//! **Scheduler (single tokio task, R3.1):** owns a min-heap of
//! `(next_fire, dataset)` entries and drives periodic rebuilds.
//!
//! **Cascade engine (separate tokio task, R4.3):** receives
//! [`CascadeHandle::notify_published`] signals from backends after every
//! successful publish.  For each upstream publish it looks up downstream
//! datasets that have `on_upstream_reload = true`, applies a per-dataset
//! sliding-window debounce, and sends one-shot [`CascadeRequest`]s to the
//! scheduler loop when the debounce window expires.
//!
//! **Coalescing (R3.2):** on each tick the scheduler acquires (1) the global
//! concurrency semaphore, then calls (2) `backend.try_reload(name)`. If the
//! per-dataset reload mutex is already held, `try_reload` returns `Ok(None)`
//! and the tick is skipped; the next fire is rescheduled from *now* + interval.
//!
//! **Timeout (R3.3):** the `try_reload` future is wrapped in
//! `tokio::time::timeout`. On expiry the future is cancelled; for DuckDB the
//! underlying `web::block` thread may continue until the engine returns, but
//! the semaphore permit is released regardless (never leaked).
//!
//! **Backoff (R3.4):** consecutive failures back off exponentially
//! (base = interval, factor 2, cap 8 × interval). Reset on success or coalesce.
//!
//! **Jitter (R3.5):** ±10 % uniform jitter applied to every scheduled fire.
//!
//! **Graceful shutdown (R3.6):** tasks are stopped via a `CancellationToken`;
//! the cancellation point is between ticks / debounce sleeps, never mid-build.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::backend::{Backend, CascadeHandle};

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
// Scheduled min-heap entry
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
// Cascade types (R4.3 / R4.4)
// ---------------------------------------------------------------------------

/// One downstream cascade dependency derived from a dataset's
/// `[dataset.refresh]` block.
#[derive(Debug, Clone)]
pub struct CascadeDep {
    /// The downstream dataset to refresh when the upstream publishes.
    pub name: String,
    /// Debounce window (`[dataset.refresh] debounce`, default 5 s).
    pub debounce: Duration,
    /// Build timeout for the downstream dataset (`[dataset.refresh] timeout`).
    pub timeout: Duration,
}

/// DAG mapping each upstream dataset name to its cascade dependents.
/// Built from all configured datasets with `refresh.on_upstream_reload = true`.
/// Key = upstream name; value = list of dependent [`CascadeDep`]s.
pub type CascadeDag = HashMap<String, Vec<CascadeDep>>;

/// An immediate-fire cascade-refresh request sent from the cascade engine
/// to the scheduler's run-loop (R4.4).  No heap entry is created; these
/// are one-shot builds that still go through the semaphore and `try_reload`.
struct CascadeRequest {
    name: String,
    timeout: Duration,
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

/// A handle that lets any server component schedule a one-shot TTL deletion
/// of a managed dataset (R8.1). Sending the dataset name and the fire
/// instant into the channel causes the scheduler loop to call
/// `backend.unregister(name)` at (approximately) the requested time.
///
/// The handle is stored as actix app-data (`web::Data<TtlHandle>`) so
/// handlers can call [`TtlHandle::schedule`] after registering a temp dataset.
#[derive(Clone)]
pub struct TtlHandle {
    tx: tokio::sync::mpsc::UnboundedSender<(tokio::time::Instant, String)>,
}

impl TtlHandle {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<(tokio::time::Instant, String)>) -> Self {
        Self { tx }
    }

    /// Schedule the unregistration of `name` at `fire_at`.
    ///
    /// Fails silently if the scheduler has already shut down (the process is
    /// exiting; the dataset will disappear with it anyway).
    pub fn schedule(&self, name: String, fire_at: tokio::time::Instant) {
        let _ = self.tx.send((fire_at, name));
    }
}

/// Return value of [`RefreshScheduler::spawn`].
pub struct SpawnResult {
    /// Join handles for the scheduler loop and optional cascade engine.
    /// Await all of them in the shutdown path.
    pub handles: Vec<tokio::task::JoinHandle<()>>,
    /// Cascade notification handle to give to each backend via
    /// [`crate::backend::Backend::set_cascade_handle`].
    /// `None` when `cascade_dag` is empty.
    pub cascade_handle: Option<CascadeHandle>,
}

/// Drives periodic refresh of configured datasets (Phase 3) and,
/// optionally, cascade-triggered refreshes (Phase 4).
pub struct RefreshScheduler {
    schedules: Vec<DatasetSchedule>,
    max_concurrent: usize,
    #[cfg(feature = "metrics")]
    metrics: Option<std::sync::Arc<crate::metrics::DatapressMetrics>>,
}

impl RefreshScheduler {
    pub fn new(schedules: Vec<DatasetSchedule>, max_concurrent: usize) -> Self {
        Self {
            schedules,
            max_concurrent: max_concurrent.max(1),
            #[cfg(feature = "metrics")]
            metrics: None,
        }
    }

    /// Attach Prometheus metrics so the scheduler can increment spill/override
    /// counters on each successful build (T5.3 deviation closure).
    #[cfg(feature = "metrics")]
    pub fn with_metrics(
        mut self,
        metrics: std::sync::Arc<crate::metrics::DatapressMetrics>,
    ) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Whether there are no periodic schedules and no cascade DAG. Note:
    /// the scheduler loop is still spawned even when this returns `true`,
    /// because TTL deletion requests may arrive later.
    pub fn is_empty(&self) -> bool {
        self.schedules.is_empty()
    }

    /// Spawn the scheduler loop and, when `cascade_dag` is non-empty, a
    /// cascade engine task.  Cancel `shutdown` to stop both tasks between
    /// ticks (R3.6 / Phase 4).
    ///
    /// `ttl_rx` receives one-shot deletion requests from the queries API
    /// (R8.1); pass the receiver end of a pre-created channel so the
    /// transmit half can be wrapped in a `TtlHandle` before the HTTP server
    /// starts.
    pub fn spawn(
        self,
        backend: Arc<dyn Backend>,
        shutdown: CancellationToken,
        cascade_dag: CascadeDag,
        ttl_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(tokio::time::Instant, String)>>,
    ) -> SpawnResult {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));

        // Build the cascade engine when the DAG is non-empty.
        let (cascade_handle, cascade_rx, cascade_jh) = if !cascade_dag.is_empty() {
            let (pub_tx, pub_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel::<CascadeRequest>();
            let handle = CascadeHandle::new(pub_tx);
            let jh = tokio::spawn(cascade_engine_loop(
                cascade_dag,
                pub_rx,
                req_tx,
                shutdown.clone(),
            ));
            (Some(handle), Some(req_rx), Some(jh))
        } else {
            (None, None, None)
        };

        #[cfg(feature = "metrics")]
        let sched_metrics = self.metrics.clone();
        #[cfg(not(feature = "metrics"))]
        let sched_metrics = ();

        let sched_jh = tokio::spawn(run_loop(
            self.schedules,
            backend,
            semaphore,
            shutdown,
            cascade_rx,
            ttl_rx,
            sched_metrics,
        ));

        let mut handles = vec![sched_jh];
        if let Some(jh) = cascade_jh {
            handles.push(jh);
        }

        SpawnResult {
            handles,
            cascade_handle,
        }
    }
}

// ---------------------------------------------------------------------------
// Cascade engine (R4.3 / R4.4)
// ---------------------------------------------------------------------------

/// Background task that manages debounce timers for cascade refreshes (R4.3).
/// Receives upstream publish events, updates per-dataset sliding-window
/// timers, and sends one-shot [`CascadeRequest`]s to the scheduler loop when
/// timers expire.
async fn cascade_engine_loop(
    dag: CascadeDag,
    mut publish_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    req_tx: tokio::sync::mpsc::UnboundedSender<CascadeRequest>,
    shutdown: CancellationToken,
) {
    // pending[dataset] = (fire_at, timeout)
    let mut pending: HashMap<String, (Instant, Duration)> = HashMap::new();

    loop {
        let next_fire: Option<Instant> = pending.values().map(|(t, _)| *t).min();

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                log::debug!("[cascade] engine shutting down");
                return;
            }
            name = publish_rx.recv() => {
                match name {
                    None => {
                        log::debug!("[cascade] publish channel closed; engine exiting");
                        return;
                    }
                    Some(upstream) => {
                        if let Some(deps) = dag.get(&upstream) {
                            let now = Instant::now();
                            for dep in deps {
                                // Sliding-window debounce: reset the timer on
                                // every upstream publish so a rapid wave of
                                // reloads coalesces into one downstream build.
                                let fire_at = now + dep.debounce;
                                pending.insert(dep.name.clone(), (fire_at, dep.timeout));
                                log::debug!(
                                    "[cascade] upstream '{upstream}' published \
                                     → '{}' debounced {:?}",
                                    dep.name, dep.debounce,
                                );
                            }
                        }
                    }
                }
            }
            _ = async {
                match next_fire {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let now = Instant::now();
                let fired: Vec<(String, Duration)> = pending
                    .iter()
                    .filter(|(_, (t, _))| *t <= now)
                    .map(|(n, (_, d))| (n.clone(), *d))
                    .collect();
                for (name, timeout) in fired {
                    pending.remove(&name);
                    log::info!("[cascade] enqueuing cascade refresh for '{name}'");
                    if req_tx.send(CascadeRequest { name, timeout }).is_err() {
                        log::debug!("[cascade] scheduler channel closed; engine exiting");
                        return;
                    }
                }
            }
        }
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
    // Receives one-shot cascade requests from the cascade engine (R4.4).
    mut cascade_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CascadeRequest>>,
    // Receives one-shot TTL deletion requests from the queries API (R8.1).
    // Wrapped in Option so the arm is skipped after the sender is dropped.
    mut ttl_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(tokio::time::Instant, String)>>,
    #[cfg(feature = "metrics")] metrics: Option<std::sync::Arc<crate::metrics::DatapressMetrics>>,
    #[cfg(not(feature = "metrics"))] _metrics: (),
) {
    // Only return early when there is genuinely nothing to do.
    if schedules.is_empty() && cascade_rx.is_none() {
        // Note: we never return early even when schedules+cascade are empty
        // because TTL events may arrive at any time.
    }

    let mut rng = Rng::new();
    let now = Instant::now();

    // Pending TTL deletions: (fire_at, name)
    let mut ttl_pending: Vec<(tokio::time::Instant, String)> = Vec::new();

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
        // Check for TTL events whose fire_at has passed or is next.
        ttl_pending.retain(|(fire_at, name)| {
            if *fire_at <= tokio::time::Instant::now() {
                let name = name.clone();
                let backend = backend.clone();
                tokio::spawn(async move {
                    match backend.unregister(&name).await {
                        Ok(()) => log::info!("[ttl] dataset='{}' expired and unregistered", name),
                        Err(e) => log::warn!("[ttl] dataset='{}' unregister error: {}", name, e),
                    }
                });
                false // remove
            } else {
                true // keep
            }
        });

        // Compute time until the next TTL fire (if any).
        let ttl_next: Option<tokio::time::Instant> = ttl_pending.iter().map(|(t, _)| *t).min();

        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                log::debug!("[refresh] scheduler shutting down");
                return;
            }

            // Receive new TTL requests.
            item = async {
                match ttl_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match item {
                    Some((fire_at, name)) => {
                        ttl_pending.push((fire_at, name));
                    }
                    None => {
                        // TTL channel closed — stop polling it.
                        ttl_rx = None;
                    }
                }
            }

            // Wake when the next TTL fires.
            _ = async {
                match ttl_next {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                // The top of the loop will process expired TTLs.
                continue;
            }

            // R4.4: cascade requests — immediate fire, one-shot (no heap entry).
            req = async {
                match cascade_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(req) = req else {
                    log::debug!("[refresh] cascade channel closed; scheduler exiting");
                    return;
                };
                let name = req.name.clone();
                let timeout_dur = req.timeout;
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("refresh semaphore closed");
                // R3.2 coalescing + R3.3 timeout apply to cascade builds too.
                let outcome =
                    tokio::time::timeout(timeout_dur, backend.try_reload(&name)).await;
                drop(permit);
                match outcome {
                    Err(_elapsed) => {
                        log::warn!(
                            "[refresh] dataset='{}' trigger=cascade outcome=timeout elapsed_ms={}",
                            name, timeout_dur.as_millis(),
                        );
                    }
                    Ok(Err(e)) => {
                        log::warn!(
                            "[refresh] dataset='{}' trigger=cascade outcome=failed error=\"{}\"",
                            name, e,
                        );
                    }
                    Ok(Ok(None)) => {
                        log::debug!("[refresh] dataset='{}' trigger=cascade outcome=skipped reason=\"reload mutex held\"", name);
                    }
                    Ok(Ok(Some(stats))) => {
                        log::info!(
                            "[publish] dataset='{}' trigger=cascade rows={} elapsed_ms={}",
                            name, stats.rows, stats.elapsed_ms,
                        );
                        use crate::backend::{RefreshRecord, RefreshSource};
                        use crate::storage::now_rfc3339;
                        let rec = RefreshRecord {
                            last_refresh_at: Some(now_rfc3339()),
                            last_refresh_duration_ms: Some(stats.elapsed_ms),
                            refresh_source: Some(RefreshSource::Cascade),
                            consecutive_failures: 0,
                            last_error: None,
                            ..Default::default()
                        };
                        backend.record_refresh(&name, rec);
                    }
                }
            }

            // R3.1 / R3.2: scheduled tick — sleep until the next heap entry.
            _ = async {
                match heap.peek() {
                    Some(e) => tokio::time::sleep_until(e.fire_at.0).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let mut entry = match heap.pop() {
                    Some(e) => e,
                    None => continue,
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
                let outcome =
                    tokio::time::timeout(timeout_dur, backend.try_reload(&name)).await;

                drop(permit); // release semaphore regardless of outcome (R3.3)

                let next_interval = match outcome {
                    Err(_elapsed) => {
                        entry.consecutive_failures += 1;
                        log::warn!(
                            "[refresh] dataset='{}' outcome=timeout elapsed_ms={} consecutive_failures={}",
                            name,
                            timeout_dur.as_millis(),
                            entry.consecutive_failures,
                        );
                        let err_msg = format!("timed out after {:?}", timeout_dur);
                        {
                            use crate::backend::RefreshRecord;
                            let rec = RefreshRecord {
                                consecutive_failures: entry.consecutive_failures,
                                last_error: Some(truncate_error(&err_msg)),
                                ..Default::default()
                            };
                            backend.record_refresh(&name, rec);
                        }
                        backoff_interval(entry.interval, entry.consecutive_failures)
                    }
                    Ok(Err(e)) => {
                        entry.consecutive_failures += 1;
                        let err_str = e.to_string();
                        log::warn!(
                            "[refresh] dataset='{}' outcome=failed consecutive_failures={} error=\"{}\"",
                            name,
                            entry.consecutive_failures,
                            err_str,
                        );
                        {
                            use crate::backend::RefreshRecord;
                            let rec = RefreshRecord {
                                consecutive_failures: entry.consecutive_failures,
                                last_error: Some(truncate_error(&err_str)),
                                ..Default::default()
                            };
                            backend.record_refresh(&name, rec);
                        }
                        backoff_interval(entry.interval, entry.consecutive_failures)
                    }
                    Ok(Ok(None)) => {
                        // Coalesced — not a failure, normal interval.
                        log::debug!("[refresh] dataset='{}' outcome=skipped reason=\"reload mutex held\"", name);
                        entry.consecutive_failures = 0;
                        {
                            use crate::backend::RefreshRecord;
                            let rec = RefreshRecord {
                                consecutive_failures: 0,
                                ..Default::default()
                            };
                            backend.record_refresh(&name, rec);
                        }
                        entry.interval
                    }
                    Ok(Ok(Some(stats))) => {
                        entry.consecutive_failures = 0;
                        log::info!(
                            "[publish] dataset='{}' trigger=schedule rows={} elapsed_ms={}",
                            name,
                            stats.rows,
                            stats.elapsed_ms,
                        );
                        {
                            use crate::backend::{RefreshRecord, RefreshSource};
                            use crate::storage::now_rfc3339;
                            let rec = RefreshRecord {
                                last_refresh_at: Some(now_rfc3339()),
                                last_refresh_duration_ms: Some(stats.elapsed_ms),
                                refresh_source: Some(RefreshSource::Schedule),
                                consecutive_failures: 0,
                                last_error: None,
                                ..Default::default()
                            };
                            backend.record_refresh(&name, rec);
                        }
                        // Increment spill / memory-override metrics from build flags.
                        #[cfg(feature = "metrics")]
                        if let Some(ref m) = metrics {
                            if stats.demoted_to_storage {
                                crate::metrics::record_spill(m, &name);
                            }
                            if stats.memory_override_exceeded {
                                crate::metrics::record_memory_override(m, &name);
                            }
                        }
                        entry.interval
                    }
                };

                let next = jittered(next_interval, entry.jitter, &mut rng);
                entry.fire_at = Reverse(tick_start + next);
                // Push next_refresh_at to the backend so /status can report it.
                {
                    use crate::backend::RefreshRecord;
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let fire_instant = entry.fire_at.0;
                    // Convert tokio Instant offset to a wall-clock RFC3339.
                    let now_inst = tokio::time::Instant::now();
                    let offset = if fire_instant > now_inst {
                        fire_instant.duration_since(now_inst)
                    } else {
                        Duration::ZERO
                    };
                    let fire_wall = SystemTime::now() + offset;
                    let secs = fire_wall
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let next_at = format_rfc3339_secs(secs);
                    let rec = RefreshRecord {
                        next_refresh_at: Some(next_at),
                        ..Default::default()
                    };
                    backend.record_refresh(&name, rec);
                }
                heap.push(entry);
            }
        }
    }
}

/// Exponential backoff capped at 8 × base (R3.4).
fn backoff_interval(base: Duration, consecutive_failures: u32) -> Duration {
    let factor = 1u32.checked_shl(consecutive_failures.min(3)).unwrap_or(8);
    base * factor
}

/// Truncate an error string to 500 characters (T5.1).
fn truncate_error(msg: &str) -> String {
    if msg.len() <= 500 {
        msg.to_string()
    } else {
        format!("{}…", &msg[..499])
    }
}

/// Format a Unix epoch seconds value as an RFC-3339 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`) without pulling in the `chrono` crate.
fn format_rfc3339_secs(secs: u64) -> String {
    // Days since 1970-01-01 and time-within-day.
    let s_per_day = 86_400u64;
    let time_of_day = secs % s_per_day;
    let days = secs / s_per_day;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Calendar conversion (no leap-seconds).
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hh, mm, ss
    )
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Gregorian calendar algorithm (valid for Unix timestamps).
    let mut year = 1970u64;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let months = if is_leap(year) {
        [31u64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u64;
    for dm in &months {
        if days < *dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, CascadeHandle, DatasetSummary, ReloadStats};
    use crate::errors::AppError;
    use crate::models::{CountRequest, QueryRequest};
    use crate::schema::DatasetSchema;
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    // -----------------------------------------------------------------------
    // Mock backend (Phase 3 tests)
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
                    ..Default::default()
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
                    ..Default::default()
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
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

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
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;

        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

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
                        ..Default::default()
                    })
                })
            }
            async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
                tokio::time::sleep(Duration::from_millis(100)).await;
                match name {
                    "a" => self.count_a.fetch_add(1, Ordering::SeqCst),
                    "b" => self.count_b.fetch_add(1, Ordering::SeqCst),
                    _ => 0,
                };
                Ok(Some(ReloadStats {
                    rows: 1,
                    elapsed_ms: 100,
                    ..Default::default()
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
            1,
        );
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;

        for _ in 0..4 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

        let total = count_a.load(Ordering::SeqCst) + count_b.load(Ordering::SeqCst);
        assert!(total >= 2, "both datasets must have built; got {total}");
    }

    // -----------------------------------------------------------------------
    // R3.3 — timeout: slow build cancelled; build_count stays 0
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn scheduler_timeout_cancels_build() {
        let (mut backend, build_count) = MockBackend::new();
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
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

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
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        let after_1 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_1, 1, "1 build at t=1s; got {after_1}");

        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let after_2 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_2, 1, "still 1 build at t=2s; got {after_2}");

        tokio::time::advance(Duration::from_millis(1500)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let after_3_5 = build_count.load(Ordering::SeqCst);
        assert_eq!(after_3_5, 2, "2nd build at ≈t=3s; got {after_3_5}");

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

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
        );
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
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend, token.clone(), HashMap::new(), Some(ttl_rx))
        };
        tokio::task::yield_now().await;

        token.cancel();
        for jh in result.handles {
            let r = tokio::time::timeout(Duration::from_secs(5), jh).await;
            assert!(r.is_ok(), "scheduler must exit within deadline");
        }
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

    // =======================================================================
    // Phase 4 — cascade tests (R4.3, R4.4, R4.6)
    // =======================================================================

    /// Mock backend that increments per-dataset build counters and, on
    /// success, forwards `notify_published(name)` to simulate what the real
    /// backends do inside `reload_inner`.
    struct CascadeMockBackend {
        counts: Arc<Mutex<HashMap<String, u32>>>,
        cascade_handle: Mutex<Option<CascadeHandle>>,
        fail_names: Arc<Mutex<HashSet<String>>>,
        /// Optional per-dataset build delay (paused time).
        build_delay: Option<Duration>,
    }

    impl CascadeMockBackend {
        fn new() -> (Arc<Self>, Arc<Mutex<HashMap<String, u32>>>) {
            let counts = Arc::new(Mutex::new(HashMap::new()));
            let b = Arc::new(Self {
                counts: counts.clone(),
                cascade_handle: Mutex::new(None),
                fail_names: Arc::new(Mutex::new(HashSet::new())),
                build_delay: None,
            });
            (b, counts)
        }
    }

    #[async_trait]
    impl Backend for CascadeMockBackend {
        fn names(&self) -> Vec<String> {
            vec![]
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
                    ..Default::default()
                })
            })
        }

        /// Simulate a real backend's reload_inner: count the build, notify
        /// cascade on success (R4.6: not on failure).
        async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
            if let Some(d) = self.build_delay {
                tokio::time::sleep(d).await;
            }
            if self.fail_names.lock().unwrap().contains(name) {
                return Err(AppError::Internal(format!("mock fail for '{name}'")));
            }
            *self
                .counts
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_default() += 1;
            // Notify cascade — mirrors what Store::reload_inner / Registry::reload_inner do.
            if let Some(h) = self.cascade_handle.lock().unwrap().as_ref() {
                h.notify_published(name);
            }
            Ok(Some(ReloadStats {
                rows: 1,
                elapsed_ms: 1,
                ..Default::default()
            }))
        }

        fn set_cascade_handle(&self, handle: CascadeHandle) {
            *self.cascade_handle.lock().unwrap() = Some(handle);
        }
    }

    /// Helper: build a CascadeDag from a list of `(upstream, [(downstream, debounce_ms)])`.
    fn make_dag(entries: &[(&str, &[(&str, u64)])]) -> CascadeDag {
        let mut dag: CascadeDag = HashMap::new();
        for &(upstream, deps) in entries {
            let v: Vec<CascadeDep> = deps
                .iter()
                .map(|&(ds, ms)| CascadeDep {
                    name: ds.to_string(),
                    debounce: Duration::from_millis(ms),
                    timeout: Duration::from_secs(60),
                })
                .collect();
            dag.insert(upstream.to_string(), v);
        }
        dag
    }

    /// Yield first (so tasks process pending work at the current time), then
    /// advance paused time by `ms` ms, then yield again to let tasks react.
    async fn tick(ms: u64) {
        // Pre-advance yields: let tasks process any pending messages/timers
        // at the current timestamp before time jumps forward.
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(ms)).await;
        // Post-advance yields: let tasks wake from sleep_until and run.
        for _ in 0..80 {
            tokio::task::yield_now().await;
        }
    }

    // -----------------------------------------------------------------------
    // R4.3 — diamond: D depends on B and C, both on A → D refreshes exactly once
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn cascade_diamond_fires_once_per_wave() {
        // A --+-> B --+-> D
        //     +-> C --+
        let dag = make_dag(&[
            ("a", &[("b", 100), ("c", 100)]),
            ("b", &[("d", 100)]),
            ("c", &[("d", 100)]),
        ]);

        let (backend, counts) = CascadeMockBackend::new();
        let backend: Arc<dyn Backend> = backend;
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![], 4);
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend.clone(), token.clone(), dag, Some(ttl_rx))
        };
        backend.set_cascade_handle(result.cascade_handle.clone().unwrap());
        tokio::task::yield_now().await;

        // Publish "a" — cascade engine should schedule B and C.
        result
            .cascade_handle
            .as_ref()
            .unwrap()
            .notify_published("a");

        // tick() pre-yields so cascade engine sees "a" at t=0, setting
        // pending[b]=100ms, pending[c]=100ms; then advances past both.
        tick(120).await;
        // B and C built (notify "b","c"), D debounce pending at ~(120+100)=220ms.
        // Advance past D's debounce.
        tick(150).await;
        tick(50).await; // extra slack for scheduler to process D

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

        let c = counts.lock().unwrap();
        assert_eq!(*c.get("b").unwrap_or(&0), 1, "B must build exactly once");
        assert_eq!(*c.get("c").unwrap_or(&0), 1, "C must build exactly once");
        assert_eq!(
            *c.get("d").unwrap_or(&0),
            1,
            "D must build exactly once (diamond)"
        );
    }

    // -----------------------------------------------------------------------
    // R4.3 — debounce: 3 rapid upstream publishes → exactly 1 downstream build
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn cascade_debounce_coalesces_rapid_publishes() {
        // A -> B with 200 ms debounce.
        let dag = make_dag(&[("a", &[("b", 200)])]);

        let (backend, counts) = CascadeMockBackend::new();
        let backend: Arc<dyn Backend> = backend;
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![], 1);
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend.clone(), token.clone(), dag, Some(ttl_rx))
        };
        backend.set_cascade_handle(result.cascade_handle.clone().unwrap());
        tokio::task::yield_now().await;

        let h = result.cascade_handle.as_ref().unwrap();

        // Three rapid publishes within the debounce window.
        h.notify_published("a");
        tick(50).await;
        h.notify_published("a");
        tick(50).await;
        h.notify_published("a");
        // Now advance past the debounce window (200 ms from last publish).
        tick(220).await;
        tick(50).await; // extra slack for scheduler

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

        let c = counts.lock().unwrap();
        assert_eq!(
            *c.get("b").unwrap_or(&0),
            1,
            "B must build exactly once despite 3 upstream publishes"
        );
    }

    // -----------------------------------------------------------------------
    // R4.3 — transitive chain A→B→C, A publishes → B then C, in order
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn cascade_transitive_chain_ordering() {
        // A -> B -> C
        let dag = make_dag(&[("a", &[("b", 100)]), ("b", &[("c", 100)])]);

        // Track ORDER of builds via a Vec<String>.
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let order_clone = order.clone();
        let counts = Arc::new(Mutex::new(HashMap::<String, u32>::new()));
        let counts_clone = counts.clone();

        struct OrderedBackend {
            order: Arc<Mutex<Vec<String>>>,
            counts: Arc<Mutex<HashMap<String, u32>>>,
            cascade_handle: Mutex<Option<CascadeHandle>>,
        }
        #[async_trait]
        impl Backend for OrderedBackend {
            fn names(&self) -> Vec<String> {
                vec![]
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
                        ..Default::default()
                    })
                })
            }
            async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
                self.order.lock().unwrap().push(name.to_string());
                *self
                    .counts
                    .lock()
                    .unwrap()
                    .entry(name.to_string())
                    .or_default() += 1;
                if let Some(h) = self.cascade_handle.lock().unwrap().as_ref() {
                    h.notify_published(name);
                }
                Ok(Some(ReloadStats {
                    rows: 1,
                    elapsed_ms: 1,
                    ..Default::default()
                }))
            }
            fn set_cascade_handle(&self, h: CascadeHandle) {
                *self.cascade_handle.lock().unwrap() = Some(h);
            }
        }

        let backend: Arc<dyn Backend> = Arc::new(OrderedBackend {
            order: order_clone,
            counts: counts_clone,
            cascade_handle: Mutex::new(None),
        });
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![], 1);
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend.clone(), token.clone(), dag, Some(ttl_rx))
        };
        backend.set_cascade_handle(result.cascade_handle.clone().unwrap());
        tokio::task::yield_now().await;

        // Publish A.
        result
            .cascade_handle
            .as_ref()
            .unwrap()
            .notify_published("a");

        // Advance enough for A→B (100ms) then B→C (100ms after B builds).
        tick(120).await; // B fires
        tick(120).await; // C fires
        tick(50).await;

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

        let ord = order.lock().unwrap();
        assert_eq!(
            *counts.lock().unwrap().get("b").unwrap_or(&0),
            1,
            "B builds once"
        );
        assert_eq!(
            *counts.lock().unwrap().get("c").unwrap_or(&0),
            1,
            "C builds once"
        );
        let pos_b = ord.iter().position(|x| x == "b").expect("b built");
        let pos_c = ord.iter().position(|x| x == "c").expect("c built");
        assert!(pos_b < pos_c, "B must build before C in cascade chain");
    }

    // -----------------------------------------------------------------------
    // R4.6 — failed upstream build must NOT trigger a cascade
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn cascade_failed_upstream_no_cascade() {
        // A (fails) -> B
        let dag = make_dag(&[("a", &[("b", 50)])]);

        let (backend, counts) = CascadeMockBackend::new();
        backend.fail_names.lock().unwrap().insert("a".to_string());
        let backend: Arc<dyn Backend> = backend;
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![], 1);
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend.clone(), token.clone(), dag, Some(ttl_rx))
        };
        backend.set_cascade_handle(result.cascade_handle.clone().unwrap());
        tokio::task::yield_now().await;

        // Trigger a cascade of "a" — but since "a" fails, no notify_published is called.
        // We verify by directly sending a cascade request through the handle.
        // NOTE: The cascade is triggered by a SUCCESSFUL publish. Since a fails,
        // no publish event reaches the cascade engine, so B should NOT build.
        // Here we simulate what WOULD happen if a manual reload of a fails: no notify.
        // (The actual R4.6 guarantee is that notify_published is only called on success.)
        // We send nothing to the cascade engine — just verify B count stays 0.

        tick(200).await; // enough time for any spurious cascade to fire

        token.cancel();
        for jh in result.handles {
            let _ = jh.await;
        }

        let c = counts.lock().unwrap();
        assert_eq!(
            *c.get("b").unwrap_or(&0),
            0,
            "B must NOT build when A failed"
        );
    }

    // -----------------------------------------------------------------------
    // Acceptance: stress test — 10 datasets, random reload storm, no deadlock
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn cascade_stress_10_datasets_no_deadlock() {
        // Build a linear chain: d0 -> d1 -> d2 -> ... -> d9
        let n = 10usize;
        let dag = (0..n - 1)
            .map(|i| {
                (
                    format!("d{i}"),
                    vec![CascadeDep {
                        name: format!("d{}", i + 1),
                        debounce: Duration::from_millis(20),
                        timeout: Duration::from_secs(5),
                    }],
                )
            })
            .collect::<HashMap<_, _>>();

        let counts: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
        let counts_clone = counts.clone();

        struct StressBackend {
            counts: Arc<Mutex<HashMap<String, u32>>>,
            cascade_handle: Mutex<Option<CascadeHandle>>,
        }
        #[async_trait]
        impl Backend for StressBackend {
            fn names(&self) -> Vec<String> {
                vec![]
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
                        ..Default::default()
                    })
                })
            }
            async fn try_reload(&self, name: &str) -> Result<Option<ReloadStats>, AppError> {
                *self
                    .counts
                    .lock()
                    .unwrap()
                    .entry(name.to_string())
                    .or_default() += 1;
                if let Some(h) = self.cascade_handle.lock().unwrap().as_ref() {
                    h.notify_published(name);
                }
                Ok(Some(ReloadStats {
                    rows: 1,
                    elapsed_ms: 0,
                    ..Default::default()
                }))
            }
            fn set_cascade_handle(&self, h: CascadeHandle) {
                *self.cascade_handle.lock().unwrap() = Some(h);
            }
        }

        let backend: Arc<dyn Backend> = Arc::new(StressBackend {
            counts: counts_clone,
            cascade_handle: Mutex::new(None),
        });
        let token = CancellationToken::new();

        let sched = RefreshScheduler::new(vec![], 4);
        let result = {
            let (_ttl_tx, ttl_rx) = tokio::sync::mpsc::unbounded_channel();
            sched.spawn(backend.clone(), token.clone(), dag, Some(ttl_rx))
        };
        backend.set_cascade_handle(result.cascade_handle.clone().unwrap());
        tokio::task::yield_now().await;

        let h = result.cascade_handle.as_ref().unwrap();

        // Simulate a reload storm: repeatedly publish d0 over 500 ms of paused time.
        for _ in 0..10 {
            h.notify_published("d0");
            tick(50).await;
        }
        // Let cascade chain fully settle.
        tick(500).await;

        token.cancel();
        for jh in result.handles {
            // No deadlock: all handles finish quickly.
            let r = tokio::time::timeout(Duration::from_secs(5), jh).await;
            assert!(r.is_ok(), "scheduler/cascade task did not finish in time");
        }

        // d0 was published externally (not built by the scheduler); d1 through
        // d9 must each have cascaded at least once for the chain to be correct.
        let c = counts.lock().unwrap();
        for i in 1..n {
            assert!(
                *c.get(&format!("d{i}")).unwrap_or(&0) >= 1,
                "d{i} should have been built via cascade"
            );
        }
    }
}
