//! Human summaries keep the recorded workload and lower-bound explanations.

use super::*;
use crate::capacity::format::repr;

fn describe(entry: &Ceiling, people: &str) -> String {
    if entry.bound == "not-run" {
        return "not measured".into();
    }
    if entry.bound == "below-first-size" {
        return "below the first size tried".into();
    }
    let Some(size) = entry.ceiling else {
        return "not measured (a step could not run; see its step.json)".into();
    };
    let suffix = if entry.workers == 1 { "" } else { "s" };
    let size = format!("{size} {people} on {} worker{suffix}", entry.workers);
    let reason = match entry.bound.as_str() {
        "generator-limited" => Some("the generator ran out of CPU first"),
        "step-limited" => Some("the search ran out of steps"),
        "interrupted" => Some("a later step could not run; see its step.json"),
        _ => None,
    };
    reason.map_or_else(
        || size.clone(),
        |reason| format!("at least {size} ({reason})"),
    )
}

pub(in crate::capacity_run) fn lines(report: &Report<'_>) -> Vec<String> {
    let browser = report.measurement.browser;
    let media = if browser.audio_only {
        "audio only"
    } else {
        "audio and video"
    };
    let chat = if browser.chat_interval_ms != 0 {
        format!(
            "one chat message per participant every {} s",
            general(browser.chat_interval_ms as f64 / 1000.0)
        )
    } else {
        "chat disabled".into()
    };
    let platform = if report.host.platform.is_empty() {
        String::new()
    } else {
        format!(" ({})", report.host.platform)
    };
    let mut lines = vec![
        String::new(),
        format!(
            "Host {}: {} CPUs, {}, {} MiB{platform}",
            report.label, report.host.logical_cpus, report.host.cpu_model, report.host.memory_mib
        ),
        format!(
            "Workload: meetings of {}, {media}, up to {} concurrent speakers per room, {chat}",
            report.measurement.meeting_size, browser.speakers
        ),
    ];
    for (workload, title, people) in [
        (
            Workload::Meetings,
            format!("meetings of {}", report.measurement.meeting_size),
            "participants",
        ),
        (
            Workload::LargeMeeting,
            "largest meeting".into(),
            "participants",
        ),
        (Workload::Webinar, "largest webinar".into(), "viewers"),
    ] {
        let ceiling = &report.ceilings[&workload];
        lines.push(format!("  {title}: {}", describe(ceiling, people)));
        if !ceiling.limit.is_empty() {
            lines.push(format!("    bounded by: {}", ceiling.limit));
        }
    }
    let projection = &report.projection;
    if report.ceilings[&Workload::Meetings].ceiling.is_some() {
        let estimate = if projection.lower_bound {
            "at least"
        } else {
            "about"
        };
        let memory = if projection.limited_by == "memory" {
            format!(
                "; memory holds fewer than the {} the CPUs carry)",
                projection.cpu_participants
            )
        } else {
            ")".into()
        };
        lines.push(format!(
            concat!(
                "Projected to {} app CPUs: {} {} participants in meetings ",
                "({} per core{}, {} Mbit/s each"
            ),
            repr(projection.app_cpus),
            estimate,
            projection.meeting_participants,
            repr(projection.meeting_participants_per_core),
            memory,
            repr(projection.egress_mbps_per_participant)
        ));
    } else {
        lines.push(
            "No projection: meetings were not measured, and only they scale to the deployment"
                .into(),
        );
    }
    lines.push("Settings:".into());
    lines.extend(
        report
            .recommendations
            .iter()
            .map(|line| format!("  {line}")),
    );
    if !browser.audio_only
        && report.ceilings[&Workload::LargeMeeting]
            .ceiling
            .is_some_and(|value| value > 33)
    {
        lines.push(
            concat!(
                "  note: beyond 33 publishers each browser shows only 32 peers ",
                "(MAX_CONSUMERS_PER_PARTICIPANT=64)"
            )
            .into(),
        );
    }
    if let Some(cost) = &report.cost {
        lines.push(format!(
            concat!(
                "Cost: ${:.3} per 1,000 participant-hours at full use ",
                "({}-bound, {:.1} participants per monthly dollar)"
            ),
            cost.dollars_per_1000_participant_hours,
            cost.limited_by,
            cost.participants_per_monthly_dollar
        ));
    }
    lines
}
