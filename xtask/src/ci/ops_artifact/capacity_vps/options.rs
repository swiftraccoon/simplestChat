//! Existing workload flags plus three explicit authenticated-artifact selectors.

use super::{Result, failure};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub(super) const USAGE: &str = "usage: xtask vps-capacity --inventory PATH --limit HOST \
--revision SHA --ops-ci-run ID --ops-run-attempt ID --ops-artifact ID \
[--prepare] [--build-images] [--collect RUN | --recover RUN]

Workload options:
  --label TEXT                  private-vps
  --workload WORKLOAD            meetings (meetings, large-meeting, webinar)
  --first-size N                30
  --steps N                     1
  --meeting-size N              30
  --speakers N                  30
  --audio-only
  --chat-interval-ms N           0
  --quick
  --runtime-seconds N            7200
  --monthly-price NUMBER
  --server-cpus NUMBER
  --generator-cpus NUMBER
  --app-cpus NUMBER
  --port-mbps NUMBER             1000

Collect and recover adopt the saved original workload; they never prepare,
build or start a new workload. Artifact IDs are explicit positive decimal IDs.";
const SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    New,
    Collect,
    Recover,
}

#[derive(Clone, Debug)]
pub(super) struct Options {
    pub inventory: PathBuf,
    pub limit: String,
    pub revision: String,
    pub ci_run: u64,
    pub attempt: u64,
    pub artifact: u64,
    pub prepare: bool,
    pub build: bool,
    pub mode: Mode,
    pub run: Option<String>,
    workload: Workload,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Workload {
    revision: String,
    run: String,
    label: String,
    workload: String,
    first_size: u64,
    steps: u64,
    meeting_size: u64,
    speakers: u64,
    audio_only: bool,
    chat_interval_ms: u64,
    quick: bool,
    runtime_seconds: u64,
    monthly_price: Option<f64>,
    server_cpus: Option<f64>,
    generator_cpus: Option<f64>,
    app_cpus: Option<f64>,
    port_mbps: f64,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn decimal(value: &str) -> Result<u64> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(failure(2));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= SAFE)
        .ok_or_else(|| failure(2))
}
fn integer(value: &str) -> Result<u64> {
    // argparse's decimal spellings allow signs and digit separators. Negative
    // zero remains zero; negative nonzero values fail the unsigned workload.
    let value = value.trim();
    let negative = value.starts_with('-');
    let value = value.strip_prefix(['+', '-']).unwrap_or(value);
    if value.is_empty()
        || value.starts_with('_')
        || value.ends_with('_')
        || value.contains("__")
        || !value.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return Err(failure(2));
    }
    let parsed = value.replace('_', "").parse().map_err(|_| failure(2))?;
    if negative && parsed != 0 {
        Err(failure(2))
    } else {
        Ok(parsed)
    }
}
fn float(value: &str) -> Result<f64> {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'_'
            && (index == 0
                || index + 1 == bytes.len()
                || !bytes[index - 1].is_ascii_digit()
                || !bytes[index + 1].is_ascii_digit())
    }) {
        return Err(failure(2));
    }
    let number = value
        .replace('_', "")
        .parse::<f64>()
        .map_err(|_| failure(2))?;
    if number.is_finite() {
        Ok(number)
    } else {
        Err(failure(2))
    }
}
pub(super) fn path(value: &Path) -> bool {
    super::super::namespace::path(value)
}

