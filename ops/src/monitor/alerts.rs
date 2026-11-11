//! Allowlisted Prometheus incidents. Only fixed rule names and bounded
//! resource labels enter an incident; free-form annotations never do.

use super::command::Request;
use super::failure::{Failure, Result, require};
use super::host::{DOCKER, Host, POSTGRES};
use super::state;
use crate::json::Json;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Every alert rule the recorder accepts.
pub(super) const RULES: [&str; 37] = [
    "AdmissionRefusals",
    "ApplicationUnavailable",
    "BackupStale",
    "CollectorFailed",
    "CollectorStale",
    "ContainerMemoryPressure",
    "ContainerRestart",
    "ContainerUnavailable",
    "DatabasePoolPressure",
    "DatabaseUnavailable",
    "ExternalCheckFailed",
    "ExternalChecksStale",
    "ExternalCoverageIncomplete",
    "ExternalImportUnavailable",
    "ExternalSpoolLoss",
    "HostDiskLow",
    "HostMemoryLow",
    "MetricsUnavailable",
    "PostgresConnectionPressure",
    "PostgresUnavailable",
    "PrometheusUnavailable",
    "PublicReadinessFailed",
    "QualityBudgetExhausted",
    "QualitySampleStale",
    "RecorderSpoolLoss",
    "RecorderUnavailable",
    "RestoreEvidenceMissing",
    "ServerErrors",
    "TelemetrySeriesDropped",
    "TransferAccountingFailed",
    "TransferAllowanceExhausted",
    "TransferAllowanceNearlyUsed",
    "TurnAllocationPressure",
    "TurnCertificateExpiring",
    "TurnMetricsUnavailable",
    "TurnTLSUnavailable",
    "WorkerDeath",
];

/// The resource labels an incident may carry.
pub(super) const RESOURCES: [&str; 4] = ["component", "instance", "job", "service"];

/// One firing rule on one resource. The key is the identity under which the
/// database keeps its history.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct Incident {
    pub key: String,
    pub rule: String,
    pub severity: String,
    pub resource: BTreeMap<String, String>,
}

/// Whether `value` is a resource label value: 1 to 96 of `[A-Za-z0-9_.:/-]`.
fn label(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:/-".contains(&byte))
}

/// Admit one rule, severity and resource, and derive its stable key: the
/// SHA-256 of the compact JSON pair with the resource's keys sorted.
pub(super) fn incident(
    rule: &str,
    severity: &str,
    resource: &BTreeMap<String, Option<String>>,
) -> Result<Incident> {
    require(
        RULES.contains(&rule) && ["warning", "critical"].contains(&severity),
        "Unknown alert rule",
    )?;
    require(
        resource.keys().all(|key| RESOURCES.contains(&key.as_str())),
        "Unknown resource label",
    )?;
    let mut admitted = BTreeMap::new();
    for (key, value) in resource {
        let value = value.as_deref().filter(|value| label(value));
        require(value.is_some(), "Invalid alert label")?;
        admitted.insert(key.clone(), value.unwrap_or_default().to_owned());
    }
    // Admitted text needs no JSON escaping: the alphabet has no quote,
    // backslash or control character.
    let pairs = admitted
        .iter()
        .map(|(key, value)| format!("\"{key}\":\"{value}\""));
    let identity = format!("[\"{rule}\",{{{}}}]", pairs.collect::<Vec<_>>().join(","));
    let key = Sha256::digest(identity.as_bytes());
    Ok(Incident {
        key: key.iter().map(|byte| format!("{byte:02x}")).collect(),
        rule: rule.to_owned(),
        severity: severity.to_owned(),
        resource: admitted,
    })
}

const MAX_BYTES: usize = 65536;
const MAX_ALERTS: usize = 100;
/// One day of minute snapshots.
pub(super) const MAX_SNAPSHOTS: usize = 1440;
/// The most retained snapshots replayed by one collection.
const REPLAYED: usize = 120;

/// One observation of the firing rules. An incomplete observation never
/// resolves an incident.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct Snapshot {
    #[serde(rename = "observedAt")]
    pub observed_at: String,
    pub revision: Option<String>,
    pub complete: bool,
    pub alerts: Vec<Incident>,
}

