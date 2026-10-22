//! Production-shape options and the loopback join-admission preflight.

use crate::bench::options::{Plan, Scenario, number};
use crate::bench::subscription::GraphRequest;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Workload {
    pub(super) name: String,
    pub(super) clients: u32,
    pub(super) rooms: u32,
    pub(super) mode: String,
    pub(super) source_addresses: u32,
    pub(super) extra: Vec<String>,
}
impl Workload {
    pub(super) fn scenario(&self) -> Scenario {
        Scenario {
            name: self.name.clone(),
            clients: self.clients,
            rooms: self.rooms,
            mode: self.mode.clone(),
            extra: self.extra.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Options {
    pub(super) server_image: String,
    pub(super) generator_image: String,
    pub(super) output: PathBuf,
    pub(super) netem_image: String,
    pub(super) capture_handshakes: bool,
    pub(super) capture_image: String,
    pub(super) subscription_plan: Plan,
    pub(super) subscription_seed: Option<u32>,
    pub(super) duration: u32,
    pub(super) warmup: u32,
    pub(super) ramp_up: u32,
    pub(super) repetitions: u32,
    pub(super) workers: u32,
    pub(super) port: u16,
    pub(super) udp_port: u16,
    pub(super) max_connections: u32,
    pub(super) source_addresses: u32,
    pub(super) cpus: f64,
    pub(super) generator_cpus: f64,
    pub(super) memory: String,
    pub(super) clients: Vec<u32>,
    pub(super) scenarios: Vec<Workload>,
    pub(super) netem: Option<String>,
    pub(super) netem_scope: String,
    pub(super) server_env: BTreeMap<String, String>,
    pub(super) label: Option<String>,
    pub(super) source_revision: Option<String>,
}
impl Options {
    pub(super) fn graph(&self) -> GraphRequest {
        GraphRequest {
            subscription_plan: self.subscription_plan,
            subscription_seed: self.subscription_seed,
            duration: self.duration,
            warmup: self.warmup,
            ramp_up: self.ramp_up,
        }
    }
}

fn absolute(cwd: &Path, value: &str) -> PathBuf {
    let source = cwd.join(value);
    let mut result = PathBuf::new();
    for component in source.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(component),
        }
    }
    result
}
fn integer(
    raw: &BTreeMap<String, String>,
    name: &str,
    default: u32,
    low: u32,
    high: u32,
) -> Result<u32> {
    let value = raw
        .get(name)
        .map_or(Some(default as f64), |value| number(value));
    ensure!(
        value.is_some_and(|value| value.fract() == 0.0
            && value >= low as f64
            && value <= high as f64),
        "--{name} must be {low}–{high}"
    );
    Ok(value.unwrap() as u32)
}
fn quota(raw: &BTreeMap<String, String>, name: &str, default: f64, high: f64) -> Result<f64> {
    let value = raw.get(name).map_or(Some(default), |value| number(value));
    ensure!(
        value.is_some_and(|value| value >= 0.5 && value <= high),
        "--{name} must be 0.5–{}",
        crate::capacity::format::general(high)
    );
    Ok(value.unwrap())
}
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

pub(super) fn parse(arguments: &[OsString], cwd: &Path) -> Result<Options> {
    let keys = [
        "server-image",
        "generator-image",
        "netem-image",
        "output",
        "clients",
        "duration",
        "warmup",
        "ramp-up",
        "repetitions",
        "workers",
        "cpus",
        "memory",
        "generator-cpus",
        "port",
        "udp-port",
        "scenarios",
        "subscription-plan",
        "subscription-seed",
        "netem",
        "netem-scope",
        "max-connections",
        "label",
        "source-revision",
        "source-addresses",
        "server-env",
        "capture-handshakes",
        "capture-image",
    ];
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for pair in arguments.chunks(2) {
        let flag = pair[0].to_string_lossy();
        let key = flag.strip_prefix("--");
        ensure!(
            key.is_some_and(|key| keys.contains(&key))
                && pair.len() == 2
                && !key.is_some_and(|key| raw.get(key).is_some_and(|value| !value.is_empty())),
            "Unknown, duplicate or incomplete option: {flag}"
        );
        raw.insert(
            key.unwrap().into(),
            pair[1]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("option needs UTF8 text"))?
                .into(),
        );
    }
    let required = |name| {
        raw.get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Required: --{name}"))
    };
    let server_image = required("server-image")?;
    let generator_image = required("generator-image")?;
    let output = absolute(cwd, &required("output")?);
    let get = |name, default: &str| raw.get(name).cloned().unwrap_or_else(|| default.into());
    let netem_image = get("netem-image", "localhost/simplestchat-netem:dev");
    let capture_handshakes = raw.get("capture-handshakes").map_or(Ok(false), |value| {
        ensure!(
            ["true", "false"].contains(&value.as_str()),
            "--capture-handshakes must be true or false"
        );
        Ok::<_, anyhow::Error>(value == "true")
    })?;
    let capture_image = get("capture-image", "localhost/simplestchat-tcpdump:dev");
    let subscription_plan = match get("subscription-plan", "fifo").as_str() {
        "fifo" => Plan::Fifo,
        "ring-v1" => Plan::Ring,
        "hotspot-v1" => Plan::Hotspot,
        _ => bail!("--subscription-plan must be fifo, ring-v1 or hotspot-v1"),
    };
    let subscription_seed = if subscription_plan != Plan::Fifo {
        let seed = raw.get("subscription-seed");
        let seed = seed
            .filter(|value| {
                value.as_str() == "0"
                    || (!value.starts_with('0')
                        && !value.is_empty()
                        && value.bytes().all(|byte| byte.is_ascii_digit()))
            })
            .and_then(|value| value.parse().ok());
        ensure!(
            seed.is_some(),
            concat!(
                "--subscription-seed is required for ring-v1 and ",
                "hotspot-v1 and must be an unsigned 32-bit decimal integer"
            )
        );
        seed
    } else {
        ensure!(
            !raw.contains_key("subscription-seed"),
            "--subscription-seed requires --subscription-plan ring-v1 or hotspot-v1"
        );
        None
    };
    let duration = integer(&raw, "duration", 60, 3, 600)?;
    let warmup = integer(&raw, "warmup", 10, 2, 60)?;
    let ramp_up = integer(&raw, "ramp-up", 5, 1, 1200)?;
    let repetitions = integer(&raw, "repetitions", 1, 1, 5)?;
    let workers = integer(&raw, "workers", 2, 1, 4)?;
    let port = integer(&raw, "port", 3129, 1024, 65535)? as u16;
    let udp_port = integer(&raw, "udp-port", 41100, 1024, 65531)? as u16;
    let max_connections = integer(&raw, "max-connections", 1000, 2, 10000)?;
    let source_addresses = integer(&raw, "source-addresses", 1, 1, 60000)?;
    let cpus = quota(&raw, "cpus", 2.0, 16.0)?;
    let generator_cpus = quota(&raw, "generator-cpus", 5.0, 32.0)?;
    let memory = get("memory", "2g");
    let valid_memory = memory
        .as_bytes()
        .split_last()
        .is_some_and(|(unit, digits)| {
            b"mg".contains(unit)
                && !digits.is_empty()
                && digits[0] != b'0'
                && digits.iter().all(u8::is_ascii_digit)
        });
    ensure!(valid_memory, "--memory must be like 512m or 2g");
    let clients = get("clients", "100")
        .split(',')
        .map(number)
        .collect::<Vec<_>>();
    ensure!(
        !clients.is_empty()
            && clients.iter().all(|value| value
                .is_some_and(|value| value.fract() == 0.0 && (2.0..=2000.0).contains(&value))),
        "--clients must be unique counts between 2 and 2000"
    );
    let clients = clients
        .into_iter()
        .map(|value| value.unwrap() as u32)
        .collect::<Vec<_>>();
    ensure!(
        clients.iter().collect::<BTreeSet<_>>().len() == clients.len(),
        "--clients must be unique counts between 2 and 2000"
    );
    let names = get("scenarios", "multi-room");
    let names = names.split(',').collect::<Vec<_>>();
    ensure!(
        names
            .iter()
            .all(|name| ["conference", "multi-room", "audio", "webinar"].contains(name))
            && names.iter().collect::<BTreeSet<_>>().len() == names.len(),
        "Unknown/duplicate scenario"
    );
    let scenarios = clients
        .iter()
        .flat_map(|clients| {
            names.iter().map(move |name| {
                let addresses = if *name == "webinar" {
                    250
                } else {
                    source_addresses
                };
                let mut extra = if *name == "audio" {
                    vec!["--audio-only".into()]
                } else if *name == "webinar" {
                    vec![
                        "--publish-ratio".into(),
                        ryu_js::Buffer::new().format(1.0 / *clients as f64).into(),
                    ]
                } else {
                    Vec::new()
                };
                if addresses > 1 {
                    extra.extend(["--source-addresses".into(), addresses.to_string()]);
                }
                Workload {
                    name: format!("{name}-{clients}"),
                    clients: *clients,
                    rooms: if *name == "multi-room" {
                        4.min(clients / 2)
                    } else {
                        1
                    },
                    mode: if *name == "webinar" {
                        "webinar".into()
                    } else {
                        "conference".into()
                    },
                    source_addresses: addresses,
                    extra,
                }
            })
        })
        .collect::<Vec<_>>();
    for scenario in &scenarios {
        let stride = scenario.source_addresses * scenario.rooms
            / gcd(scenario.source_addresses, scenario.rooms);
        let per_address = scenario.clients.div_ceil(scenario.source_addresses);
        let per_pair = scenario.clients.div_ceil(stride);
        let spacing = (if per_address > 30 {
            60.5 / (30 * scenario.source_addresses) as f64
        } else {
            0.0
        })
        .max(if per_pair > 10 {
            60.5 / (10 * stride) as f64
        } else {
            0.0
        });
        let minimum = (spacing * scenario.clients as f64).ceil() as u32;
        ensure!(
            ramp_up >= minimum,
            "{} exceeds loopback join admission limits; use --ramp-up {minimum}",
            scenario.name
        );
    }
    let netem = raw.get("netem").cloned();
    let netem_scope = get("netem-scope", "udp-both");
    let mut server_env = BTreeMap::new();
    for entry in get("server-env", "")
        .split(';')
        .filter(|value| !value.is_empty())
    {
        let pair = entry.split_once('=');
        ensure!(
            pair.is_some_and(|(key, _)| !key.is_empty()
                && key.as_bytes()[0].is_ascii_uppercase()
                && key.bytes().all(|byte| byte.is_ascii_uppercase()
                    || byte.is_ascii_digit()
                    || byte == b'_')),
            "--server-env takes KEY=VALUE pairs separated by semicolons"
        );
        let (key, value) = pair.unwrap();
        server_env.insert(key.into(), value.into());
    }
    if let Some(value) = &netem.filter(|value| !value.is_empty()) {
        super::netem::script(value, &netem_scope, udp_port, workers)?;
    } else {
        ensure!(
            !raw.get("netem-scope")
                .is_some_and(|value| !value.is_empty()),
            "--netem-scope requires --netem"
        );
    }
    Ok(Options {
        server_image,
        generator_image,
        output,
        netem_image,
        capture_handshakes,
        capture_image,
        subscription_plan,
        subscription_seed,
        duration,
        warmup,
        ramp_up,
        repetitions,
        workers,
        port,
        udp_port,
        max_connections,
        source_addresses,
        cpus,
        generator_cpus,
        memory,
        clients,
        scenarios,
        netem: raw.get("netem").cloned(),
        netem_scope,
        server_env,
        label: raw.get("label").cloned(),
        source_revision: raw.get("source-revision").cloned(),
    })
}
