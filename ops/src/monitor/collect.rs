//! One collection: fixed health signals of the managed containers, the
//! database, the public proxy, the relay's certificate, backup evidence and
//! transfer, published as one atomic textfile. Every subsystem that cannot be
//! sampled has an explicit failure gauge; no user data or secret is read.

use super::alerts;
use super::backups::{self, Entry};
use super::text::{Sample, gauge};
use super::transfer;
use crate::host::command::Request;
use crate::host::failure::{Failure, Result, require};
use crate::host::state;
use crate::host::{DOCKER, Host, POSTGRES};
use crate::json::Json;
use chrono::NaiveDateTime;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const MAX_OUTPUT: usize = 65536;
const MAX_BACKUPS: usize = 10000;
const SERVICES: [(&str, &str); 4] = [
    ("app", "simplestchat-public-simplestchat-1"),
    ("postgres", POSTGRES),
    ("proxy", "simplestchat-public-caddy-1"),
    ("turn", "simplestchat-turn-turn-1"),
];
const INSPECT: &str =
    r#"{"running":{{.State.Running}},"restarts":{{.RestartCount}},"oom":{{.State.OOMKilled}}}"#;
const PG_QUERY: &str = "SET statement_timeout='2s';
SELECT json_build_object('connections',numbackends,'deadlocks',deadlocks,'rollbacks',xact_rollback,
 'commits',xact_commit,'connection_limit',current_setting('max_connections')::int,
 'blocked', (SELECT count(*) FROM pg_stat_activity WHERE cardinality(pg_blocking_pids(pid)) > 0))
FROM pg_stat_database WHERE datname=current_database();";

/// Run one fixed read-only command and return its trimmed text.
fn command(host: &Host<'_>, argv: Vec<String>, seconds: f64) -> Result<String> {
    let oversized = "Oversized monitoring command response";
    let output = host
        .commands
        .run(&Request::fixed(argv, seconds, MAX_OUTPUT, oversized))?;
    let text = String::from_utf8(output).map_err(|_| Failure::Value)?;
    Ok(text.trim().to_owned())
}

/// Address the local daemon with fixed arguments.
pub(super) fn docker(host: &Host<'_>, arguments: &[&str]) -> Result<String> {
    let argv = DOCKER
        .iter()
        .chain(arguments)
        .map(|argument| (*argument).to_owned());
    command(host, argv.collect(), 3.0)
}

/// A JSON number as a sample.
fn number(value: &Json) -> Result<f64> {
    match value {
        Json::Integer(_) | Json::Float(_) => value.number().ok_or(Failure::Value),
        _ => Err(Failure::Value),
    }
}

/// A percentage as the daemon prints it: digits, an optional fraction, a sign.
fn percent(value: &Json) -> Result<f64> {
    let text = value.string().ok_or(Failure::Value)?;
    let raw = text.strip_suffix('%').unwrap_or(text);
    let (whole, fraction) = raw
        .split_once('.')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    require(
        digits(whole) && fraction.is_none_or(digits),
        "Invalid container sample",
    )?;
    raw.parse().map_err(|_| Failure::Value)
}

/// Sample one managed container without its environment or log payloads.
fn container(host: &Host<'_>, lines: &mut Vec<String>, service: &str, name: &str) -> Result<()> {
    let item = state::decode(docker(host, &["inspect", "--format", INSPECT, name])?.as_bytes())?;
    item.object().ok_or(Failure::Value)?;
    let field = |name: &str| item.get(name).ok_or(Failure::Key);
    let flag = |value: &Json| Sample::Float(f64::from(u8::from(value.boolean() == Some(true))));
    gauge(
        lines,
        "container_running",
        flag(field("running")?),
        Some(service),
    );
    let restarts = Sample::Float(number(field("restarts")?)?);
    gauge(lines, "container_restarts", restarts, Some(service));
    gauge(lines, "container_oom", flag(field("oom")?), Some(service));
    let format = ["stats", "--no-stream", "--format", "{{json .}}", name];
    let stats = state::decode(docker(host, &format)?.as_bytes())?;
    stats.object().ok_or(Failure::Value)?;
    for (source, metric) in [
        ("CPUPerc", "container_cpu_percent"),
        ("MemPerc", "container_memory_percent"),
    ] {
        let sample = percent(stats.get(source).ok_or(Failure::Key)?)?;
        gauge(lines, metric, Sample::Float(sample), Some(service));
    }
    Ok(())
}

