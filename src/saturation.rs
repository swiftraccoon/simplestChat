//! CPU saturation of the process's own cgroup, from cgroup v2 throttling
//! counters and pressure stall information. When the container is at its
//! quota, admitting more calls degrades every call; readiness and fresh joins
//! consult this monitor so overload sheds new users instead.

use crate::metrics::{SaturationSnapshot, ServerMetrics};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{info, warn};

pub const DEFAULT_THROTTLED_FRACTION: f64 = 0.5;
pub const DEFAULT_PRESSURE_AVG10: f64 = 50.0;
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
    pub window: Duration,
    pub interval: Duration,
    pub cpu_stat: PathBuf,
    pub cpu_pressure: PathBuf,
}

impl Default for SaturationConfig {
    fn default() -> Self {
        Self {
            throttled_fraction: DEFAULT_THROTTLED_FRACTION,
            pressure_avg10: DEFAULT_PRESSURE_AVG10,
            window: WINDOW,
            interval: INTERVAL,
            cpu_stat: PathBuf::from("/sys/fs/cgroup/cpu.stat"),
            cpu_pressure: PathBuf::from("/sys/fs/cgroup/cpu.pressure"),
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
        )
    }

    pub(crate) fn from_values(
        disabled: Option<&str>,
        throttled_fraction: Option<&str>,
        pressure_avg10: Option<&str>,
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

struct Inner {
    saturated: AtomicBool,
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
                saturated: AtomicBool::new(false),
                enabled: false,
            }),
        }
    }

    /// A monitor pinned to one state, for tests of the paths that consult it.
    #[cfg(test)]
    pub fn forced(saturated: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                saturated: AtomicBool::new(saturated),
                enabled: true,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled
    }

    pub fn saturated(&self) -> bool {
        self.inner.saturated.load(Ordering::Relaxed)
    }

    /// Starts sampling the cgroup files on the runtime; the task ends with the
    /// last handle. Each tick publishes a snapshot to the metrics.
    pub fn spawn(config: SaturationConfig, metrics: ServerMetrics) -> Self {
        let monitor = Self {
            inner: Arc::new(Inner {
                saturated: AtomicBool::new(false),
                enabled: true,
            }),
        };
        let weak = Arc::downgrade(&monitor.inner);
        let capacity =
            (config.window.as_millis() / config.interval.as_millis().max(1)) as usize + 1;
        info!(
            throttled_fraction = config.throttled_fraction,
            pressure_avg10 = config.pressure_avg10,
            "CPU saturation monitor enabled"
        );
        tokio::spawn(async move {
            let mut readings: VecDeque<CpuStat> = VecDeque::with_capacity(capacity);
            let mut ticker = tokio::time::interval(config.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut warned = false;
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
                    inner.saturated.load(Ordering::Relaxed),
                    throttled,
                    pressure,
                    &config,
                );
                if saturated != inner.saturated.swap(saturated, Ordering::Relaxed) {
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
            SaturationConfig::from_values(Some("true"), None, None)
                .unwrap()
                .is_none()
        );
        let config = SaturationConfig::from_values(None, Some("0.8"), Some("70"))
            .unwrap()
            .unwrap();
        assert_eq!(config.throttled_fraction, 0.8);
        assert_eq!(config.pressure_avg10, 70.0);
        assert!(SaturationConfig::from_values(None, Some("1.5"), None).is_err());
        assert!(SaturationConfig::from_values(None, None, Some("1")).is_err());
        assert!(SaturationConfig::from_values(Some("maybe"), None, None).is_err());
    }

    #[test]
    fn forced_and_disabled_monitors_report_their_state() {
        assert!(!SaturationMonitor::disabled().saturated());
        assert!(!SaturationMonitor::disabled().enabled());
        assert!(SaturationMonitor::forced(true).saturated());
        assert!(!SaturationMonitor::forced(false).saturated());
    }
}
