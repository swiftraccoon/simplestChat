//! Human-readable suggestions and ranked report rows, with reference formatting.

use super::format::{general, grouped};
use super::json::Json;
use super::model::{Ceilings, Host, Profile, Suggestion};
use anyhow::{Result, ensure};

pub(super) fn suggestion(
    value: &Suggestion,
    host: Host,
    ceilings: &Ceilings,
    source: &str,
    included_egress: f64,
) -> Vec<String> {
    let per_person = ceilings
        .get("meetings")
        .and_then(|entry| {
            entry
                .size
                .filter(|size| *size != 0)
                .map(|size| entry.egress_mbps / size as f64)
        })
        .unwrap_or(0.0);
    let monthly_tb = value.meetings as f64 * per_person * 3600.0 * 730.0 / 8.0 / 1e6;
    let mut lines = vec![
        format!(
            "Sizing a {}-vCPU, {} GiB host with a {} Mbit/s port ({source}):",
            host.vcpus,
            general(host.memory_mib as f64 / 1024.0),
            general(host.port_mbps)
        ),
        String::new(),
    ];
    lines.extend(value.settings());
    lines.extend([
        String::new(),
        format!(
            concat!(
                "The app gets {} CPUs ({} media workers) and {} MiB; the rest serves ",
                "the database, the proxy, TURN and the system."
            ),
            general(value.deployment.cpus),
            value.deployment.workers(),
            value.deployment.memory_mib
        ),
        format!(
            concat!(
                "Meetings of five: about {} participants ({}-bound; {:.2} Mbit/s of ",
                "egress each, {:.0} Mbit/s in all)."
            ),
            value.meetings,
            value.meeting_limit,
            per_person,
            value.meetings as f64 * per_person
        ),
        format!(
            concat!(
                "Webinars: about {} viewers across the workers ({}-bound; ",
                "one room spreads over every worker)."
            ),
            value.viewers, value.viewer_limit
        ),
        format!(
            concat!(
                "All-publishing rooms: {} people, whatever the host: every viewer ",
                "downloads every tile within the 3 Mbit/s per-viewer cap, which ",
                "MAX_BROADCASTERS_PER_ROOM=30 keeps in reach; MAX_PARTICIPANTS_PER_ROOM ",
                "follows the webinar instead."
            ),
            value.room
        ),
        concat!(
            "MAX_CONNECTIONS counts the lightest participants; heavier shapes meet ",
            "the per-worker CPU ",
            "guard (0.7 cores) first. SIMPLESTCHAT_MEMORY_LIMIT is the app's share of the host, a ",
            "process ceiling, not a memory admission guard; monitor RSS and measure the intended ",
            "workload before raising these estimates."
        )
        .into(),
        format!(
            "Open UDP 40000-{} at the firewall: one port per worker.",
            40000 + value.deployment.workers() - 1
        ),
        format!(
            concat!(
                "Egress is the next wall after CPU: check the provider's quota ",
                "and price against the Mbit/s above (a full month of meetings at that rate ",
                "is {:.1} TB)."
            ),
            monthly_tb
        ),
    ]);
    let per_viewer = ceilings
        .get("webinar")
        .and_then(|entry| {
            entry
                .size
                .filter(|size| *size != 0)
                .map(|size| entry.egress_mbps / size as f64)
        })
        .unwrap_or(0.0);
    if included_egress > 0.0 && per_person > 0.0 && per_viewer > 0.0 {
        let bits = included_egress * 1e12 * 8.0;
        let participant_hours = bits / (per_person * 1e6) / 3600.0;
        let viewer_hours = bits / (per_viewer * 1e6) / 3600.0;
        lines.push(format!(
            concat!(
                "A transfer allowance of {} TB a month is about {} ",
                "meeting participant-hours or {} webinar viewer-hours: {:.0} meeting ",
                "participants or {:.0} viewers around the clock, and the host's {} meeting ",
                "participants use it in {:.0} hours. Providers usually count both directions, ",
                "and a meeting participant uploads about as much as they download, so meetings ",
                "spend it faster than this."
            ),
            general(included_egress),
            grouped(participant_hours),
            grouped(viewer_hours),
            participant_hours / 730.0,
            viewer_hours / 730.0,
            value.meetings,
            participant_hours / value.meetings.max(1) as f64
        ));
    }
    lines
}