/// Observe each managed container; what was sampled before a failure stays.
fn containers(host: &Host<'_>, lines: &mut Vec<String>) {
    for (service, name) in SERVICES {
        let sampled = container(host, lines, service, name).is_ok();
        let ok = Sample::Whole(i128::from(sampled));
        gauge(lines, "container_collection_ok", ok, Some(service));
    }
}

/// Aggregate database pressure through one read-only statement.
fn pressure(host: &Host<'_>, lines: &mut Vec<String>) -> Result<()> {
    let arguments = [
        "exec",
        "--user",
        "postgres",
        POSTGRES,
        "psql",
        "-X",
        "-q",
        "-t",
        "-A",
        "-h",
        "/run/simplestchat-postgres",
        "-U",
        "postgres",
        "-d",
        "simplestchat",
        "-c",
        PG_QUERY,
    ];
    let sample = state::decode(docker(host, &arguments)?.as_bytes())?;
    sample.object().ok_or(Failure::Value)?;
    let keys = [
        "connections",
        "deadlocks",
        "rollbacks",
        "commits",
        "connection_limit",
        "blocked",
    ];
    for key in keys {
        let value = match sample.get(key).ok_or(Failure::Key)? {
            Json::Integer(text) => text.parse::<i128>().ok().filter(|value| *value >= 0),
            _ => None,
        };
        require(value.is_some(), "Invalid database sample")?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a gauge, as the earlier collector wrote it"
        )]
        let sample = Sample::Float(value.unwrap_or_default() as f64);
        gauge(lines, &format!("postgres_{key}"), sample, None);
    }
    Ok(())
}

fn database(host: &Host<'_>, lines: &mut Vec<String>) {
    let sampled = pressure(host, lines).is_ok();
    gauge(
        lines,
        "postgres_collection_ok",
        Sample::Whole(i128::from(sampled)),
        None,
    );
}

/// Whether the public proxy answers the readiness path with 200.
fn ready(host: &Host<'_>, domain: &str) -> bool {
    let url = format!("https://{domain}/ready");
    let argv = [
        "/usr/bin/curl",
        "--disable",
        "--silent",
        "--noproxy",
        "*",
        "--max-redirs",
        "0",
        "--proto",
        "=https",
        "--max-time",
        "3",
        "--output",
        "/dev/null",
        "--write-out",
        "%{http_code}",
        &url,
    ];
    command(host, argv.map(str::to_owned).to_vec(), 5.0).is_ok_and(|code| code == "200")
}

/// When the certificate the relay presents expires, in seconds since the
/// epoch. The handshake verifies the chain and the name; the system tool does
/// both and prints the leaf certificate, whose end date is then read.
fn relay_expiry(host: &Host<'_>, domain: &str) -> Result<i64> {
    let endpoint = format!("{domain}:5349");
    let handshake = [
        "/usr/bin/openssl",
        "s_client",
        "-connect",
        &endpoint,
        "-servername",
        domain,
        "-verify_hostname",
        domain,
        "-verify_return_error",
        "-no_tls1",
        "-no_tls1_1",
    ];
    let run = |argv: Vec<String>, input: &[u8]| {
        let oversized = "Oversized monitoring command response";
        host.commands.run(&Request {
            input: Some(input),
            ..Request::fixed(argv, 3.0, MAX_OUTPUT, oversized)
        })
    };
    let session = run(handshake.map(str::to_owned).to_vec(), b"")?;
    let read = ["/usr/bin/openssl", "x509", "-noout", "-enddate"];
    let line = String::from_utf8(run(read.map(str::to_owned).to_vec(), &session)?);
    let line = line.map_err(|_| Failure::Value)?;
    let expiry = line.trim().strip_prefix("notAfter=");
    require(expiry.is_some(), "Missing certificate expiry")?;
    let expiry = NaiveDateTime::parse_from_str(expiry.unwrap_or_default(), "%b %e %H:%M:%S %Y GMT");
    Ok(expiry.map_err(|_| Failure::Value)?.and_utc().timestamp())
}