impl Options {
    pub fn decode(arguments: &[String], caller: &Path, home: &Path) -> Result<Self> {
        let mut normalized = Vec::new();
        for argument in arguments {
            if argument.starts_with("--")
                && let Some((flag, value)) = argument.split_once('=')
            {
                if matches!(
                    flag,
                    "--prepare" | "--build-images" | "--audio-only" | "--quick"
                ) {
                    return Err(failure(2));
                }
                normalized.push(flag.to_owned());
                normalized.push(value.to_owned());
            } else {
                normalized.push(argument.clone());
            }
        }
        let mut result = Self {
            inventory: PathBuf::new(),
            limit: String::new(),
            revision: String::new(),
            ci_run: 0,
            attempt: 0,
            artifact: 0,
            prepare: false,
            build: false,
            mode: Mode::New,
            run: None,
            workload: Workload {
                revision: String::new(),
                run: "0".repeat(32),
                label: "private-vps".into(),
                workload: "meetings".into(),
                first_size: 30,
                steps: 1,
                meeting_size: 30,
                speakers: 30,
                audio_only: false,
                chat_interval_ms: 0,
                quick: false,
                runtime_seconds: 7200,
                monthly_price: None,
                server_cpus: None,
                generator_cpus: None,
                app_cpus: None,
                port_mbps: 1000.0,
            },
        };
        let mut values = normalized.iter();
        while let Some(flag) = values.next() {
            match flag.as_str() {
                "--prepare" => result.prepare = true,
                "--build-images" => result.build = true,
                "--audio-only" => result.workload.audio_only = true,
                "--quick" => result.workload.quick = true,
                _ => {
                    let value = values.next().ok_or_else(|| failure(2))?;
                    if value.len() > 4096 || value.contains('\0') {
                        return Err(failure(2));
                    }
                    match flag.as_str() {
                        "--inventory" => {
                            result.inventory = if let Some(suffix) = value.strip_prefix("~/") {
                                home.join(suffix)
                            } else {
                                caller.join(value)
                            };
                        }
                        "--limit" => result.limit = value.clone(),
                        "--revision" => result.revision = value.clone(),
                        "--ops-ci-run" => result.ci_run = decimal(value)?,
                        "--ops-run-attempt" => result.attempt = decimal(value)?,
                        "--ops-artifact" => result.artifact = decimal(value)?,
                        "--collect" | "--recover" => {
                            if result.mode != Mode::New
                                && ((flag == "--collect") != (result.mode == Mode::Collect))
                            {
                                return Err(failure(2));
                            }
                            result.mode = if flag == "--collect" {
                                Mode::Collect
                            } else {
                                Mode::Recover
                            };
                            result.run = Some(value.clone());
                        }
                        "--label" => result.workload.label = value.clone(),
                        "--workload" => result.workload.workload = value.clone(),
                        "--first-size" => result.workload.first_size = integer(value)?,
                        "--steps" => result.workload.steps = integer(value)?,
                        "--meeting-size" => result.workload.meeting_size = integer(value)?,
                        "--speakers" => result.workload.speakers = integer(value)?,
                        "--chat-interval-ms" => result.workload.chat_interval_ms = integer(value)?,
                        "--runtime-seconds" => result.workload.runtime_seconds = integer(value)?,
                        "--monthly-price" => result.workload.monthly_price = Some(float(value)?),
                        "--server-cpus" => result.workload.server_cpus = Some(float(value)?),
                        "--generator-cpus" => result.workload.generator_cpus = Some(float(value)?),
                        "--app-cpus" => result.workload.app_cpus = Some(float(value)?),
                        "--port-mbps" => result.workload.port_mbps = float(value)?,
                        _ => return Err(failure(2)),
                    }
                }
            }
        }
        if !path(&result.inventory)
            || !hex(&result.revision, 40)
            || result.ci_run == 0
            || result.attempt == 0
            || result.artifact == 0
            || result.limit.is_empty()
            || result.limit.len() > 63
            || !result.limit.as_bytes()[0].is_ascii_alphabetic()
            || !result
                .limit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || result.run.as_ref().is_some_and(|run| !hex(run, 32))
            || (result.mode != Mode::New && (result.prepare || result.build))
        {
            return Err(failure(2));
        }
        result.workload.revision = result.revision.clone();
        let run = result.run.clone().unwrap_or_else(|| "0".repeat(32));
        result.request(&run)?;
        Ok(result)
    }
    pub fn runtime(&self) -> u64 {
        if self.mode == Mode::New {
            self.workload.runtime_seconds
        } else {
            21600
        }
    }
    pub fn request(&self, run: &str) -> Result<Vec<u8>> {
        let mut workload = self.workload.clone();
        workload.run = run.into();
        let bytes = serde_json::to_vec(&workload).map_err(|_| failure(2))?;
        simplestchat_ops::capacity_vps::protocol::Workload::decode(&bytes)
            .map_err(|_| failure(2))?;
        Ok(bytes)
    }
}
