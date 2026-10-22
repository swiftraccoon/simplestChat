//! A fixed traffic-control script; parameters cannot introduce shell syntax.

use anyhow::{Result, ensure};

pub(super) fn script(parameters: &str, scope: &str, port: u16, workers: u32) -> Result<String> {
    ensure!(
        !parameters.is_empty()
            && parameters
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b" .%".contains(&byte)),
        "netem parameters may contain only letters, digits, spaces, dots and percent signs"
    );
    let mut filter = "match ip protocol 17 0xff".to_owned();
    if scope == "udp-downlink" {
        let span = workers
            .max(1)
            .checked_next_power_of_two()
            .ok_or_else(|| anyhow::anyhow!("invalid worker mask"))?;
        ensure!(span <= 65536, "invalid worker mask");
        ensure!(
            u32::from(port) % span == 0,
            concat!(
                "--udp-port must be a multiple of {} for downlink-only ",
                "impairment of {} workers"
            ),
            span,
            workers
        );
        filter.push_str(&format!(" match ip sport {port} 0x{:x}", 65536 - span));
    } else {
        ensure!(
            scope == "udp-both",
            "--netem-scope must be udp-both or udp-downlink"
        );
    }
    Ok([
        "set -e".into(),
        "tc qdisc add dev lo root handle 1: prio bands 3 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
            .into(),
        format!("tc qdisc add dev lo parent 1:3 handle 30: netem limit 200000 {parameters}"),
        format!("tc filter add dev lo parent 1: protocol ip prio 1 u32 {filter} flowid 1:3"),
        "tc qdisc show dev lo".into(),
    ]
    .join("\n"))
}