/// Check public readiness and the relay's presented certificate.
fn readiness_and_tls(host: &Host<'_>, lines: &mut Vec<String>, domain: &str) {
    let ready = Sample::Float(f64::from(u8::from(ready(host, domain))));
    gauge(lines, "public_ready", ready, None);
    let expiry = relay_expiry(host, domain);
    if let Ok(expiry) = expiry {
        let expiry = Sample::Whole(i128::from(expiry));
        gauge(lines, "turn_certificate_expiry_seconds", expiry, None);
    }
    gauge(
        lines,
        "turn_tls_ok",
        Sample::Whole(i128::from(expiry.is_ok())),
        None,
    );
}

/// The receipts that may count as backups: one before each release and the
/// nightly ones.
fn receipts(host: &Host<'_>) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let results = host.paths.root.join("results");
    if results.is_dir() {
        for entry in std::fs::read_dir(&results)? {
            let entry = entry?;
            let named = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("release."));
            let receipt = entry.path().join("database-before.receipt.json");
            if named && (receipt.exists() || receipt.is_symlink()) {
                found.push(receipt);
            }
        }
    }
    let nightly = host.paths.root.join("backups/nightly");
    if nightly.is_dir() {
        for entry in std::fs::read_dir(&nightly)? {
            let path = entry?.path();
            let named = path.file_name().and_then(|name| name.to_str());
            if named.is_some_and(|name| name.ends_with(".receipt.json")) {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// The digest memory beside the state, as earlier collectors wrote it.
#[derive(serde::Serialize)]
struct Memory {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    entries: BTreeMap<String, Remembered>,
}

/// One receipt's remembered check, or an empty object when none is kept.
struct Remembered(Option<Entry>);

impl serde::Serialize for Remembered {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match &self.0 {
            Some(entry) => entry.serialize(serializer),
            None => serde::ser::SerializeMap::end(serializer.serialize_map(Some(0))?),
        }
    }
}

/// Report backup and restore evidence truthfully. A dump exists before the
/// backup starts and survives its failure, so only a receipt whose current
/// archive bytes verify counts, at its recorded completion time; a restore
/// exercise is separate operator-owned evidence and is never inferred.
fn evidence(host: &Host<'_>, lines: &mut Vec<String>) -> Result<()> {
    let receipts = receipts(host)?;
    require(
        receipts.len() <= MAX_BACKUPS,
        "Too many release backup entries",
    )?;
    let state_directory = &host.paths.state;
    state::protected(state_directory, host.owner, true, &[0o750], None)?;
    let cache_path = state_directory.join("backup-integrity-cache.json");
    let mut cached = state::decode(b"{}")?;
    if cache_path.exists() || cache_path.is_symlink() {
        state::protected(
            &cache_path,
            host.owner,
            false,
            &[0o600],
            Some(2 * 1024 * 1024),
        )?;
        let record = state::decode(&std::fs::read(&cache_path)?)?;
        record.object().ok_or(Failure::Value)?;
        let current =
            matches!(record.get("schemaVersion"), Some(Json::Integer(text)) if text == "1");
        require(current, "Invalid backup digest cache")?;
        cached = record.get("entries").ok_or(Failure::Value)?.clone();
        cached.object().ok_or(Failure::Value)?;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "microseconds of this era fit a double"
    )]
    let now = host.clock.now().timestamp_micros() as f64 / 1_000_000.0;
    let mut refreshed = BTreeMap::new();
    let mut latest = 0.0_f64;
    for receipt in receipts {
        let Some(key) = receipt.to_str() else {
            continue;
        };
        let remembered = cached.get(key);
        // A remembered value that is not an object disqualifies its receipt.
        if remembered.is_some_and(|entry| entry.object().is_none()) {
            continue;
        }
        let mut entry = remembered.and_then(Entry::from_json);
        if let Ok(completed) = backups::validated(&receipt, host.owner, now, Some(&mut entry)) {
            refreshed.insert(key.to_owned(), Remembered(entry));
            latest = latest.max(completed);
        }
    }
    state::atomic_json(
        &cache_path,
        &Memory {
            schema_version: 1,
            entries: refreshed,
        },
    )?;
    gauge(
        lines,
        "backup_last_success_seconds",
        Sample::Float(latest),
        None,
    );
    let restore = state_directory.join("restore-verified.timestamp");
    let verified = if restore.exists() {
        let modified = std::fs::metadata(&restore)?.modified()?;
        let since = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Failure::Value)?;
        Sample::Float(since.as_secs_f64())
    } else {
        Sample::Whole(0)
    };
    gauge(lines, "restore_last_success_seconds", verified, None);
    let releases = host.paths.root.join("releases");
    let releases = releases.to_str().ok_or(Failure::Value)?.to_owned();
    let usage = ["/usr/bin/du", "-sx", "--block-size=1"].map(str::to_owned);
    let usage = command(host, [usage.to_vec(), vec![releases]].concat(), 5.0)?;
    let size = usage.split_whitespace().next().ok_or(Failure::Value)?;
    let size = size.parse::<f64>().map_err(|_| Failure::Value)?;
    gauge(lines, "release_storage_bytes", Sample::Float(size), None);
    Ok(())
}