/// The recorder's durable cursor beside its spool.
#[derive(serde::Serialize)]
struct Cursor<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped: Option<String>,
    #[serde(rename = "databaseOk", skip_serializing_if = "Option::is_none")]
    database_ok: Option<bool>,
    #[serde(rename = "lastObservedAt")]
    last_observed_at: &'a str,
}

/// An instant as the recorder spells it: UTC with microseconds.
pub(super) fn stamp(instant: DateTime<Utc>) -> String {
    instant.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
}

/// Read back an instant with its offset.
pub(super) fn instant(text: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| Failure::Value)?;
    Ok(parsed.with_timezone(&Utc))
}

/// A rule the recorder raises itself, on no particular resource.
fn own(rule: &str) -> Incident {
    incident(rule, "critical", &BTreeMap::new()).expect("a fixed rule")
}

/// The firing rules Prometheus reports, each admitted or the whole answer refused.
fn firing(host: &Host<'_>) -> Result<Vec<Incident>> {
    let url = "http://127.0.0.1:9090/api/v1/alerts";
    let value = state::decode(&host.get(url, MAX_BYTES, "Oversized alert response")?)?;
    value.object().ok_or(Failure::Value)?;
    let succeeded = value.get("status").and_then(Json::string) == Some("success");
    require(succeeded, "Alert response failed")?;
    let data = value.get("data").ok_or(Failure::Key)?;
    data.object().ok_or(Failure::Value)?;
    let entries = data.get("alerts").ok_or(Failure::Key)?;
    let entries = entries.array().ok_or(Failure::Value)?;
    require(entries.len() <= MAX_ALERTS, "Too many active alerts")?;
    let mut alerts = Vec::new();
    for entry in entries {
        entry.object().ok_or(Failure::Value)?;
        if entry.get("state").and_then(Json::string) != Some("firing") {
            continue;
        }
        let labels = entry.get("labels").ok_or(Failure::Key)?;
        labels.object().ok_or(Failure::Value)?;
        let text = |name: &str| -> Result<&str> {
            let value = labels.get(name).ok_or(Failure::Key)?;
            value.string().ok_or(Failure::Value)
        };
        let resource = RESOURCES.iter().filter_map(|key| {
            let value = labels.get(key)?.string().map(str::to_owned);
            Some(((*key).to_owned(), value))
        });
        alerts.push(incident(
            text("alertname")?,
            text("severity")?,
            &resource.collect(),
        )?);
    }
    Ok(alerts)
}

/// Read one bounded local rules answer. Any failure is itself an incident and
/// leaves the snapshot incomplete, so it resolves nothing.
pub(super) fn snapshot(host: &Host<'_>, revision: Option<&str>) -> Snapshot {
    let (complete, alerts) = match firing(host) {
        Ok(alerts) => (true, alerts),
        Err(_) => (false, vec![own("PrometheusUnavailable")]),
    };
    Snapshot {
        observed_at: stamp(host.clock.now()),
        revision: revision.map(str::to_owned),
        complete,
        alerts,
    }
}

