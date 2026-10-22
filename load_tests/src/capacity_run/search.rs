//! Quotas, deterministic stream projection, bracketing and measured ceiling bounds.

use super::types::{Ceiling, Limits, Search, Shape, StepResult, Trial, Verdict, Workload};

pub(super) fn shape(
    workload: Workload,
    host: i64,
    deployment_workers: i64,
    server: Option<f64>,
    generator: Option<f64>,
) -> Shape {
    let server = server.unwrap_or_else(|| {
        if workload == Workload::LargeMeeting {
            2.min((host / 2).max(1)).min(deployment_workers.max(1)) as f64
        } else {
            (host as f64 * 0.25).round_ties_even().max(1.0)
        }
    });
    let generator = generator.unwrap_or_else(|| {
        format!("{:.1}", host as f64 - server - 0.2)
            .parse::<f64>()
            .unwrap()
            .max(0.5)
    });
    Shape {
        server_cpus: server,
        workers: server.floor().clamp(1.0, 64.0) as i64,
        generator_cpus: generator,
    }
}

pub(super) fn streams(workload: Workload, size: i64, meeting: i64, audio: bool) -> f64 {
    let kinds = if audio { 1 } else { 2 };
    let per_client = match workload {
        Workload::Meetings => (kinds * (meeting - 1)).min(64),
        Workload::LargeMeeting => (kinds * (size - 1)).min(64),
        Workload::Webinar => kinds,
    };
    size as f64 * per_client as f64
}

fn round_down(size: f64, step: i64) -> i64 {
    step.max((size / step as f64).floor() as i64 * step)
}

pub(super) fn search(
    workload: Workload,
    shape: Shape,
    meeting_size: i64,
    max_steps: usize,
    first: Option<i64>,
    audio_only: bool,
) -> Search {
    let (first, granularity, minimum) = match workload {
        Workload::Meetings => (
            first
                .filter(|size| *size != 0)
                .map_or(meeting_size * 2 * shape.workers, |size| {
                    round_down(size as f64, meeting_size)
                }),
            meeting_size,
            meeting_size,
        ),
        Workload::LargeMeeting => (first.filter(|size| *size != 0).unwrap_or(8), 1, 2),
        Workload::Webinar => (
            first
                .filter(|size| *size != 0)
                .unwrap_or(25 * shape.workers),
            5,
            5,
        ),
    };
    Search {
        workload,
        meeting_size,
        first,
        granularity,
        max_steps,
        minimum,
        audio_only,
    }
}

pub(super) fn projected(search: Search, trial: Trial, limits: Limits, generator: f64) -> i64 {
    let streams = |size| {
        streams(
            search.workload,
            size,
            search.meeting_size,
            search.audio_only,
        )
    };
    let load = if search.workload != Workload::LargeMeeting {
        trial.mean_worker * 1.15
    } else {
        trial.busiest_worker
    };
    let target = streams(trial.size) * limits.worker_load * 0.95 / load.max(0.01);
    let mut size = trial.size;
    while streams(size + 1) <= target && size < trial.size * 20 {
        size += 1;
    }
    if trial.generator_cores > 0.0 {
        let per_stream = trial.generator_cores / streams(trial.size);
        while size > trial.size && per_stream * streams(size) > generator * 0.85 {
            size -= 1;
        }
    }
    size
}

pub(super) fn next(
    search: Search,
    trials: &[Trial],
    limits: Limits,
    generator: f64,
) -> Option<i64> {
    if trials.is_empty() {
        return Some(search.first);
    }
    if trials.len() >= search.max_steps || !trials.last()?.valid {
        return None;
    }
    let lo = trials
        .iter()
        .filter(|trial| trial.passed)
        .map(|trial| trial.size)
        .max();
    let hi = trials
        .iter()
        .filter(|trial| !trial.passed)
        .map(|trial| trial.size)
        .min();
    let candidate = match (lo, hi) {
        (Some(lo), Some(hi)) => {
            if hi - lo <= search.granularity.max((lo as f64 * 0.1) as i64) {
                return None;
            }
            round_down((lo as f64 + hi as f64) / 2.0, search.granularity)
        }
        (None, Some(hi)) => {
            let candidate = round_down(hi as f64 * 0.6, search.granularity);
            if candidate < search.minimum {
                return None;
            }
            candidate
        }
        (Some(_), None) => {
            let best = *trials
                .iter()
                .filter(|trial| trial.passed)
                .max_by_key(|trial| trial.size)?;
            let grown = projected(search, best, limits, generator)
                .max((best.size as f64 * 1.15).ceil() as i64);
            let candidate = round_down(grown as f64, search.granularity);
            if best.generator_cores > 0.0 {
                let ceiling = projected(
                    search,
                    best,
                    Limits {
                        worker_load: 1e9,
                        ..limits
                    },
                    generator,
                );
                candidate.min(round_down(ceiling as f64, search.granularity))
            } else {
                candidate
            }
        }
        (None, None) => return None,
    };
    if trials.iter().any(|trial| trial.size == candidate) || lo.is_some_and(|lo| candidate <= lo) {
        None
    } else {
        Some(candidate)
    }
}

pub(super) fn ceiling(
    workload: Workload,
    history: &[(StepResult, Verdict)],
    generator: f64,
    workers: i64,
) -> Ceiling {
    let empty = |bound: &str| Ceiling {
        workload,
        ceiling: None,
        bound: bound.into(),
        busiest_worker: 0.0,
        egress_mbps: 0.0,
        memory_peak_bytes: 0,
        workers,
        limit: String::new(),
    };
    if history.is_empty() {
        return empty("not-run");
    }
    let Some((best, _)) = history
        .iter()
        .filter(|(_, verdict)| verdict.passed)
        .max_by_key(|(step, _)| step.size)
    else {
        return empty(if history.iter().any(|(_, verdict)| verdict.valid) {
            "below-first-size"
        } else {
            "interrupted"
        });
    };
    let above = history
        .iter()
        .filter(|(step, verdict)| verdict.valid && !verdict.passed && step.size > best.size)
        .min_by_key(|(step, _)| step.size);
    let last = history.last().unwrap();
    let bound = if above.is_some() {
        "measured"
    } else if !last.1.valid {
        if !last.0.error.is_empty() {
            "interrupted"
        } else {
            "generator-limited"
        }
    } else if best.generator_cores * 1.15 > 0.85 * generator {
        "generator-limited"
    } else {
        "step-limited"
    };
    Ceiling {
        workload,
        ceiling: Some(best.size),
        bound: bound.into(),
        busiest_worker: best.busiest_worker(),
        egress_mbps: best.egress_mbps(),
        memory_peak_bytes: best.memory_peak_bytes,
        workers,
        limit: above.map_or_else(String::new, |(_, verdict)| verdict.reasons.join("; ")),
    }
}
