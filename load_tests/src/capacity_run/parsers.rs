//! Parse bounded Linux counters and host identity without external host commands.

use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CpuStat {
    pub(super) usage_usec: i64,
    pub(super) periods: i64,
    pub(super) throttled: i64,
}

pub(super) fn cpu_stat(source: &str) -> Result<CpuStat> {
    let values = source
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(' ')?;
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            value.parse::<i64>().ok().map(|value| (name, value))
        })
        .collect::<BTreeMap<_, _>>();
    let Some(usage_usec) = values.get("usage_usec") else {
        bail!("cpu.stat lacks usage_usec");
    };
    Ok(CpuStat {
        usage_usec: *usage_usec,
        periods: *values.get("nr_periods").unwrap_or(&0),
        throttled: *values.get("nr_throttled").unwrap_or(&0),
    })
}

pub(super) fn throttled(before: CpuStat, after: CpuStat) -> f64 {
    let periods = after.periods - before.periods;
    if periods > 0 {
        (after.throttled - before.throttled) as f64 / periods as f64
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HostTimes {
    pub(super) total: i64,
    pub(super) idle: i64,
    pub(super) steal: i64,
}

pub(super) fn proc_stat(source: &str) -> Result<HostTimes> {
    for line in source.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("cpu") {
            continue;
        }
        let mut values = parts
            .map(str::parse::<i64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        values.resize(values.len().max(8), 0);
        ensure!(
            values.iter().all(|value| *value >= 0),
            "negative /proc/stat counter"
        );
        let total = values[..8]
            .iter()
            .try_fold(0_i64, |total, value| total.checked_add(*value))
            .ok_or_else(|| anyhow::anyhow!("/proc/stat counter overflow"))?;
        let idle = values[3]
            .checked_add(values[4])
            .ok_or_else(|| anyhow::anyhow!("/proc/stat counter overflow"))?;
        return Ok(HostTimes {
            total,
            idle,
            steal: values[7],
        });
    }
    bail!("/proc/stat lacks the aggregate cpu line")
}

pub(super) fn count_cpus(source: &str) -> i64 {
    source
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu").is_some_and(|tail| {
                tail.split_once(' ').is_some_and(|(number, _)| {
                    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
        })
        .count() as i64
}

pub(super) fn cpu_model(source: &str) -> String {
    let mut fields = BTreeMap::new();
    for line in source.lines() {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        fields.entry(name.trim()).or_insert(value.trim());
    }
    if let Some(name) = fields.get("model name").filter(|name| !name.is_empty()) {
        return (*name).into();
    }
    let implementer = fields.get("CPU implementer").unwrap_or(&"").to_lowercase();
    let part = fields.get("CPU part").unwrap_or(&"").to_lowercase();
    let model = match (implementer.as_str(), part.as_str()) {
        ("0x41", "0xd08") => Some("ARM Cortex-A72"),
        ("0x41", "0xd0b") => Some("ARM Cortex-A76"),
        ("0x41", "0xd0c") => Some("ARM Neoverse-N1"),
        ("0x41", "0xd40") => Some("ARM Neoverse-V1"),
        ("0x41", "0xd49") => Some("ARM Neoverse-N2"),
        ("0x41", "0xd4f") => Some("ARM Neoverse-V2"),
        ("0x41", "0xd84") => Some("ARM Neoverse-V3"),
        ("0x41", "0xd8e") => Some("ARM Neoverse-N3"),
        ("0x48", "0xd01") => Some("HiSilicon Kunpeng 920"),
        ("0xc0", "0xac3") => Some("AmpereOne"),
        _ => None,
    };
    if let Some(model) = model {
        return model.into();
    }
    if !implementer.is_empty() && !part.is_empty() {
        let vendor = match implementer.as_str() {
            "0x41" => Some("ARM"),
            "0x48" => Some("HiSilicon"),
            "0x61" => Some("Apple"),
            "0xc0" => Some("Ampere"),
            _ => None,
        };
        return vendor.map_or_else(
            || format!("CPU implementer {implementer} part {part}"),
            |vendor| format!("{vendor} CPU part {part}"),
        );
    }
    fields
        .get("Hardware")
        .filter(|name| !name.is_empty())
        .copied()
        .unwrap_or("unknown")
        .into()
}

pub(super) fn platform(source: &str) -> String {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn memory_kib(source: &str) -> Result<i64> {
    for line in source.lines() {
        if line.starts_with("MemTotal:") {
            return Ok(line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("MemTotal lacks size"))?
                .parse()?);
        }
    }
    Ok(0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Udp {
    pub(super) in_datagrams: i64,
    pub(super) rcvbuf_errors: i64,
    pub(super) socket_drops: i64,
}

pub(super) fn udp(snmp: &str, table: &str, ports: &[i64]) -> Result<Udp> {
    let rows = snmp
        .lines()
        .filter(|line| line.starts_with("Udp:"))
        .map(|line| line.split_whitespace().skip(1).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if rows.len() != 2 {
        bail!("/proc/net/snmp lacks Udp counters");
    }
    let values = rows[0]
        .iter()
        .copied()
        .zip(rows[1].iter().map(|value| value.parse::<i64>()))
        .map(|(name, value)| Ok((name, value?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut socket_drops = 0_i64;
    for line in table.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 13
            || !columns[0].strip_suffix(':').is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            continue;
        }
        let port = columns[1].rsplit(':').next().unwrap();
        if ports.contains(&i64::from_str_radix(port, 16)?) {
            socket_drops = socket_drops
                .checked_add(columns.last().unwrap().parse::<i64>()?)
                .ok_or_else(|| anyhow::anyhow!("UDP socket drop counter overflow"))?;
        }
    }
    ensure!(
        socket_drops >= 0 && values.values().all(|value| *value >= 0),
        "negative UDP counter"
    );
    Ok(Udp {
        in_datagrams: *values.get("InDatagrams").unwrap_or(&0),
        rcvbuf_errors: *values.get("RcvbufErrors").unwrap_or(&0),
        socket_drops,
    })
}

pub(super) fn prometheus(source: &str) -> BTreeMap<String, f64> {
    source
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, value) = line.rsplit_once(' ')?;
            Some((name.into(), value.trim().parse().ok()?))
        })
        .collect()
}

pub(super) fn worker_loads(values: &BTreeMap<String, f64>) -> BTreeMap<i64, f64> {
    values
        .iter()
        .filter_map(|(name, value)| {
            let worker = name
                .strip_prefix("simplestchat_media_worker_cpu{worker=\"")?
                .strip_suffix("\"}")?;
            if worker.is_empty() || !worker.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some((worker.parse().ok()?, *value))
        })
        .collect()
}
