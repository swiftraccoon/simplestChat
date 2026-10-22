use super::{arguments, diagnostics};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::PathBuf;

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new(bytes: impl AsRef<[u8]>) -> Self {
        let directory =
            std::env::temp_dir().join(format!("simplestchat report {}", uuid::Uuid::new_v4()));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let path = directory.join("private records.jsonl");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(bytes.as_ref())
            .unwrap();
        Self { directory, path }
    }

    fn report(&self, max_records: usize) -> Value {
        serde_json::to_value(diagnostics::read(&self.path, max_records).unwrap()).unwrap()
    }

    fn replace(&self, bytes: impl AsRef<[u8]>) {
        fs::write(&self.path, bytes).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn record() -> Value {
    json!({"schemaVersion":1,"kind":"operation","operationId":1,"connectionId":1,
        "operation":"join_room","stage":null,"outcome":"ok","startedUs":10,"elapsedUs":1000})
}

fn summary(count: usize) -> Value {
    json!({"schemaVersion":1,"kind":"summary","accepted":count,"written":count,
        "dropped":0,"expired":0,"unfinished":0,"writeFailed":false})
}

fn lines(records: &[Value]) -> String {
    records.iter().map(|value| format!("{value}\n")).collect()
}

fn issues(report: &Value) -> Vec<&str> {
    report["coverage"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn aggregates_raw_nearest_rank_samples_and_distinct_outcomes_without_ids() {
    let mut records = Vec::new();
    for index in 1..=100 {
        let mut row = record();
        row["operationId"] = json!(index);
        row["elapsedUs"] = json!(index * 1000);
        records.push(row);
    }
    let mut stage = record();
    stage["kind"] = json!("stage");
    stage["stage"] = json!("room_lock_wait");
    stage["elapsedUs"] = json!(2500);
    records.push(stage);
    for outcome in ["error", "completed", "cancelled", "timeout", "rejected"] {
        let mut row = record();
        row["outcome"] = json!(outcome);
        records.push(row);
    }
    records.push(summary(records.len()));
    let fixture = Fixture::new(lines(&records));
    let report = fixture.report(100_000);
    assert_eq!(
        report["coverage"],
        json!({"available":true,"complete":true,"scope":"all_emitted_records","issues":[]})
    );
    let ok = report["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["outcome"] == "ok")
        .unwrap();
    assert_eq!(
        ok,
        &json!({"operation":"join_room","outcome":"ok","count":100,"p50Ms":50.0,"p95Ms":95.0,"p99Ms":99.0})
    );
    assert_eq!(
        report["stages"][0],
        json!({"operation":"join_room","stage":"room_lock_wait","outcome":"ok","count":1,"p50Ms":2.5,"p95Ms":2.5,"p99Ms":2.5})
    );
    assert_eq!(
        report["outcomeCounts"],
        json!({"ok":101,"error":1,"completed":1,"cancelled":1,"timeout":1,"rejected":1})
    );
    let text = report.to_string();
    for secret in [
        "connectionId",
        "operationId",
        "startedUs",
        "private records",
        fixture.directory.to_str().unwrap(),
    ] {
        assert!(!text.contains(secret));
    }
}

#[test]
fn accepts_server_close_consumer_and_retains_historical_stage_vocabulary() {
    let mut records = Vec::new();
    let mut operation = record();
    operation["operation"] = json!("close_consumer");
    records.push(operation.clone());
    for stage in [
        "room_password_dispatch",
        "room_password_work",
        "outbound_queue_wait",
    ] {
        let mut row = operation.clone();
        row["kind"] = json!("stage");
        row["stage"] = json!(stage);
        records.push(row);
    }
    records.push(summary(records.len()));
    let fixture = Fixture::new(lines(&records));
    let report = fixture.report(100_000);
    assert_eq!(report["coverage"]["complete"], true);
    assert_eq!(report["operations"][0]["operation"], "close_consumer");
    assert_eq!(report["stages"].as_array().unwrap().len(), 3);
}

#[test]
fn retains_valid_prefixes_and_orders_terminal_failure_issues() {
    let fixture = Fixture::new(lines(&[record()]));
    let prefix = fixture.report(100_000);
    assert_eq!(issues(&prefix), ["missing_summary"]);
    assert_eq!(prefix["records"], 1);
    assert_eq!(prefix["coverage"]["scope"], "recorded_subset");
    fixture.replace(format!("{}{}", lines(&[record()]), summary(1)));
    assert_eq!(
        issues(&fixture.report(100_000)),
        ["unterminated_line", "missing_summary"]
    );
    fixture.replace(lines(&[record(), summary(1), record()]));
    assert_eq!(issues(&fixture.report(100_000)), ["records_after_summary"]);
    let mut end = summary(2);
    end["written"] = json!(0);
    end["dropped"] = json!(1);
    end["expired"] = json!(1);
    end["unfinished"] = json!(1);
    end["writeFailed"] = json!(true);
    fixture.replace(lines(&[record(), end]));
    assert_eq!(
        issues(&fixture.report(100_000)),
        [
            "count_mismatch",
            "write_failed",
            "records_dropped",
            "records_expired",
            "unfinished_timers"
        ]
    );
    fixture.replace(lines(&[summary(0)]));
    assert_eq!(issues(&fixture.report(100_000)), ["no_records"]);
}

#[test]
fn enforces_line_file_record_utf8_and_final_newline_bounds() {
    let fixture = Fixture::new(format!("{}\n", "x".repeat(1025)));
    assert_eq!(
        issues(&fixture.report(100_000)),
        ["line_limit", "missing_summary", "no_records"]
    );
    fixture.replace(lines(&[record(), record(), record(), summary(3)]));
    let report = fixture.report(2);
    assert_eq!(report["records"], 2);
    assert_eq!(issues(&report), ["record_limit", "missing_summary"]);
    fixture.replace("x".repeat(2051));
    let report = fixture.report(1);
    assert_eq!(issues(&report), ["file_limit", "missing_summary"]);
    assert_eq!(report["coverage"]["available"], false);
    fixture.replace([0xff, b'\n']);
    assert_eq!(
        issues(&fixture.report(100_000)),
        ["invalid_json", "missing_summary", "no_records"]
    );
    for bound in [0, 100_001, usize::MAX] {
        assert!(diagnostics::read(&fixture.path, bound).is_err());
    }
}

#[test]
fn rejects_payloads_unknown_vocabulary_and_invalid_record_numbers_without_echoing_them() {
    let fixture = Fixture::new("");
    let sentinel = "PRIVATE room user credentials sdp 192.0.2.77";
    for (key, value) in [
        ("operation", json!(sentinel)),
        ("outcome", json!(sentinel)),
        ("stage", json!("room_lock_wait")),
        ("operationId", json!(0)),
        ("connectionId", json!(-1)),
        ("elapsedUs", json!(9_007_199_254_740_992_u64)),
        ("startedUs", json!(0.5)),
        ("schemaVersion", json!(2)),
        ("elapsedUs", Value::Null),
        ("privatePayload", json!(sentinel)),
    ] {
        let mut row = record();
        row[key] = value;
        fixture.replace(lines(&[row, summary(1)]));
        let report = fixture.report(100_000);
        assert!(issues(&report).contains(&"invalid_record"));
        assert_eq!(report["records"], 0);
        assert!(!report.to_string().contains(sentinel));
    }
    for row in [Value::Null, json!([]), json!({"secret":sentinel})] {
        fixture.replace(lines(&[row]));
        assert!(issues(&fixture.report(100_000)).contains(&"invalid_record"));
    }
}

#[test]
fn validates_exact_terminal_summary_schema_and_safe_counter_bounds() {
    let fixture = Fixture::new("");
    for (key, value) in [
        ("accepted", json!(100001)),
        ("written", json!(2)),
        ("dropped", json!(-1)),
        ("expired", json!(0.1)),
        ("unfinished", json!(9_007_199_254_740_992_u64)),
        ("writeFailed", json!("false")),
        ("secret", json!("PRIVATE")),
        ("schemaVersion", json!(2)),
    ] {
        let mut end = summary(1);
        end[key] = value;
        fixture.replace(lines(&[record(), end]));
        let report = fixture.report(100_000);
        assert!(issues(&report).contains(&"invalid_summary"));
        assert!(!report.to_string().contains("PRIVATE"));
    }
    fixture.replace("{\"kind\":\"summary\",\"\\ud800\":1}\n");
    assert_eq!(
        issues(&fixture.report(100_000)),
        ["invalid_summary", "missing_summary", "no_records"]
    );
}

#[test]
fn matches_javascript_number_conversion_bom_and_duplicate_key_semantics() {
    let row = record().to_string();
    for token in [
        "1.0",
        "1e0",
        "1.0000000000000001",
        "-0",
        "9007199254740991",
        "9007199254740991.49",
    ] {
        let source = row.replace("\"elapsedUs\":1000", &format!("\"elapsedUs\":{token}"));
        let fixture = Fixture::new(format!("\u{feff}{source}\n\u{feff}{}\n", summary(1)));
        assert!(
            fixture.report(100_000)["coverage"]["complete"]
                .as_bool()
                .unwrap(),
            "{token}"
        );
    }
    for token in ["1e400", "9007199254740991.5", "-1", "0.5"] {
        let source = row.replace("\"elapsedUs\":1000", &format!("\"elapsedUs\":{token}"));
        let fixture = Fixture::new(format!("{source}\n"));
        assert_eq!(
            issues(&fixture.report(100_000)),
            ["invalid_record", "missing_summary", "no_records"],
            "{token}"
        );
    }
    let source = row.replace(
        "\"operation\":\"join_room\"",
        "\"operation\":\"PRIVATE\",\"operation\":\"join_room\"",
    );
    let fixture = Fixture::new(format!("{source}\n{}\n", summary(1)));
    assert!(
        fixture.report(100_000)["coverage"]["complete"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn refuses_links_nonprivate_files_directories_and_fifos_without_blocking() {
    let fixture = Fixture::new(lines(&[record(), summary(1)]));
    let link = fixture.directory.join("linked");
    symlink(&fixture.path, &link).unwrap();
    let fifo = fixture.directory.join("fifo");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    for path in [&link, &fixture.directory, &fifo] {
        let report = serde_json::to_value(diagnostics::read(path, 100_000).unwrap()).unwrap();
        assert_eq!(issues(&report), ["unsafe_file", "missing_summary"]);
    }
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        issues(&fixture.report(100_000)),
        ["unsafe_file", "missing_summary"]
    );
    let missing = fixture.directory.join("PRIVATE absent path");
    let report = serde_json::to_value(diagnostics::read(&missing, 100_000).unwrap()).unwrap();
    assert_eq!(issues(&report), ["file_unavailable", "missing_summary"]);
    assert!(!report.to_string().contains("PRIVATE"));
}

#[test]
fn command_arguments_keep_paths_literal_and_reject_invalid_bounds() {
    let args: Vec<OsString> = ["diagnostics", "--private path", "--max-records", "2"]
        .map(OsString::from)
        .into();
    let (path, bound) = arguments(&args).unwrap();
    assert_eq!(path.to_str(), Some("--private path"));
    assert_eq!(bound, 2);
    for values in [
        vec![],
        vec!["other", "path"],
        vec!["diagnostics"],
        vec!["diagnostics", "path", "--other", "1"],
        vec!["diagnostics", "path", "--max-records", "0"],
        vec!["diagnostics", "path", "--max-records", "1.5"],
    ] {
        assert!(arguments(&values.into_iter().map(OsString::from).collect::<Vec<_>>()).is_none());
    }
}
