//! Informational host observations for a local comparison, never workload gates.
//!
//! The field names and OS/architecture labels follow the JavaScript manifest.
//! Unavailable observations are explicit nulls. Linux load averages come from
//! `/proc/loadavg`, which rounds the same 1/5/15 minute averages to two decimals;
//! Darwin uses `sysctl vm.loadavg`. Neither reports a container resource quota.

use super::process::{Command, Exit, Host, Sink};
use anyhow::Result;
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const FAILURE: &str = "Benchmark environment observation failed";
const COMMAND_LIMIT: u64 = 4096;
const CPU_LIMIT: u64 = 2 * 1024 * 1024;
const PROC_LIMIT: u64 = 64 * 1024;
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;

#[derive(Debug)]
struct Interrupted(u8);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(FAILURE)
    }
}
impl std::error::Error for Interrupted {}

/// An observed pending signal remains typed without exposing command output.
pub fn failure_status(error: &anyhow::Error) -> Option<u8> {
    error.downcast_ref::<Interrupted>().map(|value| value.0)
}

async fn check(host: &dyn Host) -> Result<()> {
    tokio::select! {
        biased;
        code = host.cancelled() => Err(Interrupted(code).into()),
        () = std::future::ready(()) => Ok(()),
    }
}

enum Observation {
    Exit(Exit),
    Unavailable,
    Cancelled(u8),
}

/// Every started observation is retired before its output is inspected. A
/// missing tool or unsuccessful observation is unavailable; cancellation and a
/// failed owned cleanup refuse the manifest instead of hiding an active child.
async fn command(
    host: &dyn Host,
    directory: &Path,
    environment: &BTreeMap<OsString, OsString>,
    program: &str,
    arguments: &[&str],
) -> Result<Option<String>> {
    check(host).await?;
    let mut environment = super::commands::clean(environment);
    environment.insert("LC_ALL".into(), "C".into());
    let Ok(mut child) = host.spawn(Command {
        executable: program.into(),
        arguments: arguments.iter().map(|value| (*value).into()).collect(),
        directory: directory.into(),
        environment,
        timeout: Duration::from_secs(2),
        stdout_limit: COMMAND_LIMIT,
        stderr_limit: COMMAND_LIMIT,
        input: Vec::new(),
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    }) else {
        check(host).await?;
        return Ok(None);
    };
    let observation = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observed = tokio::select! {
                biased;
                code = host.cancelled() => return Observation::Cancelled(code),
                observed = child.poll() => observed,
            };
            match observed {
                Ok(Some(exit)) => return Observation::Exit(exit),
                Err(_) => return Observation::Unavailable,
                Ok(None) => {
                    tokio::select! {
                        biased;
                        code = host.cancelled() => return Observation::Cancelled(code),
                        () = tokio::time::sleep(Duration::from_millis(10)) => {},
                    }
                }
            }
        }
    })
    .await
    .unwrap_or(Observation::Unavailable);
    let stopped = child.stop(Duration::ZERO).await;
    if let Observation::Cancelled(code) = observation {
        return Err(Interrupted(code).into());
    }
    let stopped = stopped.map_err(|_| anyhow::anyhow!(FAILURE))?;
    check(host).await?;
    if !matches!(observation, Observation::Exit(exit) if exit.success()) || !stopped.success() {
        return Ok(None);
    }
    if child.output().len() as u64 > COMMAND_LIMIT {
        return Ok(None);
    }
    Ok(String::from_utf8(child.output().to_vec()).ok())
}

fn text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_owned();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn positive_integer(value: &str) -> Option<u64> {
    let value = value.trim().parse::<u64>().ok()?;
    (value > 0 && value <= MAX_SAFE_INTEGER).then_some(value)
}

fn averages(value: &str) -> Option<[f64; 3]> {
    let mut fields = value.split_whitespace();
    let mut result = [0.0_f64; 3];
    for number in &mut result {
        *number = fields.next()?.parse().ok()?;
        if !number.is_finite() || *number < 0.0 {
            return None;
        }
    }
    Some(result)
}

