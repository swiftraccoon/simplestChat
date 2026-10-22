//! Thread groups remain descriptive; cgroup usage includes threads that exited.

use super::parsers::ServerSample;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::BTreeMap;

pub(super) fn group(name: &str) -> &'static str {
    if name.starts_with("mediasoup-worke") {
        "mediasoupWorkers"
    } else if name.starts_with("tokio-runtime-w") || name.starts_with("tokio-rt-worker") {
        "tokioWorkers"
    } else {
        "other"
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Cgroup {
    pub(super) usage_seconds: f64,
    pub(super) periods: f64,
    pub(super) throttled_periods: f64,
    pub(super) throttled_seconds: f64,
    pub(super) cpu_percent_of_quota: f64,
    pub(super) throttled_period_fraction: Option<f64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Attribution {
    pub(super) sampled_duration_seconds: f64,
    pub(super) thread_cpu_seconds: BTreeMap<String, f64>,
    pub(super) cgroup: Cgroup,
}

pub(super) fn summarize(
    samples: &[ServerSample],
    start: f64,
    end: f64,
    ticks: f64,
    quota: f64,
) -> Result<Attribution> {
    let window = samples
        .iter()
        .filter(|s| s.elapsed_ms >= start && s.elapsed_ms <= end)
        .filter_map(|s| {
            s.server_detail
                .as_ref()
                .map(|detail| (s.elapsed_ms, detail))
        })
        .collect::<Vec<_>>();
    ensure!(
        window.len() >= 2,
        "Insufficient server detail samples in measurement window"
    );
    let (first, a) = window[0];
    let (last, b) = window[window.len() - 1];
    let seconds = (last - first) / 1000.0;
    let mut groups = BTreeMap::new();
    for (name, ticks_now) in &b.threads {
        *groups.entry(group(name).into()).or_insert(0.0) +=
            (ticks_now - a.threads.get(name).unwrap_or(&0.0)) / ticks;
    }
    let usage = (b.cgroup.get("usage_usec").copied().unwrap_or(f64::NAN)
        - a.cgroup.get("usage_usec").copied().unwrap_or(f64::NAN))
        / 1e6;
    let periods = b.cgroup.get("nr_periods").copied().unwrap_or(f64::NAN)
        - a.cgroup.get("nr_periods").copied().unwrap_or(f64::NAN);
    let throttled = b.cgroup.get("nr_throttled").copied().unwrap_or(f64::NAN)
        - a.cgroup.get("nr_throttled").copied().unwrap_or(f64::NAN);
    Ok(Attribution {
        sampled_duration_seconds: seconds,
        thread_cpu_seconds: groups,
        cgroup: Cgroup {
            usage_seconds: usage,
            periods,
            throttled_periods: throttled,
            throttled_seconds: (b.cgroup.get("throttled_usec").copied().unwrap_or(f64::NAN)
                - a.cgroup.get("throttled_usec").copied().unwrap_or(f64::NAN))
                / 1e6,
            cpu_percent_of_quota: usage / seconds / quota * 100.0,
            throttled_period_fraction: if periods != 0.0 {
                Some(throttled / periods)
            } else {
                None
            },
        },
    })
}
