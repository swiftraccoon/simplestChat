//! The reviewed monitoring statements against real PostgreSQL: replay,
//! resolve and reopen, privacy and retention. These run only inside the
//! disposable cluster that `simplestchat-ci database-check monitoring` owns
//! and selects; anywhere else, a broad `--include-ignored` run included, they
//! return at once.

use super::alerts;
use crate::host::command::Request;
use crate::host::command::tests::Scripted;
use crate::host::failure::Failure;
use crate::host::state::tests::Scratch;
use crate::host::tests::paths;
use crate::host::{Host, System};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SELECTOR: &str = "MONITORING_DATABASE_CHECK";

/// The owning check's database, or nothing anywhere else.
fn database() -> Option<String> {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
    if !(set(SELECTOR) && set("DISPOSABLE_TEST_DATABASE")) {
        return None;
    }
    Some(std::env::var("TEST_DATABASE_URL").expect("the owning check's database"))
}

/// The directory of the statements a host installs.
fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files")
}

fn statement(name: &str) -> String {
    std::fs::read_to_string(files().join(name)).unwrap()
}

/// Run `psql` with `arguments`, `source` on standard input.
fn client(arguments: &[&str], source: &[u8]) -> Vec<u8> {
    let mut command = Command::new("psql");
    command.args(arguments).stdin(Stdio::piped());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Send fixed SQL, and one JSON value as the client variable `payload` when
/// given, and return the trimmed unaligned answer.
fn sql(url: &str, source: &str, payload: Option<&Value>) -> String {
    let payload = payload.map(|value| format!("payload={value}"));
    let mut arguments = vec!["-X", "-q", "-t", "-A", url, "--set", "ON_ERROR_STOP=1"];
    if let Some(payload) = &payload {
        arguments.extend(["--set", payload.as_str()]);
    }
    let answer = client(&arguments, source.as_bytes());
    String::from_utf8(answer).unwrap().trim().to_owned()
}

fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// From `test_ambiguous_committed_response_retains_database_outage_after_recovery`:
/// a statement that commits but whose answer never arrives must still leave,
/// and then resolve, the database's own incident.
#[test]
#[ignore = "runs only in the cluster `database-check monitoring` owns"]
fn an_ambiguous_committed_response_retains_the_outage_after_recovery() {
    let Some(url) = database() else { return };
    sql(&url, &statement("monitoring-schema.sql"), None);
    sql(
        &url,
        "TRUNCATE operations.alerts, operations.alert_cursor;",
        None,
    );
    let attempts = std::cell::Cell::new(0);
    let commands = Scripted::new(|request: &Request<'_>| {
        if request.argv[0] == "/usr/bin/curl" {
            // Prometheus answers: nothing is firing.
            return Ok(br#"{"status":"success","data":{"alerts":[]}}"#.to_vec());
        }
        // The recorder's own statement and payload, committed for real.
        let payload = request
            .argv
            .iter()
            .find(|word| word.starts_with("payload="));
        let arguments = ["-X", "-q", url.as_str(), "--set", payload.unwrap().as_str()];
        let answer = client(&arguments, request.input.unwrap());
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            return Err(Failure::Timeout);
        }
        Ok(answer)
    });
    let scratch = Scratch::new();
    let mut place = paths(scratch.path());
    std::fs::create_dir_all(&place.state).unwrap();
    place.sql = files();
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: place,
        owner: 0,
    };
    assert!(!alerts::record(&host, None).unwrap().0);
    assert!(alerts::record(&host, None).unwrap().0);
    let history = "SELECT rule, count(*), count(resolved_at) FROM operations.alerts GROUP BY rule;";
    assert_eq!(sql(&url, history, None), "DatabaseUnavailable|1|1");
    sql(
        &url,
        "TRUNCATE operations.alerts, operations.alert_cursor;",
        None,
    );
}

