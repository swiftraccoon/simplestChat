//! Read-only host facts over the enrolled key, gathered by two fixed POSIX
//! shell programs. The host prints plain `key=value` records; every value is
//! validated here, and nothing the host sends is interpreted as a program.

use super::ssh::{Authentication, Target, command, quote};
use super::{Refusal, Result, Runner, require};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// What every record line starts with; banners and other output are ignored.
const RECORD: &str = "SIMPLESTCHAT_BOOTSTRAP_FACT ";
const GIB: u64 = 1024 * 1024 * 1024;
/// The largest ownership journal read back from the host.
const MAX_JOURNAL_BYTES: usize = 16384;

/// Platform, capacity and privilege facts. It changes nothing on the host.
pub(super) const FACTS: &str = r#"set -eu
fact() { printf 'SIMPLESTCHAT_BOOTSTRAP_FACT %s=%s\n' "$1" "$2"; }
field() { sed -n "s/^$1=//p" /etc/os-release | head -n 1 | tr -d '"'; }
fact distribution "$(field ID)"
fact version "$(field VERSION_ID)"
fact architecture "$(uname -m)"
fact kernel "$(uname -r)"
fact cpus "$(getconf _NPROCESSORS_ONLN)"
if [ -d /srv ]; then volume=/srv; else volume=/; fi
fact freeBytes "$(df -P -B1 "$volume" | awk 'NR == 2 { print $4 }')"
# A file system without an inode count reports none free.
fact freeInodes "$(df -P -i "$volume" | awk 'NR == 2 { print ($4 ~ /^[0-9]+$/ ? $4 : 0) }')"
if [ -d /run/systemd/system ]; then fact systemd true; else fact systemd false; fi
if [ "$(id -u)" = 0 ] || sudo -n true >/dev/null 2>&1; then
  fact passwordlessSudo true
else
  fact passwordlessSudo false
fi
if command -v docker >/dev/null 2>&1; then
  fact dockerInstalled true
else
  fact dockerInstalled false
fi
fact python "$(python3 --version 2>/dev/null | sed -n 's/^Python //p')"
"#;

/// Whether a benchmark, an image build or an unfinished run owns the host.
/// A lock, journal or directory with unexpected ownership stops the program.
pub(super) const BUSY: &str = r#"set -eu
fact() { printf 'SIMPLESTCHAT_BOOTSTRAP_FACT %s=%s\n' "$1" "$2"; }
shape() { stat -c '%F %u %a' "$1"; }
root=/run/simplestchat-bench
held=false
journal=
if [ -e "$root" ] || [ -L "$root" ]; then
  [ "$(shape "$root")" = 'directory 0 700' ]
  lock="$root/workload.lock"
  if [ -e "$lock" ] || [ -L "$lock" ]; then
    case "$(shape "$lock")" in
      'regular file 0 600' | 'regular empty file 0 600') ;;
      *) exit 1 ;;
    esac
    if ! flock --nonblock --exclusive "$lock" true; then held=true; fi
  fi
  record="$root/current.json"
  if [ -e "$record" ] || [ -L "$record" ]; then
    [ "$(shape "$record")" = 'regular file 0 600' ]
    [ "$(stat -c %s "$record")" -le 16384 ]
    journal=$(base64 -w0 "$record")
  fi
fi
active=false
for unit in simplestchat-image-build.service simplestchat-benchmark.service; do
  state=$(systemctl is-active "$unit") && status=0 || status=$?
  case "$status" in 0 | 3 | 4) ;; *) exit 1 ;; esac
  case "$state" in active | activating | deactivating | reloading) active=true ;; esac
done
fact held "$held"
fact active "$active"
fact journal "$journal"
"#;

/// Exactly the `expected` records in `text`, each once; other lines, such as
/// a login banner, are ignored.
fn parse(text: &str, expected: &[&str]) -> Result<BTreeMap<String, String>> {
    let malformed = Refusal("invalid_preflight_envelope");
    let mut found = BTreeMap::new();
    for record in text.lines().filter_map(|line| line.strip_prefix(RECORD)) {
        let (name, value) = record.split_once('=').ok_or(malformed)?;
        let repeated = found.insert(name.to_owned(), value.to_owned()).is_some();
        require(!repeated, "invalid_preflight_envelope")?;
    }
    let named = expected.iter().all(|name| found.contains_key(*name));
    require(
        named && found.len() == expected.len(),
        "invalid_preflight_envelope",
    )?;
    Ok(found)
}

