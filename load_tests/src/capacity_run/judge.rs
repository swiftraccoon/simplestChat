//! Keep a generator-limited trial separate from a measured server failure.

use super::types::{Limits, StepResult, Verdict, Workload};

fn video_tiles(step: &StepResult) -> i64 {
    if step.audio_only {
        return 0;
    }
    match step.workload {
        Workload::Meetings => (step.clients / step.rooms.max(1) - 1).max(0),
        Workload::LargeMeeting => (step.size - 1).clamp(0, 32),
        Workload::Webinar => 1,
    }
}

fn lost_media(step: &StepResult) -> String {
    if video_tiles(step) as f64 * 100_000.0 >= 0.95 * 3_000_000.0
        && step.viewers_at_cap != 0
        && step.failed_video_consumers == step.failed_consumers
    {
        format!(
            concat!(
                "{} video consumers lost media while {} viewers' estimates sat at ",
                "the server's 3 Mbit/s per-viewer cap: their tiles need more than a viewer ",
                "may receive, whatever the CPU"
            ),
            step.failed_consumers, step.viewers_at_cap
        )
    } else {
        format!("{} consumers lost media", step.failed_consumers)
    }
}

pub(super) fn judge(step: &StepResult, limits: Limits) -> Verdict {
    if !step.error.is_empty() {
        return Verdict {
            passed: false,
            valid: false,
            reasons: vec![step.error.clone()],
        };
    }
    let throttled = step.generator_throttled > limits.generator_throttled;
    let setup = step.generator_setup_throttled > limits.generator_throttled;
    let valid = !throttled && !setup && !step.generator_out_of_memory;
    let checks = [
        (
            throttled,
            format!(
                concat!(
                    "the generator was throttled in {:.0}% of periods, ",
                    "so it, not the server, set the pace"
                ),
                step.generator_throttled * 100.0
            ),
        ),
        (
            setup,
            format!(
                concat!(
                    "the generator was throttled in {:.0}% of periods while ",
                    "clients joined, so it, not the server, set the pace"
                ),
                step.generator_setup_throttled * 100.0
            ),
        ),
        (
            step.generator_out_of_memory,
            "the generator ran out of memory, so it, not the server, set the pace".into(),
        ),
        (
            !step.server_exit.is_empty(),
            format!("server exited: {}", step.server_exit),
        ),
        (
            step.failed_connections != 0 || step.refused_joins != 0,
            format!(
                "{} joins failed ({} refused by the worker guard)",
                step.failed_connections, step.refused_joins
            ),
        ),
        (step.failed_consumers != 0, lost_media(step)),
        (
            step.drop_share() > limits.drop_share,
            format!(
                "the kernel dropped {:.2}% of inbound datagrams",
                step.drop_share() * 100.0
            ),
        ),
        (
            step.busiest_worker() > limits.worker_load,
            format!(
                "a worker reached {:.2} cores (guard {:.2})",
                step.busiest_worker(),
                limits.worker_load
            ),
        ),
        (
            step.server_throttled > limits.server_throttled,
            format!(
                "the server's CPU quota ran out in {:.0}% of periods",
                step.server_throttled * 100.0
            ),
        ),
        (
            step.server_saturated_scrapes > 0,
            concat!(
                "the server reported itself CPU-saturated, so /ready failed ",
                "and it refused joins"
            )
            .into(),
        ),
        (
            step.memory_limit_bytes != 0
                && step.memory_peak_bytes as f64
                    > limits.memory_share * step.memory_limit_bytes as f64,
            "memory rose above the memory guard's share of the limit".into(),
        ),
    ];
    let mut reasons = checks
        .into_iter()
        .filter_map(|(failed, reason)| failed.then_some(reason))
        .collect::<Vec<_>>();
    if !step.generator_passed && reasons.is_empty() {
        if let Some(first) = step.generator_failures.first() {
            let more = step.generator_failure_count - 1;
            let suffix = if more > 0 {
                format!(" (and {more} more)")
            } else {
                String::new()
            };
            reasons.push(format!("the generator's checks failed: {first}{suffix}"));
        } else {
            reasons.push(format!(
                "the generator's own checks failed ({} errors)",
                step.generator_errors
            ));
        }
    }
    Verdict {
        passed: valid && reasons.is_empty(),
        valid,
        reasons,
    }
}
