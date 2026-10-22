//! Typed calibration reports, unchanged projection math and honest ceiling bounds.

use super::{
    context::Context,
    host::{self, Host, Images},
    options::Options,
    types::{Ceiling, Limits, Shape, StepResult, Verdict, Workload},
};
use crate::capacity::{format::general, model};
use anyhow::Result;
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::collections::BTreeMap;

pub(super) type Results = BTreeMap<Workload, Vec<(StepResult, Verdict)>>;
pub(super) type Ceilings = BTreeMap<Workload, Ceiling>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Deployment {
    pub(super) app_cpus: f64,
    pub(super) memory_mib: i64,
    pub(super) worker_threshold: f64,
    pub(super) workers: i64,
}
impl Deployment {
    pub(super) fn new(host: &Host, options: &Options) -> Result<Self> {
        let value = model::deployment(
            host.logical_cpus,
            host.memory_mib,
            options.app_cpus,
            options.app_memory_mib,
        )?;
        Ok(Self {
            app_cpus: value.cpus,
            memory_mib: value.memory_mib,
            worker_threshold: options.worker_threshold,
            workers: value.workers(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Projection {
    app_cpus: f64,
    meeting_participants_per_core: f64,
    cpu_participants: i64,
    meeting_participants: i64,
    limited_by: &'static str,
    lower_bound: bool,
    egress_mbps_per_participant: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Prices {
    monthly: f64,
    egress_per_gb: f64,
    included_egress_tb: f64,
    port_mbps: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Cost {
    participants: i64,
    limited_by: &'static str,
    egress_gb_per_participant_hour: f64,
    dollars_per_1000_participant_hours: f64,
    participants_per_monthly_dollar: f64,
    prices: Prices,
}

struct Environment<'a>(&'a [(String, String)]);
impl Serialize for Environment<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut entries: Vec<(&str, &str)> = Vec::new();
        for (key, value) in self.0 {
            if let Some((_, previous)) = entries.iter_mut().find(|(name, _)| *name == key) {
                *previous = value;
            } else {
                entries.push((key, value));
            }
        }
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement<'a> {
    shapes: &'a BTreeMap<Workload, Shape>,
    joins_per_second: BTreeMap<Workload, f64>,
    server_memory: &'a str,
    generator_memory: &'a str,
    server_env: Environment<'a>,
    browser: &'a super::types::Browser,
    limits: Limits,
    meeting_size: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Step<'a> {
    #[serde(flatten)]
    result: &'a StepResult,
    passed: bool,
    valid: bool,
    reasons: &'a [String],
    busiest_worker: f64,
    drop_share: f64,
    egress_mbps: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report<'a> {
    schema_version: u32,
    label: String,
    finished_at: String,
    host: &'a Host,
    images: &'a Images,
    measurement: Measurement<'a>,
    ceilings: &'a Ceilings,
    deployment: &'a Deployment,
    projection: Projection,
    recommendations: Vec<String>,
    cost: Option<Cost>,
    steps: Vec<Step<'a>>,
}

fn cost(projection: &model::Projection, options: &Options) -> Option<Cost> {
    let monthly = options.monthly_price?;
    if projection.participants == 0 {
        return None;
    }
    let mut limited_by = projection.limited_by;
    let mut participants = projection.participants;
    let mbps = projection.mbps_per_participant;
    if options.port_mbps > 0.0 && mbps > 0.0 {
        let network = (options.port_mbps * 0.8 / mbps) as i64;
        if network < participants {
            participants = network;
            limited_by = "network";
        }
    }
    let gb = mbps * 3600.0 / 8.0 / 1000.0;
    let hours = participants as f64 * 730.0;
    let overage = (hours * gb - options.included_egress_tb * 1000.0).max(0.0);
    let price = monthly + overage * options.egress_price_per_gb;
    Some(Cost {
        participants,
        limited_by,
        egress_gb_per_participant_hour: gb,
        dollars_per_1000_participant_hours: if hours != 0.0 {
            price / hours * 1000.0
        } else {
            f64::INFINITY
        },
        participants_per_monthly_dollar: if monthly != 0.0 {
            participants as f64 / monthly
        } else {
            f64::INFINITY
        },
        prices: Prices {
            monthly,
            egress_per_gb: options.egress_price_per_gb,
            included_egress_tb: options.included_egress_tb,
            port_mbps: options.port_mbps,
        },
    })
}

fn recommendations(
    ceilings: &Ceilings,
    projection: &model::Projection,
    deployment: &Deployment,
    rmem: i64,
) -> Vec<String> {
    let mut lines = vec![
        format!("SIMPLESTCHAT_CPUS={}", general(deployment.app_cpus)),
        format!("MEDIA_WORKERS={}", deployment.workers),
    ];
    if let Some(size) = ceilings
        .get(&Workload::LargeMeeting)
        .and_then(|value| value.ceiling)
        .filter(|value| *value != 0)
    {
        lines.push(format!(
            "MAX_PARTICIPANTS_PER_ROOM={}",
            (size as f64 * 0.9).floor().clamp(2.0, 10000.0) as i64
        ));
    }
    if projection.memory_limit_mib != 0 {
        lines.push(format!(
            "SIMPLESTCHAT_MEMORY_LIMIT={}m",
            projection.memory_limit_mib
        ));
    }
    if deployment.worker_threshold != 0.7 {
        lines.push(format!(
            "CPU_SATURATION_WORKER_UTILIZATION={}",
            general(deployment.worker_threshold)
        ));
    }
    if rmem < 2_097_152 {
        lines.push(
            concat!(
                "host sysctl: net.core.rmem_max=2097152 net.core.wmem_max=2097152 ",
                "(the workers ask for 1 MiB socket buffers)"
            )
            .into(),
        );
    }
    lines
}

pub(super) struct Measured<'a> {
    pub(super) context: &'a Context<'a>,
    pub(super) host: &'a Host,
    pub(super) images: &'a Images,
    pub(super) deployment: &'a Deployment,
    pub(super) shapes: &'a BTreeMap<Workload, Shape>,
    pub(super) limits: Limits,
}

pub(super) fn build<'a>(
    options: &Options,
    measured: Measured<'a>,
    results: &'a Results,
    ceilings: &'a Ceilings,
) -> Result<Report<'a>> {
    let Measured {
        context,
        host,
        images,
        deployment,
        shapes,
        limits,
    } = measured;
    let sizing = ceilings
        .iter()
        .map(|(workload, value)| {
            (
                workload.name().into(),
                model::Ceiling {
                    size: value.ceiling,
                    egress_mbps: value.egress_mbps,
                    memory_bytes: value.memory_peak_bytes,
                    workers: value.workers,
                },
            )
        })
        .collect();
    let projection = model::project(
        &sizing,
        model::Deployment {
            cpus: deployment.app_cpus,
            memory_mib: deployment.memory_mib,
        },
    )?;
    let recommendations = recommendations(ceilings, &projection, deployment, host.rmem_max);
    let cost = cost(&projection, options);
    let lower_bound = ceilings.get(&Workload::Meetings).is_none_or(|entry| {
        entry.ceiling.is_none_or(|value| value == 0) || entry.bound != "measured"
    });
    let projection = Projection {
        app_cpus: deployment.app_cpus,
        meeting_participants_per_core: host::rounded(projection.per_core, 2),
        cpu_participants: projection.cpu_participants,
        meeting_participants: projection.participants,
        limited_by: projection.limited_by,
        lower_bound,
        egress_mbps_per_participant: host::rounded(projection.mbps_per_participant, 3),
    };
    let measurement = Measurement {
        shapes,
        joins_per_second: shapes
            .iter()
            .map(|(workload, shape)| {
                (
                    *workload,
                    super::types::Plan {
                        workload: *workload,
                        size: 1,
                        meeting_size: options.meeting_size,
                        warmup_seconds: 0,
                        duration_seconds: 0,
                        workers: shape.workers,
                    }
                    .join_rate(),
                )
            })
            .collect(),
        server_memory: &context.server_memory,
        generator_memory: &context.generator_memory,
        server_env: Environment(context.server_env),
        browser: context.browser,
        limits,
        meeting_size: options.meeting_size,
    };
    Ok(Report {
        schema_version: 1,
        label: if options.label.is_empty() {
            if host.platform.is_empty() {
                host.cpu_model.clone()
            } else {
                host.platform.clone()
            }
        } else {
            options.label.clone()
        },
        finished_at: context.clock.utc(),
        host,
        images,
        measurement,
        ceilings,
        deployment,
        projection,
        recommendations,
        cost,
        steps: results
            .values()
            .flatten()
            .map(|(step, verdict)| Step {
                result: step,
                passed: verdict.passed,
                valid: verdict.valid,
                reasons: &verdict.reasons,
                busiest_worker: step.busiest_worker(),
                drop_share: step.drop_share(),
                egress_mbps: step.egress_mbps(),
            })
            .collect(),
    })
}

#[path = "summary.rs"]
mod summary;
pub(super) use summary::lines as summary;