/// Runs one fixed program on the host and returns its records.
fn records(
    runner: &dyn Runner,
    target: &Target,
    program: &str,
    sudo: bool,
    expected: &[&str],
) -> Result<BTreeMap<String, String>> {
    let elevate = if sudo && target.user != "root" {
        "sudo -n "
    } else {
        ""
    };
    let remote = format!("{elevate}sh -c {}", quote(program));
    let remote = command(target, &remote, Authentication::Key, false);
    let finished = runner.run(&remote, Duration::from_secs(45))?;
    require(finished.status == 0, "remote_preflight_failed")?;
    parse(&finished.text, expected)
}

fn flag(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Refusal("invalid_preflight_envelope")),
    }
}

fn count(value: &str) -> Result<u64> {
    let digits = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    require(digits, "invalid_preflight_envelope")?;
    value
        .parse()
        .map_err(|_| Refusal("invalid_preflight_envelope"))
}

fn token(value: &str) -> Result<&str> {
    let shape = Regex::new(r"^[A-Za-z0-9._+~:-]{0,128}$").unwrap();
    require(shape.is_match(value), "invalid_preflight_envelope")?;
    Ok(value)
}

/// The records the facts program prints.
const FACT_NAMES: [&str; 11] = [
    "distribution",
    "version",
    "architecture",
    "kernel",
    "cpus",
    "freeBytes",
    "freeInodes",
    "systemd",
    "passwordlessSudo",
    "dockerInstalled",
    "python",
];

/// The host's facts as the evidence record keeps them.
fn facts(runner: &dyn Runner, target: &Target) -> Result<Value> {
    let found = records(runner, target, FACTS, false, &FACT_NAMES)?;
    let python = token(&found["python"])?;
    Ok(json!({
        "distribution": token(&found["distribution"])?,
        "version": token(&found["version"])?,
        "architecture": token(&found["architecture"])?,
        "cpus": count(&found["cpus"])?,
        "freeBytes": count(&found["freeBytes"])?,
        "freeInodes": count(&found["freeInodes"])?,
        "kernel": token(&found["kernel"])?,
        "systemd": flag(&found["systemd"])?,
        "passwordlessSudo": flag(&found["passwordlessSudo"])?,
        "python": if python.is_empty() { Value::Null } else { json!(python) },
        "dockerInstalled": flag(&found["dockerInstalled"])?,
    }))
}

/// Whether the host is running, or has not finished, a benchmark workload.
fn busy(runner: &dyn Runner, target: &Target) -> Result<bool> {
    let found = records(runner, target, BUSY, true, &["held", "active", "journal"])?;
    let mut busy = flag(&found["held"])? || flag(&found["active"])?;
    if !found["journal"].is_empty() {
        let journal = STANDARD.decode(&found["journal"]);
        let journal = journal.map_err(|_| Refusal("invalid_preflight_envelope"))?;
        require(
            journal.len() <= MAX_JOURNAL_BYTES,
            "invalid_preflight_envelope",
        )?;
        let journal: Value =
            serde_json::from_slice(&journal).map_err(|_| Refusal("invalid_preflight_envelope"))?;
        let finalized = journal["schemaVersion"] == 1 && journal["finalized"] == true;
        busy = busy || !finalized;
    }
    Ok(busy)
}

/// Requires the supported platform, enough free space and an idle host
/// before any provisioning, and returns the facts for the evidence.
pub(super) fn preflight(
    runner: &dyn Runner,
    target: &Target,
    minimum_free_gib: u64,
) -> Result<Value> {
    let facts = facts(runner, target)?;
    let debian = facts["distribution"] == "debian" && facts["version"] == "13";
    require(debian, "unsupported_distribution")?;
    let platform = facts["architecture"] == "x86_64" && facts["systemd"] == true;
    require(platform, "unsupported_host_platform")?;
    let free = facts["freeBytes"].as_u64().unwrap_or(0);
    require(free >= minimum_free_gib * GIB, "insufficient_disk_space")?;
    if facts["passwordlessSudo"] == true {
        require(!busy(runner, target)?, "host_workload_active_or_unfinished")?;
    }
    Ok(facts)
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
