//! Bounded local comparison policy, parsed before creating any owned resources.

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    Performance,
    Diagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Detail {
    Full,
    CaptureOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Departure {
    Abrupt,
    ExplicitLeave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    #[serde(rename = "fifo")]
    Fifo,
    #[serde(rename = "ring-v1")]
    Ring,
    #[serde(rename = "hotspot-v1")]
    Hotspot,
}

impl Plan {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Ring => "ring-v1",
            Self::Hotspot => "hotspot-v1",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub name: String,
    pub clients: u32,
    pub rooms: u32,
    pub mode: String,
    pub extra: Vec<String>,
}

impl Scenario {
    pub fn audio_only(&self) -> bool {
        self.extra.iter().any(|value| value == "--audio-only")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub baseline_root: PathBuf,
    pub baseline_bin: PathBuf,
    pub candidate_root: PathBuf,
    pub candidate_bin: PathBuf,
    pub generator: PathBuf,
    pub generator_source_root: PathBuf,
    pub output: PathBuf,
    pub subscription_plan: Plan,
    pub subscription_seed: Option<u32>,
    pub purpose: Purpose,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_interface: Option<String>,
    pub diagnostic_detail: Detail,
    pub departure: Departure,
    pub duration: u32,
    pub warmup: u32,
    pub ramp_up: u32,
    pub repetitions: u32,
    pub workers: u32,
    pub port: u16,
    pub udp_port: u16,
    pub clients: Vec<u32>,
    pub scenarios: Vec<Scenario>,
}

impl Options {
    pub fn full_diagnostics(&self) -> bool {
        self.purpose == Purpose::Diagnostic && self.diagnostic_detail == Detail::Full
    }
}

/// Equivalent to JavaScript's finite Number conversion for CLI numeric options,
/// including explicit radix prefixes and the ECMAScript whitespace vocabulary.
pub(super) fn number(text: &str) -> Option<f64> {
    let text = text.trim_matches(|ch: char| {
        matches!(ch, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
            | '\u{205f}' | '\u{3000}' | '\u{feff}')
    });
    if text.is_empty() {
        return Some(0.0);
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0b", "0B"], 2), (["0o", "0O"], 8)] {
        if let Some(digits) = prefixes.iter().find_map(|prefix| text.strip_prefix(prefix)) {
            if digits.is_empty() {
                return None;
            }
            let mut value = 0.0;
            for digit in digits.chars() {
                value = value * radix as f64 + digit.to_digit(radix)? as f64;
            }
            return value.is_finite().then_some(value);
        }
    }
    if text
        .bytes()
        .any(|byte| !matches!(byte, b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn integer(text: &str, minimum: u32, maximum: u32) -> Option<u32> {
    let value = number(text)?;
    (value.fract() == 0.0 && value >= minimum as f64 && value <= maximum as f64)
        .then_some(value as u32)
}

fn absolute(cwd: &Path, text: &str) -> PathBuf {
    let source = cwd.join(text);
    let mut output = PathBuf::new();
    for component in source.components() {
        match component {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            other => output.push(other.as_os_str()),
        }
    }
    output
}

/// Parse and validate all admission, duration and diagnostic settings before
/// touching output paths, ports or executables. Relative paths use caller cwd.
pub fn parse(args: &[OsString], cwd: &Path) -> Result<Options> {
    let mut raw = BTreeMap::new();
    let accepted = [
        "baseline-root",
        "baseline-bin",
        "candidate-root",
        "candidate-bin",
        "generator",
        "generator-source-root",
        "output",
        "clients",
        "duration",
        "warmup",
        "ramp-up",
        "repetitions",
        "workers",
        "port",
        "udp-port",
        "scenarios",
        "purpose",
        "capture-interface",
        "diagnostic-detail",
        "departure",
        "subscription-plan",
        "subscription-seed",
    ];
    for pair in args.chunks(2) {
        let key = pair[0].to_str().and_then(|value| value.strip_prefix("--"));
        let value = pair.get(1).and_then(|value| value.to_str());
        let Some((key, value)) = key.zip(value) else {
            bail!("Unknown, duplicate or incomplete option");
        };
        ensure!(
            accepted.contains(&key)
                && !value.is_empty()
                && !value.starts_with("--")
                && raw.insert(key, value).is_none(),
            "Unknown, duplicate or incomplete option"
        );
    }
    let required = |key| {
        raw.get(key)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Required: --{key}"))
    };
    let path = |key| required(key).map(|value| absolute(cwd, value));
    let candidate_root = path("candidate-root")?;
    let subscription_plan = match raw.get("subscription-plan").copied().unwrap_or("fifo") {
        "fifo" => Plan::Fifo,
        "ring-v1" => Plan::Ring,
        "hotspot-v1" => Plan::Hotspot,
        _ => bail!("--subscription-plan must be fifo, ring-v1 or hotspot-v1"),
    };
    let subscription_seed = if subscription_plan == Plan::Fifo {
        ensure!(
            !raw.contains_key("subscription-seed"),
            "--subscription-seed requires ring-v1 or hotspot-v1"
        );
        None
    } else {
        let seed = required("subscription-seed")?;
        ensure!(
            seed == "0"
                || seed
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                    && seed.bytes().all(|byte| byte.is_ascii_digit()),
            "Invalid subscription seed"
        );
        Some(
            seed.parse::<u32>()
                .map_err(|_| anyhow::anyhow!("Invalid subscription seed"))?,
        )
    };
    let purpose = match raw.get("purpose").copied().unwrap_or("performance") {
        "performance" => Purpose::Performance,
        "diagnostic" => Purpose::Diagnostic,
        _ => bail!("--purpose must be performance or diagnostic"),
    };
    let capture_interface = raw
        .get("capture-interface")
        .map(|value| (*value).to_owned());
    ensure!(
        capture_interface
            .as_deref()
            .is_none_or(|value| purpose == Purpose::Diagnostic && ["lo", "lo0"].contains(&value)),
        "--capture-interface requires diagnostic mode and lo/lo0"
    );
    let diagnostic_detail = match raw.get("diagnostic-detail").copied().unwrap_or("full") {
        "full" => Detail::Full,
        "capture-only" => Detail::CaptureOnly,
        _ => bail!("Invalid --diagnostic-detail"),
    };
    ensure!(
        (!raw.contains_key("diagnostic-detail") || purpose == Purpose::Diagnostic)
            && (diagnostic_detail != Detail::CaptureOnly || capture_interface.is_some()),
        "--diagnostic-detail requires diagnostic mode; capture-only requires capture"
    );
    let departure = match raw.get("departure").copied().unwrap_or("abrupt") {
        "abrupt" => Departure::Abrupt,
        "explicit-leave" => Departure::ExplicitLeave,
        _ => bail!("Invalid --departure"),
    };
    ensure!(
        !raw.contains_key("departure")
            || purpose == Purpose::Diagnostic && diagnostic_detail == Detail::Full,
        "--departure requires full diagnostic mode"
    );
    let bounded = |key, fallback: u32, min, max| {
        integer(
            raw.get(key).copied().unwrap_or(&fallback.to_string()),
            min,
            max,
        )
        .ok_or_else(|| anyhow::anyhow!("--{key} must be {min}–{max}"))
    };
    let (duration, warmup, ramp_up) = (
        bounded("duration", 60, 3, 180)?,
        bounded("warmup", 10, 2, 60)?,
        bounded("ramp-up", 5, 1, 600)?,
    );
    let clients: Vec<_> = raw
        .get("clients")
        .copied()
        .unwrap_or("10")
        .split(',')
        .map(|value| integer(value, 2, 100).ok_or_else(|| anyhow::anyhow!("Invalid --clients")))
        .collect::<Result<_>>()?;
    ensure!(
        clients.iter().collect::<BTreeSet<_>>().len() == clients.len(),
        "Duplicate --clients"
    );
    let kinds: Vec<_> = raw
        .get("scenarios")
        .copied()
        .unwrap_or("conference")
        .split(',')
        .collect();
    ensure!(
        kinds
            .iter()
            .all(|kind| ["conference", "multi-room", "webinar", "audio", "churn"].contains(kind))
            && kinds.iter().collect::<BTreeSet<_>>().len() == kinds.len(),
        "Unknown/duplicate scenario"
    );
    ensure!(
        subscription_plan == Plan::Fifo
            || kinds
                .iter()
                .all(|kind| ["conference", "multi-room", "audio"].contains(kind)),
        "Planned subscriptions require stable all-publisher scenarios"
    );
    let mut scenarios = Vec::new();
    for count in &clients {
        for kind in &kinds {
            let rooms = if *kind == "multi-room" {
                4.min(count / 2)
            } else {
                1
            };
            let extra = match *kind {
                "audio" => vec!["--audio-only".to_owned()],
                "churn" => vec![
                    "--churn-rate".to_owned(),
                    ryu_js::Buffer::new()
                        .format(count.div_ceil(5) as f64 / duration as f64)
                        .to_owned(),
                ],
                _ => Vec::new(),
            };
            let spacing = (if *count > 30 { 60.5_f64 / 30.0 } else { 0.0 }).max(
                if count.div_ceil(rooms) > 10 {
                    60.5 / (10 * rooms) as f64
                } else {
                    0.0
                },
            );
            let minimum = (spacing * *count as f64).ceil() as u32;
            ensure!(
                ramp_up >= minimum,
                "{kind}-{count} exceeds join admission limits; use --ramp-up {minimum}"
            );
            ensure!(
                *kind != "churn" || *count < 10,
                "Use fewer than10 local clients for churn"
            );
            scenarios.push(Scenario {
                name: format!("{kind}-{count}"),
                clients: *count,
                rooms,
                mode: if *kind == "webinar" {
                    "webinar"
                } else {
                    "conference"
                }
                .to_owned(),
                extra,
            });
        }
    }
    Ok(Options {
        baseline_root: path("baseline-root")?,
        baseline_bin: path("baseline-bin")?,
        candidate_bin: path("candidate-bin")?,
        generator: path("generator")?,
        output: path("output")?,
        generator_source_root: raw
            .get("generator-source-root")
            .map(|value| absolute(cwd, value))
            .unwrap_or_else(|| candidate_root.clone()),
        candidate_root,
        subscription_plan,
        subscription_seed,
        purpose,
        capture_interface,
        diagnostic_detail,
        departure,
        duration,
        warmup,
        ramp_up,
        repetitions: bounded("repetitions", 3, 1, 5)?,
        workers: bounded("workers", 1, 1, 4)?,
        port: bounded("port", 3129, 1024, 65535)? as u16,
        udp_port: bounded("udp-port", 41100, 1024, 65531)? as u16,
        clients,
        scenarios,
    })
}