/// From `test_external_history_partial_recovery_reruns_privacy_and_retention`:
/// the real statements keep skipped-check gaps and cannot turn incomplete
/// polling into a recovery.
#[test]
#[ignore = "runs only in the cluster `database-check monitoring` owns"]
fn external_history_keeps_gaps_privacy_and_retention_through_partial_recovery() {
    let Some(url) = database() else { return };
    sql(&url, &statement("monitoring-schema.sql"), None);
    let roles = "DO $$ BEGIN
        IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN
            CREATE ROLE postgres NOLOGIN;
        END IF;
        IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
            CREATE ROLE simplestchat_app NOLOGIN;
            CREATE ROLE simplestchat_migrate NOLOGIN;
        END IF;
        END $$;
        ALTER SCHEMA operations OWNER TO postgres;
        ALTER TABLE operations.schema_version OWNER TO postgres;";
    sql(&url, roles, None);
    let schema = statement("monitoring-external-schema.sql");
    sql(&url, &format!("SET ROLE postgres;\n{schema}"), None);
    let record = statement("monitoring-external.sql");
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let after = |seconds: i64| stamp(now + TimeDelta::seconds(seconds));
    let attempt = |identity: u64, number: u64, result: &str, seconds: i64| {
        json!({
            "runId": identity,
            "attempt": number,
            "sourceRevision": "a".repeat(40),
            "startedAt": after(seconds - 30),
            "completedAt": after(seconds),
            "result": result,
            "conclusion": if result == "failure" { "failure" } else { "success" },
            "checks": {"readiness": if result == "success" { "success" } else { "missing" }},
        })
    };
    let snapshot = |seconds: i64, entries: Vec<Value>, complete: bool| {
        json!({
            "observedAt": after(seconds),
            "windowStart": stamp(now - TimeDelta::days(1)),
            "revision": "b".repeat(40),
            "workflows": [{
                "workflow": "availability",
                "apiOk": true,
                "complete": complete,
                "pending": u8::from(!complete),
                "truncated": false,
                "runs": entries,
            }],
        })
    };
    let failure = attempt(1, 1, "failure", -180);
    let skipped = attempt(2, 1, "incomplete", -120);
    let first = snapshot(0, vec![failure, skipped], true);
    // Check the importer's actual plan, also where PostgreSQL has no LLVM.
    // Unanalyzed temporary tables were once estimated at 1,360 snapshots,
    // multiplying the two JSON expansions into 13.6 million planned rows.
    let insert = "INSERT INTO operations.external_runs";
    let prefix = record
        .split("INSERT INTO operations.external_status")
        .next()
        .unwrap();
    assert_eq!(prefix.matches(insert).count(), 1);
    let explained = prefix.replacen(insert, &format!("EXPLAIN (FORMAT JSON) {insert}"), 1);
    let plans = sql(&url, &format!("{explained}\nROLLBACK;"), Some(&first));
    let plans: Value = serde_json::from_str(&plans).unwrap();
    let mut nodes = vec![&plans[0]["Plan"]];
    let mut scans = 0;
    while let Some(node) = nodes.pop() {
        if node["Relation Name"] == "incoming_external" {
            assert_eq!(node["Plan Rows"], 1);
            scans += 1;
        }
        nodes.extend(node["Plans"].as_array().into_iter().flatten());
    }
    assert!(scans > 0);
    sql(&url, &record, Some(&first));
    sql(&url, &record, Some(&first));
    let runs = "SELECT count(*) FROM operations.external_runs;";
    assert_eq!(sql(&url, runs, None), "2");
    let status = |columns: &str| {
        let source = format!(
            "SELECT {columns} FROM operations.external_status WHERE workflow='availability';"
        );
        sql(&url, &source, None)
    };
    assert_eq!(status("failure_since IS NOT NULL"), "t");
    let report = sql(&url, &statement("monitoring-report.sql"), None);
    let report: Value = serde_json::from_str(&report).unwrap();
    let coverage = &report["externalCoverage"];
    assert_eq!(coverage[0]["coverage"], "incomplete");
    assert_eq!(coverage[1]["coverage"], "missing");
    assert!(coverage[0]["source_revision"].is_string());
    assert_ne!(
        coverage[0]["source_revision"],
        coverage[0]["deployed_revision_at_import"]
    );
    // A successful rerun is distinct evidence, but partial polling cannot
    // clear an incident, and a stale complete replay cannot override it.
    let rerun = snapshot(60, vec![attempt(1, 2, "success", -60)], false);
    sql(&url, &record, Some(&rerun));
    sql(&url, &record, Some(&first));
    assert_eq!(status("failure_since IS NOT NULL, complete"), "t|f");
    assert_eq!(sql(&url, runs, None), "3");
    sql(&url, &record, Some(&snapshot(120, Vec::new(), true)));
    assert_eq!(status("failure_since IS NULL, complete"), "t|t");
    let private = "SELECT bool_and(NOT has_schema_privilege(
        r.rolname,'operations','USAGE,CREATE') AND NOT has_table_privilege(
        r.rolname,'operations.external_runs','SELECT,INSERT,UPDATE,DELETE,TRUNCATE'))
        FROM pg_roles r WHERE r.rolname IN ('simplestchat_app','simplestchat_migrate');";
    assert_eq!(sql(&url, private, None), "t");
    let aged = "INSERT INTO operations.external_runs
        SELECT 'availability',100+n,1,repeat('a',40),repeat('b',40),now(),now(),
            'success','success','{}',now() FROM generate_series(1,10001) n;
        UPDATE operations.external_runs SET started_at=now()-interval '41 days',
            completed_at=now()-interval '40 days' WHERE run_id=1;";
    sql(&url, aged, None);
    sql(&url, &record, Some(&snapshot(180, Vec::new(), true)));
    assert_eq!(sql(&url, runs, None), "10000");
    let oldest = "SELECT count(*) FROM operations.external_runs WHERE run_id=1;";
    assert_eq!(sql(&url, oldest, None), "0");
    // A gap longer than the bounded backfill stays visible after later
    // healthy polling.
    let mut gap = snapshot(200, Vec::new(), true);
    gap["windowStart"] = json!(after(190));
    sql(&url, &record, Some(&gap));
    assert_eq!(status("history_gap_since IS NOT NULL"), "t");
}

