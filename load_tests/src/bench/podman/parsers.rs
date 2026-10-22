//! Linux sampler blocks preserve process, cgroup, thread and socket provenance.

use crate::bench::{
    options::number,
    resources::{ProcessSample, linux_sample},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) fn thread_ticks(dump: &str) -> BTreeMap<String, f64> {
    let mut result = BTreeMap::new();
    for line in dump.lines() {
        let Some(close) = line.rfind(')') else {
            continue;
        };
        let open = line.find('(').map_or(0, |index| index + 1);
        if open > close {
            continue;
        }
        let name = &line[open..close];
        let fields = line
            .get(close + 2..)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>();
        let ticks = fields
            .get(11)
            .and_then(|v| number(v))
            .zip(fields.get(12).and_then(|v| number(v)))
            .map(|(user, system)| user + system)
            .filter(|value| value.is_finite());
        if let Some(ticks) = ticks {
            *result.entry(name.into()).or_insert(0.0) += ticks;
        }
    }
    result
}

pub(super) fn cgroup_cpu_stat(source: &str) -> Result<BTreeMap<String, f64>> {
    let mut values = BTreeMap::new();
    for line in source.split('\n').filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let key = fields.next().unwrap_or("");
        values.insert(
            key.into(),
            fields.next().and_then(number).unwrap_or(f64::NAN),
        );
    }
    for key in ["usage_usec", "nr_periods", "nr_throttled", "throttled_usec"] {
        ensure!(
            values.get(key).is_some_and(|value| value.is_finite()),
            "cpu.stat lacks {key}"
        );
    }
    Ok(values)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Detail {
    pub(super) cgroup: BTreeMap<String, f64>,
    pub(super) threads: BTreeMap<String, f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerBlock {
    pub(super) server: ProcessSample,
    pub(super) server_detail: Detail,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerSample {
    pub(super) elapsed_ms: f64,
    pub(super) server: Option<ProcessSample>,
    pub(super) server_detail: Option<Detail>,
}

pub(super) fn server_block(block: &str, clock_ticks: f64, page_size: f64) -> Result<ServerBlock> {
    let mut pieces = block.split("@@S\n");
    let stat = pieces.next().unwrap_or("");
    let (cpu, tasks) = pieces
        .next()
        .unwrap_or("")
        .split_once("@@Y\n")
        .unwrap_or(("", ""));
    Ok(ServerBlock {
        server: linux_sample(stat, clock_ticks, page_size)?,
        server_detail: Detail {
            cgroup: cgroup_cpu_stat(cpu)?,
            threads: thread_ticks(tasks),
        },
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Mapping {
    pub(super) range: String,
    pub(super) rss_ki_b: f64,
    pub(super) name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MemoryBlock {
    pub(super) rss_ki_b: f64,
    pub(super) anonymous_ki_b: f64,
    pub(super) mappings: Vec<Mapping>,
    pub(super) cgroup: BTreeMap<String, f64>,
}

pub(super) fn memory_block(block: &str) -> Result<MemoryBlock> {
    let (rollup, remainder) = block.split_once("@@M\n").unwrap_or((block, ""));
    let (mapping, cgroup) = remainder.split_once("@@C\n").unwrap_or((remainder, ""));
    let mut values = BTreeMap::new();
    for line in rollup.lines() {
        let Some((key, rest)) = line.trim().split_once(':') else {
            continue;
        };
        if !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if parts.len() == 2
                && parts[1] == "kB"
                && !parts[0].is_empty()
                && parts[0].bytes().all(|byte| byte.is_ascii_digit())
            {
                values.insert(key, number(parts[0]).unwrap_or(f64::NAN));
            }
        }
    }
    let rss = values.get("Rss").copied().unwrap_or(f64::NAN);
    let anonymous = values.get("Anonymous").copied().unwrap_or(f64::NAN);
    ensure!(
        rss.is_finite() && anonymous.is_finite(),
        "Invalid smaps_rollup"
    );
    let mut mappings = Vec::new();
    for line in mapping.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 2 {
            continue;
        }
        let Some((a, b)) = columns[0].split_once('-') else {
            continue;
        };
        if [a, b].iter().all(|v| {
            !v.is_empty()
                && v.bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            mappings.push(Mapping {
                range: columns[0].into(),
                rss_ki_b: number(columns[1]).unwrap_or(f64::NAN),
                name: columns[2..].join(" "),
            });
        }
    }
    let mut counters = BTreeMap::new();
    for line in cgroup.lines() {
        let Some((key, value)) = line.trim().split_once(' ') else {
            continue;
        };
        if !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            && !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
        {
            counters.insert(key, number(value).unwrap_or(f64::NAN));
        }
    }
    ensure!(
        counters.get("anon").is_some_and(|value| value.is_finite()),
        "Invalid memory.stat"
    );
    Ok(MemoryBlock {
        rss_ki_b: rss,
        anonymous_ki_b: anonymous,
        mappings,
        cgroup: ["anon", "file", "kernel", "sock", "slab"]
            .into_iter()
            .map(|key| (key.into(), *counters.get(key).unwrap_or(&0.0)))
            .collect(),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Socket {
    pub(super) port: f64,
    pub(super) rx_queue_bytes: f64,
    pub(super) drops: f64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Udp {
    pub(super) in_datagrams: f64,
    pub(super) in_errors: f64,
    pub(super) rcvbuf_errors: f64,
    pub(super) sockets: Vec<Socket>,
    pub(super) server_socket_drops: f64,
}
fn hex(value: &str) -> f64 {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let mut any = false;
    let mut result = 0.0;
    for digit in value.chars().map_while(|ch| ch.to_digit(16)) {
        any = true;
        result = result * 16.0 + digit as f64;
    }
    if any { result } else { f64::NAN }
}
pub(super) fn netns_udp(text: &str, ports: &[u16]) -> Result<Udp> {
    let lines = text.lines().collect::<Vec<_>>();
    let udp = lines
        .iter()
        .filter(|line| line.starts_with("Udp:"))
        .collect::<Vec<_>>();
    ensure!(udp.len() >= 2, "Missing Udp lines in /proc/net/snmp");
    let names = udp[0].split_whitespace().skip(1);
    let values = udp[1].split_whitespace().skip(1).collect::<Vec<_>>();
    let totals = names
        .enumerate()
        .map(|(index, name)| {
            (
                name,
                values
                    .get(index)
                    .and_then(|v| number(v))
                    .unwrap_or(f64::NAN),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for key in ["InDatagrams", "InErrors", "RcvbufErrors"] {
        ensure!(
            totals.get(key).is_some_and(|value| value.is_finite()),
            "Missing Udp {key} counter"
        );
    }
    let mut sockets = Vec::new();
    for line in lines {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 13
            || !columns[0].strip_suffix(':').is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            continue;
        }
        let port = hex(columns[1].split(':').nth(1).unwrap_or(""));
        if !ports.iter().any(|wanted| f64::from(*wanted) == port) {
            continue;
        }
        sockets.push(Socket {
            port,
            rx_queue_bytes: hex(columns[4].split(':').nth(1).unwrap_or("")),
            drops: columns
                .last()
                .and_then(|value| number(value))
                .unwrap_or(f64::NAN),
        });
    }
    Ok(Udp {
        in_datagrams: totals["InDatagrams"],
        in_errors: totals["InErrors"],
        rcvbuf_errors: totals["RcvbufErrors"],
        server_socket_drops: sockets.iter().map(|socket| socket.drops).sum(),
        sockets,
    })
}