/// A lower-case DNS name of at least two labels ending in a letter-only one.
pub(super) fn domain(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    let alphanumeric = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let label = |label: &&str| {
        let bytes = label.as_bytes();
        (1..=63).contains(&bytes.len())
            && bytes.iter().all(|byte| alphanumeric(byte) || *byte == b'-')
            && bytes.first().is_some_and(alphanumeric)
            && bytes.last().is_some_and(alphanumeric)
    };
    let Some((last, others)) = labels.split_last() else {
        return false;
    };
    !others.is_empty()
        && others.iter().all(label)
        && (2..=63).contains(&last.len())
        && last.bytes().all(|byte| byte.is_ascii_lowercase())
}

/// Text at `key` of the object stored in `path`.
pub(super) fn setting(path: &std::path::Path, key: &str) -> Result<(Json, String)> {
    let value = state::decode(&std::fs::read(path)?)?;
    value.object().ok_or(Failure::Value)?;
    let text = value
        .get(key)
        .ok_or(Failure::Key)?
        .string()
        .ok_or(Failure::Value)?
        .to_owned();
    Ok((value, text))
}

/// Whether `text` is a full lower-case commit identifier.
pub(super) fn revision(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Sample everything once and publish the textfile. Returns the line printed
/// for the journal.
pub(super) fn collect(host: &Host<'_>) -> Result<&'static str> {
    let started = host.clock.monotonic();
    let mut lines = Vec::new();
    let (configuration, name) = setting(&host.paths.settings, "domain")?;
    require(domain(&name), "Invalid monitored domain")?;
    let (_, selected) = setting(&host.paths.images, "revision")?;
    require(revision(&selected), "Invalid monitored revision")?;
    containers(host, &mut lines);
    database(host, &mut lines);
    readiness_and_tls(host, &mut lines, &name);
    let evident = evidence(host, &mut lines).is_ok();
    gauge(
        &mut lines,
        "evidence_collection_ok",
        Sample::Whole(i128::from(evident)),
        None,
    );
    let today = host.clock.now().date_naive();
    let state_directory = &host.paths.state;
    let counted = transfer::transfer(
        &mut lines,
        &configuration,
        &host.paths.network,
        state_directory,
        today,
    );
    let counted = Sample::Whole(i128::from(counted.is_ok()));
    gauge(&mut lines, "transfer_collection_ok", counted, None);
    match alerts::record(host, Some(&selected)) {
        Ok((healthy, pending, dropped)) => {
            let healthy = Sample::Float(f64::from(u8::from(healthy)));
            gauge(&mut lines, "recorder_ok", healthy, None);
            let pending = Sample::Whole(i128::try_from(pending).map_err(|_| Failure::Value)?);
            gauge(&mut lines, "recorder_spool_snapshots", pending, None);
            let dropped = Sample::Whole(i128::from(dropped));
            gauge(&mut lines, "recorder_spool_dropped_total", dropped, None);
        }
        Err(_) => gauge(&mut lines, "recorder_ok", Sample::Whole(0), None),
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "microseconds of this era fit a double"
    )]
    let now = host.clock.now().timestamp_micros() as f64 / 1_000_000.0;
    gauge(
        &mut lines,
        "collector_timestamp_seconds",
        Sample::Float(now),
        None,
    );
    let elapsed = (host.clock.monotonic() - started).as_secs_f64();
    gauge(
        &mut lines,
        "collector_duration_seconds",
        Sample::Float(elapsed),
        None,
    );
    let target = state_directory.join("textfile/operations.prom");
    state::atomic(&target, (lines.join("\n") + "\n").as_bytes())?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))?;
    Ok("{\"event\": \"monitoring_collection\", \"completed\": true}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::tests::Scripted;
    use crate::host::state::tests::Scratch;
    use crate::host::tests::{Fixed, paths};
    use serde_json::Value;
    use std::path::Path;

    fn private(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The deployment both recorded scenarios ran against.
    fn deployment(recorded: &Value, scratch: &Path, healthy: bool) {
        let paths = paths(scratch);
        std::fs::create_dir_all(paths.state.join("textfile")).unwrap();
        private(&paths.state, 0o750);
        let release = paths.root.join("results/release.one");
        std::fs::create_dir_all(&release).unwrap();
        private(&release, 0o700);
        let nightly = paths.root.join("backups/nightly");
        std::fs::create_dir_all(&nightly).unwrap();
        private(&nightly, 0o700);
        std::fs::create_dir(paths.root.join("releases")).unwrap();
        let dump = recorded["dump"].as_str().unwrap();
        let receipt = serde_json::json!({
            "schemaVersion": 1, "dump": "database-before.dump", "bytes": dump.len(),
            "sha256": recorded["digest"], "completedAt": "2026-11-10T01:02:03+00:00",
        });
        let broken = serde_json::json!({
            "schemaVersion": 1, "dump": "20261109T010203Z.dump", "bytes": 5,
            "sha256": "0".repeat(64), "completedAt": "2026-11-11T01:02:03+00:00",
        });
        for (path, text) in [
            (
                release.join("database-before.receipt.json"),
                receipt.to_string(),
            ),
            (release.join("database-before.dump"), dump.to_owned()),
            (
                nightly.join("20261109T010203Z.receipt.json"),
                broken.to_string(),
            ),
        ] {
            std::fs::write(&path, text).unwrap();
            private(&path, 0o600);
        }
        if healthy {
            let stamp = paths.state.join("restore-verified.timestamp");
            let file = std::fs::File::create(&stamp).unwrap();
            let verified =
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_794_300_000_250);
            file.set_modified(verified).unwrap();
        }
        let statistics = paths.network.join("eth0/statistics");
        std::fs::create_dir_all(&statistics).unwrap();
        std::fs::write(statistics.join("tx_bytes"), "5000\n").unwrap();
        std::fs::write(statistics.join("rx_bytes"), "9000\n").unwrap();
        let earlier = r#"{"period":"2026-11-01","tx_bytes":100,"rx_bytes":200,"last_tx":4000,"last_rx":8500}"#;
        std::fs::write(paths.state.join("transfer.json"), earlier).unwrap();
        let settings = r#"{"domain":"chat.example.test","transfer_allowance_tb":2}"#;
        std::fs::write(&paths.settings, settings).unwrap();
        let images = serde_json::json!({"revision": recorded["revision"]});
        std::fs::write(&paths.images, images.to_string()).unwrap();
        std::fs::create_dir(&paths.sql).unwrap();
        std::fs::write(paths.sql.join("monitoring-alerts.sql"), "-- reviewed\n").unwrap();
    }

    /// What the healthy recorded host answered to each fixed command.
    fn healthy(request: &Request<'_>) -> Result<Vec<u8>> {
        let argv = &request.argv;
        let has = |word: &str| argv.iter().any(|argument| argument == word);
        let last = argv.last().unwrap().as_str();
        let text = match argv[0].as_str() {
            "/usr/bin/curl" if last.ends_with("/ready") => "200".to_owned(),
            "/usr/bin/curl" => concat!(
                r#"{"status":"success","data":{"alerts":[{"state":"firing","labels":"#,
                r#"{"alertname":"ContainerRestart","severity":"warning","service":"postgres"}}]}}"#
            )
            .to_owned(),
            "/usr/bin/openssl" if has("s_client") => "-----BEGIN CERTIFICATE-----\n".to_owned(),
            "/usr/bin/openssl" => "notAfter=Jan  2 00:02:02 2027 GMT\n".to_owned(),
            "/usr/bin/du" => format!("123456\t{last}\n"),
            _ if has("inspect") => {
                let restarts = match last {
                    "simplestchat-public-postgres-1" => 2,
                    "simplestchat-public-caddy-1" => 1,
                    _ => 0,
                };
                format!("{{\"running\":true,\"restarts\":{restarts},\"oom\":false}}\n")
            }
            _ if has("stats") && last == "simplestchat-turn-turn-1" => {
                "{\"CPUPerc\":\"--\",\"MemPerc\":\"0.10%\"}\n".to_owned()
            }
            _ if has("stats") => {
                "{\"CPUPerc\":\"12.50%\",\"MemPerc\":\"3%\",\"Name\":\"x\"}\n".to_owned()
            }
            _ if has("-c") => concat!(
                r#"{"connections":7,"deadlocks":0,"rollbacks":3,"commits":4567,"#,
                r#""connection_limit":100,"blocked":0}"#,
                "\n"
            )
            .to_owned(),
            _ => String::new(),
        };
        Ok(text.into_bytes())
    }

    /// The whole textfile, the journal line and the daemon and disk commands
    /// are the recorded collector's, on a healthy host and on one where every
    /// command and connection fails.
    #[test]
    fn the_textfile_and_fixed_commands_are_the_recorded_collectors() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_collect.json")).unwrap();
        let scenarios = recorded["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 2);
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap();
            let well = scenario["healthy"] == true;
            let scratch = Scratch::new();
            deployment(&recorded, scratch.path(), well);
            let clock = Fixed::at("2026-11-11T12:00:00.5Z");
            clock.tick.set(std::time::Duration::from_millis(250));
            let commands = Scripted::new(|request| {
                if well {
                    healthy(request)
                } else {
                    Err(Failure::Command)
                }
            });
            let paths = paths(scratch.path());
            let owner =
                std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(scratch.path()).unwrap());
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths,
                owner,
            };
            let printed = collect(&host).unwrap();
            assert_eq!(printed, scenario["stdout"].as_str().unwrap(), "{name}");
            let target = host.paths.state.join("textfile/operations.prom");
            let written = std::fs::read_to_string(&target).unwrap();
            let expected = scenario["prom"].as_str().unwrap();
            for (ours, theirs) in written.lines().zip(expected.lines()) {
                assert_eq!(ours, theirs, "{name}");
            }
            assert_eq!(written, expected, "{name}");
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(format!("0o{mode:o}"), scenario["mode"].as_str().unwrap());
            // The daemon and disk commands, in order, with their deadlines.
            let root = scratch.path().to_str().unwrap();
            let recorded_calls = scenario["calls"].as_array().unwrap();
            let expected = recorded_calls
                .iter()
                .filter(|call| call.get("argv").is_some());
            let asked = commands.asked.borrow();
            let ours = asked.iter().filter(|asked| {
                !["/usr/bin/curl", "/usr/bin/openssl"].contains(&asked.argv[0].as_str())
            });
            let ours: Vec<_> = ours.collect();
            let expected: Vec<_> = expected.collect();
            assert_eq!(ours.len(), expected.len(), "{name}");
            for (ours, theirs) in ours.iter().zip(expected) {
                let argv = theirs["argv"].as_array().unwrap();
                let argv = argv
                    .iter()
                    .map(|word| word.as_str().unwrap().replace("{root}", root));
                assert_eq!(ours.argv, argv.collect::<Vec<_>>(), "{name}");
                assert_eq!(Some(ours.seconds), theirs["timeout"].as_f64(), "{name}");
                assert_eq!(
                    ours.input
                        .as_deref()
                        .map(String::from_utf8_lossy)
                        .as_deref(),
                    theirs["input"].as_str()
                );
            }
            let spool = alerts::entries(&host.paths.state.join("spool")).unwrap();
            let names = spool
                .iter()
                .map(|path| path.file_name().unwrap().to_str().unwrap());
            let recorded_spool = scenario["spool"].as_array().unwrap();
            let recorded_spool = recorded_spool.iter().map(|name| name.as_str().unwrap());
            assert_eq!(
                names.collect::<Vec<_>>(),
                recorded_spool.collect::<Vec<_>>()
            );
            let cursor = std::fs::read(host.paths.state.join("recorder.json")).unwrap();
            let cursor: Value = serde_json::from_slice(&cursor).unwrap();
            assert_eq!(cursor, scenario["recorder"], "{name}");
            let totals = std::fs::read(host.paths.state.join("transfer.json")).unwrap();
            let totals: Value = serde_json::from_slice(&totals).unwrap();
            assert_eq!(totals, scenario["transfer"], "{name}");
            private(&host.paths.state, 0o700);
        }
    }
}