/// The injected root is a real opened directory; only fixed proc basenames can
/// be read. Special files and links are unavailable, reads are bounded before
/// allocation growth, and even informational input close failures are reported.
fn proc_file(directory: &File, name: &str, maximum: u64) -> Result<Option<String>> {
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
    let Ok(descriptor) = openat(directory, name, flags, Mode::empty()) else {
        return Ok(None);
    };
    let mut file = File::from(descriptor);
    let value = (|| {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > maximum {
            return None;
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > maximum {
            return None;
        }
        String::from_utf8(bytes).ok()
    })();
    nix::unistd::close(file).map_err(|_| anyhow::anyhow!(FAILURE))?;
    Ok(value)
}

#[derive(Default)]
struct Details {
    cpu: Option<String>,
    logical_cpus: Option<u64>,
    memory_bytes: Option<u64>,
    load_average_before: Option<[f64; 3]>,
}

fn linux(root: &Path) -> Result<Details> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let Ok(descriptor) = open(root, flags, Mode::empty()) else {
        return Ok(Details::default());
    };
    let directory = File::from(descriptor);
    let observed = (|| -> Result<Details> {
        let cpuinfo = proc_file(&directory, "cpuinfo", CPU_LIMIT)?;
        let meminfo = proc_file(&directory, "meminfo", PROC_LIMIT)?;
        let loadavg = proc_file(&directory, "loadavg", PROC_LIMIT)?;
        let mut result = Details::default();
        if let Some(cpuinfo) = cpuinfo {
            let mut processors = BTreeSet::new();
            let mut valid = true;
            for line in cpuinfo.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                match key.trim() {
                    "model name" if result.cpu.is_none() => {
                        result.cpu = text(Some(value.into()));
                    }
                    "processor" => match value.trim().parse::<u32>() {
                        Ok(index) if processors.insert(index) => {}
                        _ => valid = false,
                    },
                    _ => {}
                }
            }
            if valid && !processors.is_empty() {
                result.logical_cpus = Some(processors.len() as u64);
            }
        }
        if let Some(meminfo) = meminfo {
            let mut entries = meminfo.lines().filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key == "MemTotal").then_some(value)
            });
            if let Some(value) = entries.next()
                && entries.next().is_none()
            {
                let mut parts = value.split_whitespace();
                if let Some(bytes) = parts
                    .next()
                    .and_then(positive_integer)
                    .and_then(|value| value.checked_mul(1024))
                    && bytes <= MAX_SAFE_INTEGER
                    && parts.next() == Some("kB")
                    && parts.next().is_none()
                {
                    result.memory_bytes = Some(bytes);
                }
            }
        }
        result.load_average_before = loadavg.as_deref().and_then(averages);
        Ok(result)
    })();
    let closed = nix::unistd::close(directory).map_err(|_| anyhow::anyhow!(FAILURE));
    let observed = observed?;
    closed?;
    Ok(observed)
}

async fn observe_for(
    host: &dyn Host,
    directory: &Path,
    environment: &BTreeMap<OsString, OsString>,
    platform: &str,
    architecture: &str,
    proc_root: &Path,
) -> Result<Value> {
    check(host).await?;
    let (release, details) = if platform == "macos" {
        let (release, cpu, count, memory, load) = tokio::join!(
            command(host, directory, environment, "uname", &["-r"]),
            command(
                host,
                directory,
                environment,
                "sysctl",
                &["-n", "machdep.cpu.brand_string"]
            ),
            command(host, directory, environment, "sysctl", &["-n", "hw.ncpu"]),
            command(
                host,
                directory,
                environment,
                "sysctl",
                &["-n", "hw.memsize"]
            ),
            command(
                host,
                directory,
                environment,
                "sysctl",
                &["-n", "vm.loadavg"]
            ),
        );
        let details = Details {
            cpu: text(cpu?),
            logical_cpus: count?.as_deref().and_then(positive_integer),
            memory_bytes: memory?.as_deref().and_then(positive_integer),
            load_average_before: load?.as_deref().and_then(|value| {
                let body = value.trim().strip_prefix('{')?.strip_suffix('}')?;
                (body.split_whitespace().count() == 3)
                    .then(|| averages(body))
                    .flatten()
            }),
        };
        (text(release?), details)
    } else {
        let release = text(command(host, directory, environment, "uname", &["-r"]).await?);
        let details = if platform == "linux" {
            linux(proc_root)?
        } else {
            Details::default()
        };
        (release, details)
    };
    check(host).await?;
    let mut unavailable = Vec::new();
    for (field, available) in [
        ("release", release.is_some()),
        ("cpu", details.cpu.is_some()),
        ("logicalCpus", details.logical_cpus.is_some()),
        ("memoryBytes", details.memory_bytes.is_some()),
        ("loadAverageBefore", details.load_average_before.is_some()),
    ] {
        if !available {
            unavailable.push(field);
        }
    }
    let architecture = match architecture {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        "powerpc" => "ppc",
        "loongarch64" => "loong64",
        value => value,
    };
    Ok(json!({
        "platform":if platform == "macos" { "darwin" } else { platform },
        "release":release,"arch":architecture,"cpu":details.cpu,
        "logicalCpus":details.logical_cpus,"memoryBytes":details.memory_bytes,
        "loadAverageBefore":details.load_average_before,"implementation":"rust",
        "loadAverageSource":match platform {
            "linux" => Some("/proc/loadavg (rounded to two decimals)"),
            "macos" => Some("sysctl vm.loadavg"),
            _ => None,
        },
        "unavailableObservations":unavailable,
    }))
}

/// Collect bounded read-only host evidence. A null is unavailable, never an
/// invented zero or a process-affinity count substituted for host CPU count.
/// Every started child is closed/reaped before returning; cleanup or signal
/// failures expose one fixed error without command output or private paths.
pub async fn observe(
    host: &dyn Host,
    directory: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<Value> {
    observe_for(
        host,
        directory,
        environment,
        std::env::consts::OS,
        std::env::consts::ARCH,
        Path::new("/proc"),
    )
    .await
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