/// The spool's entries in replay order.
pub(super) fn entries(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    if !directory.exists() {
        return Ok(found);
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// How much one spool may hold.
pub(super) struct Bound {
    /// The most snapshots kept.
    pub entries: usize,
    /// The most bytes one snapshot may have.
    pub bytes: usize,
    /// The refusal for a larger one.
    pub oversized: &'static str,
}

/// The recorder's spool: one day of minute snapshots.
pub(super) const SPOOL: Bound = Bound {
    entries: MAX_SNAPSHOTS,
    bytes: MAX_BYTES,
    oversized: "Oversized incident snapshot",
};

/// Append one snapshot under the next sequence, keeping at most the bound:
/// the earliest observation and the newest evidence stay, and every discarded
/// snapshot is counted.
pub(super) fn persist(
    directory: &Path,
    data: &[u8],
    now: DateTime<Utc>,
    bound: &Bound,
) -> Result<(PathBuf, usize)> {
    if !directory.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(directory)?;
    }
    require(data.len() <= bound.bytes, bound.oversized)?;
    let mut retained = entries(directory)?;
    let clock = u128::try_from(now.timestamp_nanos_opt().ok_or(Failure::Value)?);
    let mut sequence = clock.map_err(|_| Failure::Value)?;
    if let Some(last) = retained.last() {
        let stem = last.file_stem().and_then(|stem| stem.to_str());
        let last: u128 = stem
            .and_then(|stem| stem.parse().ok())
            .ok_or(Failure::Value)?;
        sequence = sequence.max(last + 1);
    }
    let destination = directory.join(format!("{sequence:020}.json"));
    state::atomic(&destination, data)?;
    retained.push(destination.clone());
    let dropped = retained.len().saturating_sub(bound.entries);
    for path in &retained[1..=dropped] {
        std::fs::remove_file(path)?;
    }
    Ok((destination, dropped))
}

/// Send one snapshot through the reviewed statement as a quoted variable.
fn commit(host: &Host<'_>, sql: &[u8], payload: &str) -> Result<()> {
    let mut argv: Vec<String> = DOCKER.map(str::to_owned).to_vec();
    let tail = [
        "exec",
        "-i",
        "--user",
        "postgres",
        POSTGRES,
        "psql",
        "-X",
        "-q",
        "-h",
        "/run/simplestchat-postgres",
        "-U",
        "postgres",
        "-d",
        "simplestchat",
        "--set",
    ];
    argv.extend(tail.map(str::to_owned));
    argv.push(format!("payload={payload}"));
    let request = Request {
        input: Some(sql),
        ..Request::fixed(argv, 7.0, 1024 * 1024, "Oversized alert statement response")
    };
    host.commands.run(&request).map(|_| ())
}

/// Record the current snapshot and replay retained ones in order. A failed
/// statement leaves every snapshot in place, marks the current one with the
/// database's own incident and stops; committed snapshots are removed.
/// Returns whether the database took everything offered, how many snapshots
/// remain, and how many were ever discarded.
pub(super) fn record(host: &Host<'_>, revision: Option<&str>) -> Result<(bool, usize, u64)> {
    let cursor_path = host.paths.state.join("recorder.json");
    let previous = state::object_or_empty(&cursor_path, 1024 * 1024)?;
    let text = |name: &str| -> Result<Option<&str>> {
        let value = previous
            .get(name)
            .map(|value| value.string().ok_or(Failure::Value));
        value.transpose()
    };
    let mut value = snapshot(host, revision);
    let spool = host.paths.state.join("spool");
    // The persisted watermark precedes database writes, so a backward step of
    // the wall clock cannot make a new observation look like a committed one.
    let mut watermark = instant(text("lastObservedAt")?.unwrap_or(&value.observed_at))?;
    if let Some(newest) = entries(&spool)?.last() {
        let newest = state::decode(&state::read(newest, MAX_BYTES)?)?;
        let observed = newest.get("observedAt").ok_or(Failure::Key)?;
        watermark = watermark.max(instant(observed.string().ok_or(Failure::Value)?)?);
    }
    let step = chrono::Duration::microseconds(1);
    value.observed_at = stamp(instant(&value.observed_at)?.max(watermark + step));
    let dropped_before: u64 = match text("dropped")? {
        Some(count) => count.parse().map_err(|_| Failure::Value)?,
        None => 0,
    };
    let database_ok = previous.get("databaseOk").and_then(Json::boolean);
    let first = Cursor {
        dropped: text("dropped")?.map(str::to_owned),
        database_ok,
        last_observed_at: &value.observed_at,
    };
    state::atomic_json(&cursor_path, &first)?;
    let compact = serde_json::to_vec(&value).map_err(|_| Failure::Value)?;
    let (current, discarded) = persist(&spool, &compact, host.clock.now(), &SPOOL)?;
    let dropped = dropped_before + u64::try_from(discarded).map_err(|_| Failure::Value)?;
    let sql = std::fs::read(host.paths.sql.join("monitoring-alerts.sql"))?;
    let deadline = host.clock.monotonic() + std::time::Duration::from_secs(12);
    let mut succeeded = true;
    for path in entries(&spool)?.iter().take(REPLAYED) {
        if host.clock.monotonic() >= deadline {
            break;
        }
        let data = std::fs::read(path)?;
        require(
            data.len() <= MAX_BYTES,
            "Oversized retained incident snapshot",
        )?;
        let payload = String::from_utf8(data).map_err(|_| Failure::Value)?;
        if commit(host, &sql, &payload).is_err() {
            succeeded = false;
            // Keep this failure even if PostgreSQL returns before the next scrape.
            value.alerts.push(own("DatabaseUnavailable"));
            value.complete = false;
            let later = instant(&value.observed_at)? + step;
            value.observed_at = stamp(host.clock.now().max(later));
            state::atomic_json(&current, &value)?;
            break;
        }
        std::fs::remove_file(path)?;
    }
    let last = Cursor {
        dropped: Some(dropped.to_string()),
        database_ok: Some(succeeded),
        last_observed_at: &value.observed_at,
    };
    state::atomic_json(&cursor_path, &last)?;
    Ok((succeeded, entries(&spool)?.len(), dropped))
}

#[cfg(test)]
mod tests {
    use super::super::command::tests::Scripted;
    use super::super::command::{Commands, Request};
    use super::super::failure::Failure;
    use super::super::host::tests::{Fixed, paths};
    use super::super::state::tests::Scratch;
    use super::*;

    /// One deployment under `scratch` with its state and statement in place.
    fn host<'a>(commands: &'a dyn Commands, clock: &'a Fixed, scratch: &Scratch) -> Host<'a> {
        let paths = paths(scratch.path());
        std::fs::create_dir_all(&paths.state).unwrap();
        std::fs::create_dir_all(&paths.sql).unwrap();
        std::fs::write(paths.sql.join("monitoring-alerts.sql"), b"-- reviewed\n").unwrap();
        Host {
            commands,
            clock,
            paths,
            owner: 0,
        }
    }

    /// Whether a request is the rules query rather than a database statement.
    fn rules(request: &Request<'_>) -> bool {
        request.argv[0] == "/usr/bin/curl"
    }

    fn decoded(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    const QUIET: &[u8] = br#"{"status":"success","data":{"alerts":[]}}"#;

    /// Firing allowlisted rules become incidents with only resource labels; an
    /// unknown rule, a refused answer or an unreachable Prometheus leaves one
    /// incident of its own and an incomplete snapshot.
    #[test]
    fn only_complete_allowlisted_answers_are_complete_snapshots() {
        let scratch = Scratch::new();
        let clock = Fixed::at("2026-09-23T10:00:00.000001Z");
        let firing = br#"{"status":"success","data":{"alerts":[
            {"state":"firing","labels":{"alertname":"ContainerRestart","severity":"warning",
             "service":"app","pod":"ignored","instance":"host:9100"}},
            {"state":"pending","labels":{"alertname":"Unknown","severity":"none"}},
            {"state":"firing","labels":{"alertname":"WorkerDeath","severity":"critical"}}]}}"#;
        let commands = Scripted::new(|_| Ok(firing.to_vec()));
        let value = snapshot(&host(&commands, &clock, &scratch), Some("abc"));
        assert!(value.complete);
        assert_eq!(value.observed_at, "2026-09-23T10:00:00.000001+00:00");
        assert_eq!(value.revision.as_deref(), Some("abc"));
        let rules = value.alerts.iter().map(|alert| alert.rule.as_str());
        assert_eq!(
            rules.collect::<Vec<_>>(),
            ["ContainerRestart", "WorkerDeath"]
        );
        let labels = value.alerts[0].resource.keys().map(String::as_str);
        assert_eq!(labels.collect::<Vec<_>>(), ["instance", "service"]);
        let asked = commands.asked.borrow();
        assert_eq!(asked.len(), 1);
        let argv = asked[0].argv.join(" ");
        assert!(argv.starts_with("/usr/bin/curl --disable --silent --show-error --fail"));
        assert!(argv.contains("--noproxy * --max-redirs 0"));
        assert!(argv.ends_with("--max-time 3 --output - http://127.0.0.1:9090/api/v1/alerts"));
        drop(asked);
        let unknown = br#"{"status":"success","data":{"alerts":[
            {"state":"firing","labels":{"alertname":"Unknown","severity":"warning"}}]}}"#;
        let failed = br#"{"status":"error","data":{"alerts":[]}}"#;
        let unlabelled = br#"{"status":"success","data":{"alerts":[{"state":"firing"}]}}"#;
        let many = format!(
            r#"{{"status":"success","data":{{"alerts":[{}]}}}}"#,
            vec![r#"{"state":"resolved"}"#; 101].join(",")
        );
        let answers: [&[u8]; 6] = [
            unknown,
            failed,
            unlabelled,
            many.as_bytes(),
            b"[1]",
            b"not json",
        ];
        for answer in answers {
            let commands = Scripted::new(|_| Ok(answer.to_vec()));
            let value = snapshot(&host(&commands, &clock, &Scratch::new()), None);
            assert!(!value.complete);
            assert_eq!(value.alerts, [own("PrometheusUnavailable")]);
        }
        let commands = Scripted::new(|_| Err(Failure::Command));
        let value = snapshot(&host(&commands, &clock, &Scratch::new()), None);
        assert!(!value.complete && value.alerts.len() == 1);
        let quiet = Scripted::new(|_| Ok(QUIET.to_vec()));
        let value = snapshot(&host(&quiet, &clock, &Scratch::new()), None);
        assert!(value.complete && value.alerts.is_empty());
    }

    /// An outage cannot consume unbounded disk, and what is lost is counted:
    /// the earliest observation and the newest evidence stay.
    #[test]
    fn spool_keeps_the_oldest_and_newest_and_counts_discards() {
        let scratch = Scratch::new();
        let spool = scratch.path().join("spool");
        let clock = Fixed::at("2026-09-23T10:00:00Z");
        let three = Bound {
            entries: 3,
            ..SPOOL
        };
        for number in 0..3 {
            let data = format!("{{\"number\":{number}}}");
            let kept = persist(&spool, data.as_bytes(), clock.now.get(), &three).unwrap();
            assert_eq!(kept.1, 0);
        }
        let (newest, dropped) =
            persist(&spool, b"{\"number\":3}", clock.now.get(), &three).unwrap();
        assert_eq!(dropped, 1);
        let retained = entries(&spool).unwrap();
        let numbers = retained
            .iter()
            .map(|path| decoded(path)["number"].as_u64().unwrap());
        assert_eq!(numbers.collect::<Vec<_>>(), [0, 2, 3]);
        assert_eq!(retained.last(), Some(&newest));
        let name = newest.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), "00000000000000000000.json".len());
        let large = vec![b' '; MAX_BYTES + 1];
        let refused = persist(&spool, &large, clock.now.get(), &three);
        assert_eq!(
            refused,
            Err(Failure::Refused("Oversized incident snapshot"))
        );
    }

    /// A statement that fails leaves the snapshot in the spool carrying the
    /// database's own incident; the next healthy run commits and removes it.
    #[test]
    fn a_failed_statement_keeps_the_snapshot_and_a_healthy_run_commits_it() {
        let scratch = Scratch::new();
        let clock = Fixed::at("2026-09-23T10:00:00Z");
        let down = Scripted::new(|request| {
            if rules(request) {
                Ok(QUIET.to_vec())
            } else {
                Err(Failure::Command)
            }
        });
        let host_down = host(&down, &clock, &scratch);
        assert_eq!(record(&host_down, None), Ok((false, 1, 0)));
        let spool = host_down.paths.state.join("spool");
        let entry = decoded(&entries(&spool).unwrap()[0]);
        assert_eq!(entry["complete"], false);
        assert_eq!(entry["alerts"][0]["rule"], "DatabaseUnavailable");
        let statement = down.asked.borrow()[1].clone();
        let argv = statement.argv.join(" ");
        assert!(argv.starts_with(
            "/usr/bin/docker --host unix:///var/run/docker.sock exec -i --user postgres \
             simplestchat-public-postgres-1 psql -X -q -h /run/simplestchat-postgres \
             -U postgres -d simplestchat --set payload={\"observedAt\":"
        ));
        assert_eq!(statement.input.as_deref(), Some(&b"-- reviewed\n"[..]));
        assert!((statement.seconds - 7.0).abs() < f64::EPSILON);
        let cursor = decoded(&host_down.paths.state.join("recorder.json"));
        assert_eq!(cursor["dropped"], "0");
        assert_eq!(cursor["databaseOk"], false);
        let up = Scripted::new(|request| {
            Ok(if rules(request) {
                QUIET.to_vec()
            } else {
                Vec::new()
            })
        });
        let host_up = host(&up, &clock, &scratch);
        assert_eq!(record(&host_up, None), Ok((true, 0, 0)));
        // Both the retained failure and the new observation were offered, in order.
        let offered = up.asked.borrow();
        assert_eq!(
            offered
                .iter()
                .filter(|asked| asked.argv[0] != "/usr/bin/curl")
                .count(),
            2
        );
        let cursor = decoded(&host_up.paths.state.join("recorder.json"));
        assert_eq!(cursor["databaseOk"], true);
        assert!(cursor["lastObservedAt"].as_str().unwrap() > "2026-09-23T10:00:00.000001");
    }

    /// A clock correction cannot overwrite an older incident or move the
    /// replay cursor back: the new snapshot sorts after the retained one and
    /// only it is marked.
    #[test]
    fn a_backward_clock_keeps_order_and_marks_only_the_current_entry() {
        let scratch = Scratch::new();
        let later = Fixed::at("2030-01-01T00:00:00Z");
        let earlier = Fixed::at("2020-01-01T00:00:00Z");
        let down = Scripted::new(|request| {
            if rules(request) {
                Ok(QUIET.to_vec())
            } else {
                Err(Failure::Command)
            }
        });
        let host = host(&down, &earlier, &scratch);
        let spool = host.paths.state.join("spool");
        let original = br#"{"observedAt":"2030-01-01T00:00:00+00:00","revision":null,"complete":true,"alerts":[]}"#;
        let (first, _) = persist(&spool, original, later.now.get(), &SPOOL).unwrap();
        assert_eq!(record(&host, None), Ok((false, 2, 0)));
        assert_eq!(std::fs::read(&first).unwrap(), original);
        let retained = entries(&spool).unwrap();
        assert_eq!(retained[0], first);
        let newest = decoded(&retained[1]);
        assert!(newest["observedAt"].as_str().unwrap() > "2030-01-01T00:00:00+00:00");
        assert_eq!(newest["observedAt"], "2030-01-01T00:00:00.000002+00:00");
        assert_eq!(newest["alerts"][0]["rule"], "DatabaseUnavailable");
        assert_eq!(newest["complete"], false);
    }

    /// Foreign state is a fault, never a reset: a cursor or a newest entry
    /// that cannot be read stops the recorder before it writes.
    #[test]
    fn foreign_cursor_or_spool_state_is_a_fault() {
        let clock = Fixed::at("2026-09-23T10:00:00Z");
        let quiet = Scripted::new(|_| Ok(QUIET.to_vec()));
        let cases: [(&str, &[u8], Failure); 4] = [
            ("recorder.json", b"[]", Failure::Value),
            ("recorder.json", br#"{"lastObservedAt":7}"#, Failure::Value),
            (
                "recorder.json",
                br#"{"lastObservedAt":"yesterday"}"#,
                Failure::Value,
            ),
            ("recorder.json", br#"{"dropped":"many"}"#, Failure::Value),
        ];
        for (name, bytes, failure) in cases {
            let scratch = Scratch::new();
            let host = host(&quiet, &clock, &scratch);
            std::fs::write(host.paths.state.join(name), bytes).unwrap();
            assert_eq!(record(&host, None), Err(failure), "{name}");
            assert!(entries(&host.paths.state.join("spool")).unwrap().is_empty());
        }
        let scratch = Scratch::new();
        let host = host(&quiet, &clock, &scratch);
        let spool = host.paths.state.join("spool");
        persist(&spool, b"{\"number\":1}", clock.now.get(), &SPOOL).unwrap();
        assert_eq!(record(&host, None), Err(Failure::Key));
    }

    /// The rule list is the recorded one, and every recorded incident has the
    /// recorded key or refusal.
    #[test]
    fn incidents_have_the_recorded_keys_and_refusals() {
        let recorded = super::super::recorded::reference();
        let rules = recorded["rules"].as_array().unwrap();
        let rules = rules.iter().map(|rule| rule.as_str().unwrap());
        assert_eq!(rules.collect::<Vec<_>>(), RULES);
        let cases = recorded["incident"].as_array().unwrap();
        assert_eq!(cases.len(), 15);
        for case in cases {
            let arguments = case["arguments"].as_array().unwrap();
            let resource = arguments[2].as_object().unwrap();
            // A label that is not text is carried as absent and refused.
            let resource = resource
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().map(str::to_owned)))
                .collect();
            let answer = incident(
                arguments[0].as_str().unwrap(),
                arguments[1].as_str().unwrap(),
                &resource,
            );
            match (&answer, case.get("refused")) {
                (Err(Failure::Refused(message)), Some(expected)) => {
                    assert_eq!(*message, expected.as_str().unwrap(), "{case}");
                }
                (Ok(incident), None) => {
                    // The fixture calls the recorded key `digest`: a secret
                    // scanner reads a hexadecimal value labelled as a key as
                    // a credential.
                    let mut expected = case["value"].clone();
                    let key = expected.as_object_mut().unwrap().remove("digest");
                    assert_eq!(
                        incident.key,
                        key.as_ref().unwrap().as_str().unwrap(),
                        "{case}"
                    );
                    expected["key"] = key.unwrap();
                    assert_eq!(serde_json::to_value(incident).unwrap(), expected);
                }
                other => panic!("{case}: {other:?}"),
            }
        }
    }

    /// Every Rust source under `directory`, concatenated.
    fn sources(directory: &Path, text: &mut String) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, text);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                text.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }

    /// From `test_rules_and_exported_metrics_have_a_checked_contract`: no
    /// rule can disappear from the recorder or name a metric nothing exports.
    /// A server metric is spelled in the server's sources; an Ops gauge is a
    /// sample of the recorded collector and importer outputs.
    #[test]
    fn rules_name_only_recorded_rules_and_exported_metrics() {
        let ops = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rules = ops.join("ansible/files/monitoring-alerts.yml");
        let rules = std::fs::read_to_string(rules).unwrap();
        let alert = |line: &'static str| line.trim_start().strip_prefix("- alert: ");
        let rules: &'static str = rules.leak();
        let alerts: Vec<&str> = rules.lines().filter_map(alert).collect();
        assert_eq!(alerts.len(), 34);
        for alert in alerts {
            assert!(RULES.contains(&alert), "{alert}");
        }
        let mut server = String::new();
        sources(&ops.parent().unwrap().join("src"), &mut server);
        let recorded = [
            include_str!("recorded_collect.json"),
            include_str!("recorded_external.json"),
        ];
        let mut metrics = std::collections::BTreeSet::new();
        for (at, _) in rules.match_indices("simplestchat_") {
            let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
            if at > 0 && word(rules.as_bytes()[at - 1]) {
                continue;
            }
            let name = |character: &char| character.is_ascii_lowercase() || *character == '_';
            metrics.insert(rules[at..].chars().take_while(name).collect::<String>());
        }
        assert!(metrics.len() > 35, "{}", metrics.len());
        for metric in metrics {
            if metric.starts_with("simplestchat_ops_") {
                let samples = [format!("{metric} "), format!("{metric}{{")];
                let sampled = |text: &&str| samples.iter().any(|sample| text.contains(sample));
                assert!(recorded.iter().any(sampled), "{metric}");
                continue;
            }
            let quality = metric.strip_prefix("simplestchat_quality_");
            let quality = quality.map(|name| format!("\"{name}\""));
            let exported =
                server.contains(&metric) || quality.is_some_and(|name| server.contains(&name));
            assert!(exported, "{metric}");
        }
    }
}
