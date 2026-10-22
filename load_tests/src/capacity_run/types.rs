//! Workload, quota and measured-step records independent of any engine process.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Workload {
    #[default]
    Meetings,
    LargeMeeting,
    Webinar,
}

impl Workload {
    pub(super) const ALL: [Self; 3] = [Self::Meetings, Self::LargeMeeting, Self::Webinar];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Meetings => "meetings",
            Self::LargeMeeting => "large-meeting",
            Self::Webinar => "webinar",
        }
    }
}

impl std::fmt::Display for Workload {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(self.name())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Browser {
    pub(super) capture: String,
    pub(super) speakers: i64,
    pub(super) viewport_width: i64,
    pub(super) pixel_ratio: f64,
    pub(super) layout: String,
    pub(super) audio_only: bool,
    pub(super) chat_interval_ms: i64,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            capture: "720p".into(),
            speakers: 1,
            viewport_width: 1440,
            pixel_ratio: 2.0,
            layout: "classic".into(),
            audio_only: false,
            chat_interval_ms: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Shape {
    pub(super) server_cpus: f64,
    pub(super) workers: i64,
    pub(super) generator_cpus: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
    pub(super) workload: Workload,
    pub(super) size: i64,
    pub(super) meeting_size: i64,
    pub(super) warmup_seconds: i64,
    pub(super) duration_seconds: i64,
    pub(super) workers: i64,
}

impl Plan {
    pub(super) fn clients(self) -> i64 {
        self.size + i64::from(self.workload == Workload::Webinar)
    }

    pub(super) fn rooms(self) -> i64 {
        if self.workload == Workload::Meetings {
            self.size / self.meeting_size
        } else {
            1
        }
    }

    pub(super) fn join_rate(self) -> f64 {
        match self.workload {
            Workload::Meetings => 0.5 * self.workers as f64,
            Workload::LargeMeeting => 1.5,
            Workload::Webinar => 4.0,
        }
    }

    pub(super) fn ramp_seconds(self) -> i64 {
        10.max((self.clients() as f64 / self.join_rate()).ceil() as i64)
    }

    pub(super) fn window(self, started: f64) -> (f64, f64) {
        let start = started + self.ramp_seconds() as f64 + self.warmup_seconds as f64;
        (start, start + self.duration_seconds as f64 - 5.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Limits {
    pub(super) worker_load: f64,
    pub(super) drop_share: f64,
    pub(super) generator_throttled: f64,
    pub(super) server_throttled: f64,
    pub(super) memory_share: f64,
    pub(super) steal_share: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            worker_load: 0.7,
            drop_share: 0.001,
            generator_throttled: 0.05,
            server_throttled: 0.1,
            memory_share: 0.85,
            steal_share: 0.05,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StepResult {
    pub(super) workload: Workload,
    pub(super) size: i64,
    pub(super) clients: i64,
    pub(super) rooms: i64,
    pub(super) started_at: String,
    pub(super) generator_passed: bool,
    pub(super) generator_errors: i64,
    pub(super) generator_failures: Vec<String>,
    pub(super) generator_failure_count: i64,
    pub(super) failed_connections: i64,
    pub(super) failed_consumers: i64,
    pub(super) failed_video_consumers: i64,
    pub(super) viewers_at_cap: i64,
    pub(super) validated_consumers: i64,
    pub(super) refused_joins: i64,
    pub(super) worker_load_peak: BTreeMap<i64, f64>,
    pub(super) worker_load_mean: BTreeMap<i64, f64>,
    pub(super) server_cores: f64,
    pub(super) server_throttled: f64,
    pub(super) load_scrapes: i64,
    pub(super) server_saturated_scrapes: i64,
    pub(super) generator_cores: f64,
    pub(super) generator_throttled: f64,
    pub(super) generator_setup_throttled: f64,
    pub(super) host_steal: f64,
    pub(super) host_busy: f64,
    pub(super) sent_datagrams: i64,
    pub(super) socket_drops: i64,
    pub(super) namespace_datagrams: i64,
    pub(super) rcvbuf_errors: i64,
    pub(super) memory_peak_bytes: i64,
    pub(super) memory_limit_bytes: i64,
    pub(super) generator_memory_peak_bytes: i64,
    pub(super) generator_out_of_memory: bool,
    pub(super) received_packets: i64,
    pub(super) received_payload_bytes: i64,
    pub(super) measurement_seconds: f64,
    pub(super) receive_ready_p99_ms: f64,
    pub(super) video_start_p99_ms: f64,
    pub(super) server_exit: String,
    pub(super) error: String,
    pub(super) meeting_size: i64,
    pub(super) audio_only: bool,
    pub(super) speakers: i64,
    pub(super) chat_interval_ms: i64,
}

impl StepResult {
    pub(super) fn new(plan: Plan, browser: &Browser, started_at: String) -> Self {
        Self {
            workload: plan.workload,
            size: plan.size,
            clients: plan.clients(),
            rooms: plan.rooms(),
            started_at,
            meeting_size: plan.meeting_size,
            audio_only: browser.audio_only,
            speakers: browser.speakers,
            chat_interval_ms: browser.chat_interval_ms,
            ..Self::default()
        }
    }

    pub(super) fn busiest_worker(&self) -> f64 {
        self.worker_load_peak
            .values()
            .copied()
            .reduce(f64::max)
            .unwrap_or(0.0)
    }

    pub(super) fn mean_worker(&self) -> f64 {
        if self.worker_load_mean.is_empty() {
            0.0
        } else {
            self.worker_load_mean.values().sum::<f64>() / self.worker_load_mean.len() as f64
        }
    }

    pub(super) fn drop_share(&self) -> f64 {
        if self.sent_datagrams != 0 {
            self.socket_drops as f64 / self.sent_datagrams as f64
        } else if self.socket_drops != 0 {
            1.0
        } else {
            0.0
        }
    }

    pub(super) fn egress_mbps(&self) -> f64 {
        if self.measurement_seconds <= 0.0 {
            return 0.0;
        }
        let wire = i128::from(self.received_payload_bytes) + i128::from(self.received_packets) * 62;
        (wire * 8) as f64 / self.measurement_seconds / 1e6
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Verdict {
    pub(super) passed: bool,
    pub(super) valid: bool,
    pub(super) reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Trial {
    pub(super) size: i64,
    pub(super) passed: bool,
    pub(super) valid: bool,
    pub(super) busiest_worker: f64,
    pub(super) mean_worker: f64,
    pub(super) generator_cores: f64,
}

impl Trial {
    pub(super) fn from_step(step: &StepResult, verdict: &Verdict) -> Self {
        Self {
            size: step.size,
            passed: verdict.passed,
            valid: verdict.valid,
            busiest_worker: step.busiest_worker(),
            mean_worker: step.mean_worker(),
            generator_cores: step.generator_cores,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Search {
    pub(super) workload: Workload,
    pub(super) meeting_size: i64,
    pub(super) first: i64,
    pub(super) granularity: i64,
    pub(super) max_steps: usize,
    pub(super) minimum: i64,
    pub(super) audio_only: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ceiling {
    pub(super) workload: Workload,
    pub(super) ceiling: Option<i64>,
    pub(super) bound: String,
    pub(super) busiest_worker: f64,
    pub(super) egress_mbps: f64,
    pub(super) memory_peak_bytes: i64,
    pub(super) workers: i64,
    pub(super) limit: String,
}
