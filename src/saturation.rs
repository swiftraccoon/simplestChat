//! CPU saturation of the process's own cgroup, from cgroup v2 throttling
//! counters and pressure stall information, and of each media worker thread,
//! from its scheduler CPU time. When the container is at its quota, admitting
//! more calls degrades every call; readiness and fresh joins consult this
//! monitor so overload sheds new users instead. A mediasoup router lives on
//! one worker thread, so one busy room can pin a core while the quota still
//! shows headroom: rooms on a saturated worker refuse fresh joins and new
//! rooms are placed elsewhere.

use crate::metrics::{SaturationSnapshot, ServerMetrics, WorkerCpuSnapshot};
use mediasoup::worker::WorkerId;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const DEFAULT_THROTTLED_FRACTION: f64 = 0.5;
pub const DEFAULT_PRESSURE_AVG10: f64 = 50.0;
/// Share of one core a worker thread may use over the window before it is
/// saturated. mediasoup's loop is single-threaded, so beyond this every
/// packet of every room on the worker queues.
pub const DEFAULT_WORKER_UTILIZATION: f64 = 0.85;
/// A saturated worker clears once its utilization falls below this share of
/// the threshold.
const WORKER_CLEAR_FACTOR: f64 = 0.8;
const WINDOW: Duration = Duration::from_secs(10);
const INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub struct SaturationConfig {
    /// Fraction of enforcement periods throttled over the window at which the
    /// process counts as saturated.
    pub throttled_fraction: f64,
    /// `cpu.pressure` `some avg10` percentage at which the process counts as
    /// saturated.
    pub pressure_avg10: f64,
    /// Share of one core over the window at which a media worker thread
    /// counts as saturated.
    pub worker_utilization: f64,
    pub window: Duration,
    pub interval: Duration,
    pub cpu_stat: PathBuf,
    pub cpu_pressure: PathBuf,
    /// `/proc/self/task`, where each worker thread's `schedstat` lives.
    pub task_dir: PathBuf,
}

impl Default for SaturationConfig {
    fn default() -> Self {
        Self {
            throttled_fraction: DEFAULT_THROTTLED_FRACTION,
            pressure_avg10: DEFAULT_PRESSURE_AVG10,
            worker_utilization: DEFAULT_WORKER_UTILIZATION,
            window: WINDOW,
            interval: INTERVAL,
            cpu_stat: PathBuf::from("/sys/fs/cgroup/cpu.stat"),
            cpu_pressure: PathBuf::from("/sys/fs/cgroup/cpu.pressure"),
            task_dir: PathBuf::from("/proc/self/task"),
        }
    }
}

impl SaturationConfig {
    /// `None` when `CPU_SATURATION_DISABLED=true`.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_values(
            std::env::var("CPU_SATURATION_DISABLED").ok().as_deref(),
            std::env::var("CPU_SATURATION_THROTTLED_FRACTION")
                .ok()
                .as_deref(),
            std::env::var("CPU_SATURATION_PRESSURE_AVG10")
                .ok()
                .as_deref(),
            std::env::var("CPU_SATURATION_WORKER_UTILIZATION")
                .ok()
                .as_deref(),
        )
    }

    pub(crate) fn from_values(
        disabled: Option<&str>,
        throttled_fraction: Option<&str>,
        pressure_avg10: Option<&str>,
        worker_utilization: Option<&str>,
    ) -> anyhow::Result<Option<Self>> {
        match disabled.map(str::trim) {
            Some("true") | Some("1") => return Ok(None),
            None | Some("") | Some("false") | Some("0") => {}
            Some(other) => {
                anyhow::bail!("CPU_SATURATION_DISABLED must be true or false, not {other}")
            }
        }
        let mut config = Self::default();
        if let Some(value) = throttled_fraction {
            config.throttled_fraction = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| (0.05..=1.0).contains(v))
                .ok_or_else(|| {
                    anyhow::anyhow!("CPU_SATURATION_THROTTLED_FRACTION must be between 0.05 and 1")
                })?;
        }
        if let Some(value) = pressure_avg10 {
            config.pressure_avg10 = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| (5.0..=100.0).contains(v))
                .ok_or_else(|| {
                    anyhow::anyhow!("CPU_SATURATION_PRESSURE_AVG10 must be between 5 and 100")
                })?;
        }
        if let Some(value) = worker_utilization {
            config.worker_utilization = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| (0.2..=1.0).contains(v))
                .ok_or_else(|| {
                    anyhow::anyhow!("CPU_SATURATION_WORKER_UTILIZATION must be between 0.2 and 1")
                })?;
        }
        Ok(Some(config))
    }
}