fn ceiling(entry: &Json) -> Result<String> {
    entry.object("ceiling")?;
    let value = &entry["ceiling"];
    if matches!(value, Json::Null) {
        return Ok("-".into());
    }
    let lower = matches!(
        entry["bound"].string(),
        Some("generator-limited" | "step-limited" | "interrupted")
    );
    Ok(format!(
        "{}{}",
        if lower { "≥" } else { "" },
        value.display()
    ))
}

pub(super) fn comparison_row(report: &Json) -> Result<(f64, String)> {
    let host = &report["host"];
    host.object("host")?;
    let projection = &report["projection"];
    projection.object("projection")?;
    let ceilings = &report["ceilings"];
    ceilings.object("ceilings")?;
    for (value, fields) in [
        (report, &["label"][..]),
        (host, &["logicalCpus", "cpuModel"][..]),
        (
            projection,
            &["meetingParticipants", "egressMbpsPerParticipant"][..],
        ),
    ] {
        for field in fields {
            ensure!(value.get(field).is_some(), "missing report field: {field}");
        }
    }
    let room = ceiling(&ceilings["large-meeting"])?;
    let webinar = ceiling(&ceilings["webinar"])?;
    let mut meetings = format!(
        "{}{}",
        if projection["lowerBound"].is_true() {
            "≥"
        } else {
            ""
        },
        projection["meetingParticipants"].display()
    );
    ceilings["meetings"].object("meetings")?;
    if matches!(ceilings["meetings"]["ceiling"], Json::Null) {
        meetings = "-".into();
    }
    let mut price = "-".into();
    let mut per_1000 = f64::INFINITY;
    if matches!(report["cost"], Json::Object(_)) {
        let cost = &report["cost"];
        per_1000 = cost["dollarsPer1000ParticipantHours"].number("cost")?;
        cost["prices"].object("prices")?;
        price = format!("${}", general(cost["prices"]["monthly"].number("price")?));
    }
    let dollars = if per_1000.is_finite() {
        format!("{per_1000:.3}")
    } else {
        "-".into()
    };
    let model = host["cpuModel"]
        .display()
        .chars()
        .take(28)
        .collect::<String>();
    let row = format!(
        "{:<24} {:>4} {:<28} {:>8} {:>6} {:>5} {:>6} {:>6} {:>9}",
        report["label"].display(),
        host["logicalCpus"].display(),
        model,
        price,
        meetings,
        room,
        webinar,
        projection["egressMbpsPerParticipant"].display(),
        dollars
    );
    Ok((per_1000, row))
}

pub(super) fn compare(reports: &[Json]) -> Result<String> {
    let profiles = reports
        .iter()
        .map(Profile::read)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        profiles
            .first()
            .is_none_or(|first| profiles.iter().skip(1).all(|profile| profile == first)),
        concat!(
            "cannot compare costs across different workload shapes; use reports with ",
            "the same meeting size, media, speakers, browser settings and chat interval"
        )
    );
    let mut rows = reports
        .iter()
        .map(comparison_row)
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|(left_cost, left), (right_cost, right)| {
        match left_cost.partial_cmp(right_cost) {
            Some(std::cmp::Ordering::Equal) => left.cmp(right),
            Some(order) => order,
            None => std::cmp::Ordering::Equal,
        }
    });
    let mut output = format!(
        "{:<24} {:>4} {:<28} {:>8} {:>6} {:>5} {:>6} {:>6} {:>9}\n",
        "host", "cpus", "model", "price", "meet", "room", "webin", "Mb/s", "$/1k ph"
    );
    for (_, row) in rows {
        output.push_str(&row);
        output.push('\n');
    }
    if let Some(profile) = profiles.first() {
        output.push_str(&format!("Workload: {}\n", profile.describe()));
    }
    Ok(output)
}