/// From `test_lifecycle_replay_privileges_and_retention`: committed retries
/// are idempotent and a resolved incident keeps its own history.
#[test]
#[ignore = "runs only in the cluster `database-check monitoring` owns"]
fn incident_lifecycle_replay_privileges_and_retention() {
    let Some(url) = database() else { return };
    sql(&url, &statement("monitoring-schema.sql"), None);
    let record = statement("monitoring-alerts.sql");
    let alert = alerts::incident("DatabaseUnavailable", "critical", &BTreeMap::new()).unwrap();
    let alert = serde_json::to_value(alert).unwrap();
    // Keep lifecycle events inside the statements' retention window: only the
    // explicit aging step below may expire an incident as the calendar moves.
    let clock = "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', \
                 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"');";
    let observed = DateTime::parse_from_rfc3339(&sql(&url, clock, None)).unwrap();
    let observed = observed.with_timezone(&Utc) - TimeDelta::hours(1);
    let value = |minutes: i64, complete: bool, alerts: &[&Value]| {
        json!({
            "observedAt": stamp(observed + TimeDelta::minutes(minutes)),
            "revision": "a".repeat(40),
            "complete": complete,
            "alerts": alerts,
        })
    };
    let open = "SELECT count(*) FROM operations.alerts WHERE resolved_at IS NULL;";
    sql(&url, &record, Some(&value(0, true, &[&alert])));
    sql(&url, &record, Some(&value(0, true, &[&alert])));
    let seen = "SELECT count(*),sum(observations) FROM operations.alerts;";
    assert_eq!(sql(&url, seen, None), "1|1");
    sql(&url, &record, Some(&value(1, false, &[])));
    assert_eq!(sql(&url, open, None), "1");
    sql(&url, &record, Some(&value(2, true, &[])));
    assert_eq!(sql(&url, open, None), "0");
    sql(&url, &record, Some(&value(3, true, &[&alert])));
    let resolved = "SELECT count(*),count(resolved_at) FROM operations.alerts;";
    assert_eq!(sql(&url, resolved, None), "2|1");
    // PUBLIC must not gain schema access when a new unprivileged role appears.
    sql(
        &url,
        "CREATE ROLE operations_fixture; SET ROLE operations_fixture;",
        None,
    );
    let usage = "SELECT has_schema_privilege('operations_fixture','operations','USAGE');";
    assert_eq!(sql(&url, usage, None), "f");
    let aged = "UPDATE operations.alerts SET first_seen=now()-interval '40 days',
        last_seen=now()-interval '40 days', resolved_at=now()-interval '40 days'
        WHERE resolved_at IS NOT NULL;";
    sql(&url, aged, None);
    sql(&url, &record, Some(&value(4, true, &[&alert])));
    assert_eq!(
        sql(&url, "SELECT count(*) FROM operations.alerts;", None),
        "1"
    );
}