/// The cgroup v2 `cpu.stat` counters this monitor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuStat {
    pub nr_periods: u64,
    pub nr_throttled: u64,
}

/// `None` when the file lacks the throttling counters (no quota, cgroup v1).
pub fn parse_cpu_stat(text: &str) -> Option<CpuStat> {
    let mut nr_periods = None;
    let mut nr_throttled = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match (
            parts.next(),
            parts.next().and_then(|v| v.parse::<u64>().ok()),
        ) {
            (Some("nr_periods"), Some(v)) => nr_periods = Some(v),
            (Some("nr_throttled"), Some(v)) => nr_throttled = Some(v),
            _ => {}
        }
    }
    Some(CpuStat {
        nr_periods: nr_periods?,
        nr_throttled: nr_throttled?,
    })
}

/// The `some avg10` percentage of a cgroup v2 `cpu.pressure` file.
pub fn parse_cpu_pressure_some_avg10(text: &str) -> Option<f64> {
    let line = text.lines().find(|line| line.starts_with("some "))?;
    line.split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Throttled share of the enforcement periods between the oldest and newest
/// reading; `None` until periods have elapsed (no quota, or nothing yet).
pub fn throttled_fraction(readings: &VecDeque<CpuStat>) -> Option<f64> {
    let (oldest, newest) = (readings.front()?, readings.back()?);
    let periods = newest.nr_periods.checked_sub(oldest.nr_periods)?;
    let throttled = newest.nr_throttled.checked_sub(oldest.nr_throttled)?;
    (periods > 0).then(|| (throttled as f64 / periods as f64).clamp(0.0, 1.0))
}

/// Saturation with hysteresis: entered at the thresholds, left only once both
/// signals fall below half of them, so a load hovering at the line does not
/// flap admission on and off.
pub fn evaluate(
    previously_saturated: bool,
    throttled: Option<f64>,
    pressure: Option<f64>,
    config: &SaturationConfig,
) -> bool {
    let throttled = throttled.unwrap_or(0.0);
    let pressure = pressure.unwrap_or(0.0);
    if previously_saturated {
        throttled >= config.throttled_fraction / 2.0 || pressure >= config.pressure_avg10 / 2.0
    } else {
        throttled >= config.throttled_fraction || pressure >= config.pressure_avg10
    }
}

/// Nanoseconds a thread has run on a CPU: the first field of
/// `/proc/self/task/<tid>/schedstat`.
pub fn parse_schedstat(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.parse().ok()
}

/// Share of one core a worker thread used across its sampling window: CPU
/// nanoseconds over wall nanoseconds between the oldest and the newest sample.
/// A counter that went backwards contributes nothing rather than a negative
/// share.
pub fn worker_utilization(readings: &VecDeque<(Duration, u64)>) -> Option<f64> {
    let (first_at, first_cpu) = readings.front()?;
    let (last_at, last_cpu) = readings.back()?;
    let wall = last_at.checked_sub(*first_at)?.as_nanos();
    if wall == 0 {
        return None;
    }
    let cpu = u128::from(last_cpu.saturating_sub(*first_cpu));
    Some((cpu as f64 / wall as f64).clamp(0.0, 1.0))
}

/// Worker saturation with hysteresis: entered at the threshold, left once the
/// utilization falls below 80 % of it. A worker without a reading is never
/// saturated.
pub fn evaluate_worker(
    previously_saturated: bool,
    utilization: Option<f64>,
    threshold: f64,
) -> bool {
    match utilization {
        None => false,
        Some(share) if previously_saturated => share >= threshold * WORKER_CLEAR_FACTOR,
        Some(share) => share >= threshold,
    }
}

/// One media worker as the monitor sees it: its position in the pool (the
/// metric label) and its Linux thread ID, when the pool could learn it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerThreadInfo {
    pub worker_id: WorkerId,
    pub index: usize,
    pub tid: Option<u32>,
}

/// The media worker threads the monitor samples and the per-worker saturation
/// flags it maintains for placement and admission.
pub trait WorkerThreads: Send + Sync {
    fn worker_threads(&self) -> Vec<WorkerThreadInfo>;
    fn set_worker_saturated(&self, worker_id: WorkerId, saturated: bool);
}

struct Inner {
    /// The cgroup is throttled or under pressure.
    cgroup_saturated: AtomicBool,
    /// Every worker thread with a reading is saturated: no room can be placed
    /// anywhere useful.
    workers_saturated: AtomicBool,
    enabled: bool,
}

/// Shared handle; cloning is cheap and every clone observes the same state.
#[derive(Clone)]
pub struct SaturationMonitor {
    inner: Arc<Inner>,
}

impl SaturationMonitor {
    /// A monitor that never reports saturation (no cgroup, or disabled).
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Inner {
                cgroup_saturated: AtomicBool::new(false),
                workers_saturated: AtomicBool::new(false),
                enabled: false,
            }),
        }
    }

    /// A monitor pinned to one state, for tests of the paths that consult it.
    #[cfg(test)]
    pub fn forced(saturated: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                cgroup_saturated: AtomicBool::new(saturated),
                workers_saturated: AtomicBool::new(false),
                enabled: true,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled
    }

    /// The process as a whole has no capacity: the cgroup is saturated, or
    /// every media worker is. A single saturated worker is reported per room
    /// through the worker pool instead.
    pub fn saturated(&self) -> bool {
        self.inner.cgroup_saturated.load(Ordering::Relaxed)
            || self.inner.workers_saturated.load(Ordering::Relaxed)
    }

    /// Starts sampling the cgroup files and, when a worker pool is given, each
    /// worker thread's scheduler time on the runtime; the task ends with the
    /// last handle. Each tick publishes a snapshot to the metrics.
    pub fn spawn(
        config: SaturationConfig,
        metrics: ServerMetrics,
        workers: Option<Arc<dyn WorkerThreads>>,
    ) -> Self {
        let monitor = Self {
            inner: Arc::new(Inner {
                cgroup_saturated: AtomicBool::new(false),
                workers_saturated: AtomicBool::new(false),
                enabled: true,
            }),
        };
        let weak = Arc::downgrade(&monitor.inner);
        let capacity =
            (config.window.as_millis() / config.interval.as_millis().max(1)) as usize + 1;
        info!(
            throttled_fraction = config.throttled_fraction,
            pressure_avg10 = config.pressure_avg10,
            worker_utilization = config.worker_utilization,
            "CPU saturation monitor enabled"
        );
        tokio::spawn(async move {
            let started = Instant::now();
            let mut readings: VecDeque<CpuStat> = VecDeque::with_capacity(capacity);
            let mut worker_readings: HashMap<WorkerId, VecDeque<(Duration, u64)>> = HashMap::new();
            let mut worker_state: HashMap<WorkerId, bool> = HashMap::new();
            let mut ticker = tokio::time::interval(config.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut warned = false;
            let mut warned_workers = false;
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let stat = tokio::fs::read_to_string(&config.cpu_stat)
                    .await
                    .ok()
                    .and_then(|text| parse_cpu_stat(&text));
                let pressure = tokio::fs::read_to_string(&config.cpu_pressure)
                    .await
                    .ok()
                    .and_then(|text| parse_cpu_pressure_some_avg10(&text));
                if stat.is_none() && pressure.is_none() && !warned {
                    warned = true;
                    warn!(
                        "CPU saturation monitor found neither cgroup throttling counters nor pressure; admission stays unguarded"
                    );
                }
                if let Some(stat) = stat {
                    if readings.len() == capacity {
                        readings.pop_front();
                    }
                    readings.push_back(stat);
                }
                let throttled = throttled_fraction(&readings);
                let saturated = evaluate(
                    inner.cgroup_saturated.load(Ordering::Relaxed),
                    throttled,
                    pressure,
                    &config,
                );
                if saturated != inner.cgroup_saturated.swap(saturated, Ordering::Relaxed) {
                    if saturated {
                        warn!(
                            throttled = throttled.unwrap_or(0.0),
                            pressure = pressure.unwrap_or(0.0),
                            "CPU saturated: refusing new joins and reporting not ready"
                        );
                    } else {
                        info!("CPU saturation cleared; admitting joins again");
                    }
                }
                metrics.set_saturation(SaturationSnapshot {
                    throttling_available: stat.is_some(),
                    pressure_available: pressure.is_some(),
                    saturated,
                    throttled_fraction: throttled.unwrap_or(0.0),
                    pressure_avg10: pressure.unwrap_or(0.0),
                });

                let Some(workers) = &workers else {
                    continue;
                };
                let threads = workers.worker_threads();
                worker_readings.retain(|id, _| threads.iter().any(|t| t.worker_id == *id));
                worker_state.retain(|id, _| threads.iter().any(|t| t.worker_id == *id));
                let mut snapshots = Vec::with_capacity(threads.len());
                let mut any_reading = false;
                let mut any_cpu_sample = false;
                let mut every_saturated = true;
                for thread in threads {
                    let sample = match thread.tid {
                        Some(tid) => tokio::fs::read_to_string(
                            config.task_dir.join(tid.to_string()).join("schedstat"),
                        )
                        .await
                        .ok()
                        .and_then(|text| parse_schedstat(&text)),
                        None => None,
                    };
                    let history = worker_readings
                        .entry(thread.worker_id)
                        .or_insert_with(|| VecDeque::with_capacity(capacity));
                    match sample {
                        Some(cpu_ns) => {
                            any_cpu_sample = true;
                            if history.len() == capacity {
                                history.pop_front();
                            }
                            history.push_back((started.elapsed(), cpu_ns));
                        }
                        None => history.clear(),
                    }
                    let utilization = worker_utilization(history);
                    let previous = worker_state
                        .get(&thread.worker_id)
                        .copied()
                        .unwrap_or(false);
                    let saturated =
                        evaluate_worker(previous, utilization, config.worker_utilization);
                    if saturated != previous {
                        workers.set_worker_saturated(thread.worker_id, saturated);
                        if saturated {
                            warn!(
                                worker = thread.index,
                                utilization = utilization.unwrap_or(0.0),
                                "media worker saturated: its rooms refuse fresh joins and new rooms go elsewhere"
                            );
                        } else {
                            info!(worker = thread.index, "media worker saturation cleared");
                        }
                    }
                    worker_state.insert(thread.worker_id, saturated);
                    match utilization {
                        Some(_) => {
                            any_reading = true;
                            every_saturated &= saturated;
                        }
                        None => every_saturated = false,
                    }
                    snapshots.push(WorkerCpuSnapshot {
                        index: thread.index,
                        utilization,
                        saturated,
                    });
                }
                if !any_cpu_sample && !snapshots.is_empty() && !warned_workers {
                    warned_workers = true;
                    warn!(
                        "CPU saturation monitor cannot read media worker thread time; per-worker admission stays unguarded"
                    );
                }
                let all = any_reading && every_saturated;
                if all != inner.workers_saturated.swap(all, Ordering::Relaxed) {
                    if all {
                        warn!(
                            "every media worker is saturated: refusing new joins and reporting not ready"
                        );
                    } else {
                        info!("a media worker has capacity again");
                    }
                }
                metrics.set_worker_cpu(snapshots);
            }
        });
        monitor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_stat_needs_both_throttling_counters() {
        let text = "usage_usec 10\nuser_usec 5\nsystem_usec 5\nnr_periods 40\nnr_throttled 8\nthrottled_usec 900\n";
        assert_eq!(
            parse_cpu_stat(text),
            Some(CpuStat {
                nr_periods: 40,
                nr_throttled: 8
            })
        );
        assert_eq!(parse_cpu_stat("usage_usec 10\n"), None);
    }

    #[test]
    fn pressure_reads_the_some_line_only() {
        let text = "some avg10=12.50 avg60=3.00 avg300=0.50 total=100\nfull avg10=99.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(parse_cpu_pressure_some_avg10(text), Some(12.5));
        assert_eq!(parse_cpu_pressure_some_avg10("full avg10=1.0\n"), None);
    }

    #[test]
    fn throttled_fraction_spans_the_window_and_waits_for_periods() {
        let mut readings = VecDeque::new();
        assert_eq!(throttled_fraction(&readings), None);
        readings.push_back(CpuStat {
            nr_periods: 100,
            nr_throttled: 10,
        });
        assert_eq!(
            throttled_fraction(&readings),
            None,
            "no periods elapsed yet"
        );
        readings.push_back(CpuStat {
            nr_periods: 120,
            nr_throttled: 25,
        });
        readings.push_back(CpuStat {
            nr_periods: 140,
            nr_throttled: 40,
        });
        assert_eq!(throttled_fraction(&readings), Some(0.75));
    }

    #[test]
    fn saturation_enters_at_the_threshold_and_leaves_at_half() {
        let config = SaturationConfig::default();
        assert!(!evaluate(false, Some(0.49), Some(49.0), &config));
        assert!(evaluate(false, Some(0.5), None, &config));
        assert!(evaluate(false, None, Some(50.0), &config));
        assert!(
            evaluate(true, Some(0.3), None, &config),
            "stays saturated above half"
        );
        assert!(evaluate(true, None, Some(30.0), &config));
        assert!(!evaluate(true, Some(0.24), Some(24.0), &config));
    }

    #[test]
    fn configuration_bounds_and_disable_switch() {
        assert!(
            SaturationConfig::from_values(Some("true"), None, None, None)
                .unwrap()
                .is_none()
        );
        let config = SaturationConfig::from_values(None, Some("0.8"), Some("70"), None)
            .unwrap()
            .unwrap();
        assert_eq!(config.throttled_fraction, 0.8);
        assert_eq!(config.pressure_avg10, 70.0);
        assert!(SaturationConfig::from_values(None, Some("1.5"), None, None).is_err());
        assert!(SaturationConfig::from_values(None, None, Some("1"), None).is_err());
        assert!(SaturationConfig::from_values(Some("maybe"), None, None, None).is_err());
    }

    #[test]
    fn forced_and_disabled_monitors_report_their_state() {
        assert!(!SaturationMonitor::disabled().saturated());
        assert!(!SaturationMonitor::disabled().enabled());
        assert!(SaturationMonitor::forced(true).saturated());
        assert!(!SaturationMonitor::forced(false).saturated());
    }

    #[test]
    fn schedstat_first_field_is_cpu_nanoseconds() {
        assert_eq!(parse_schedstat("123456789 42 7\n"), Some(123_456_789));
        assert_eq!(parse_schedstat(""), None);
        assert_eq!(parse_schedstat("abc 1 2"), None);
    }

    #[test]
    fn worker_utilization_spans_the_sampling_window() {
        let mut readings = VecDeque::new();
        assert_eq!(worker_utilization(&readings), None);
        readings.push_back((Duration::from_secs(0), 1_000_000_000));
        assert_eq!(
            worker_utilization(&readings),
            None,
            "one sample has no span"
        );
        readings.push_back((Duration::from_secs(2), 1_500_000_000));
        readings.push_back((Duration::from_secs(4), 4_400_000_000));
        let utilization = worker_utilization(&readings).unwrap();
        assert!((utilization - 0.85).abs() < 1e-9, "{utilization}");
        readings.push_back((Duration::from_secs(6), 4_000_000_000));
        assert_eq!(
            worker_utilization(&readings),
            Some(0.5),
            "a counter that went backwards (worker replaced) is clamped, not negative"
        );
    }

    #[test]
    fn worker_saturation_enters_at_the_threshold_and_leaves_below_eighty_percent_of_it() {
        assert!(!evaluate_worker(false, Some(0.84), 0.85));
        assert!(evaluate_worker(false, Some(0.85), 0.85));
        assert!(evaluate_worker(true, Some(0.70), 0.85));
        assert!(!evaluate_worker(true, Some(0.67), 0.85));
        assert!(
            !evaluate_worker(true, None, 0.85),
            "no reading clears the state"
        );
    }

    #[test]
    fn worker_utilization_threshold_is_bounded() {
        let config = SaturationConfig::from_values(None, None, None, Some(" 0.9 "))
            .unwrap()
            .unwrap();
        assert!((config.worker_utilization - 0.9).abs() < 1e-9);
        assert_eq!(
            SaturationConfig::default().worker_utilization,
            DEFAULT_WORKER_UTILIZATION
        );
        assert!(SaturationConfig::from_values(None, None, None, Some("0.1")).is_err());
        assert!(SaturationConfig::from_values(None, None, None, Some("1.5")).is_err());
        assert!(SaturationConfig::from_values(None, None, None, Some("hot")).is_err());
    }
}
