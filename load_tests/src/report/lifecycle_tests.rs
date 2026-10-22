use super::{LINE_BYTES, MAX_LINES, read, timeline};
use serde_json::{Value, json};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::PathBuf;

const PARTICIPANT: &str = "00000000-0000-4000-8000-000000000001";
const TRANSPORT: &str = "00000000-0000-4000-8000-000000000002";
const GENERATION: &str = "00000000-0000-4000-8000-000000000003";
const WORKER: &str = "00000000-0000-4000-8000-000000000004";
const BASE: i64 = 1_789_128_000_000;

fn at(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(BASE + ms)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn marker(ms: i64, event: &str, fields: &str) -> String {
    format!(
        "{} DEBUG simplestchat::lifecycle: lifecycle event=\"{event}\" participant_id={PARTICIPANT}{}",
        at(ms),
        if fields.is_empty() {
            String::new()
        } else {
            format!(" {fields}")
        }
    )
}

fn transport(ms: i64, event: &str, state: Option<&str>) -> String {
    marker(
        ms,
        event,
        &format!(
            "transport_id={TRANSPORT} transport_type=\"recv\"{}",
            state.map_or_else(String::new, |state| format!(" state={state}"))
        ),
    )
}

fn cleanup(ms: i64, event: &str) -> String {
    marker(ms, event, &format!("generation={GENERATION}"))
}

fn clamp(ms: i64) -> String {
    format!(
        "{} ERROR mediasoup::worker: [id:{WORKER}] webrtc::GoogCcNetworkController::ClampConstraints() | start bitrate smaller than min bitrate [starting_rate_:30000, min_data_rate_:100000]",
        at(ms)
    )
}

fn log(departure: &str) -> Vec<String> {
    let mut rows = vec![
        clamp(600),
        transport(1500, "transport_created", None),
        transport(2000, "transport_ice", Some("Connected")),
        transport(2500, "transport_dtls", Some("Connected")),
        clamp(5000),
        transport(12000, "transport_ice", Some("Disconnected")),
        clamp(12100),
        marker(
            12200,
            if departure == "abrupt" {
                "grace_started"
            } else {
                "explicit_leave_started"
            },
            "",
        ),
    ];
    if departure == "abrupt" {
        rows.push(marker(13000, "grace_cleanup_started", ""));
    }
    rows.extend([
        cleanup(13100, "media_cleanup_started"),
        transport(13200, "transport_closed", None),
        clamp(13300),
        cleanup(13400, "media_cleanup_finished"),
    ]);
    if departure == "explicit-leave" {
        rows.push(marker(13500, "explicit_leave_finished", ""));
    }
    rows.extend([clamp(15500), clamp(16500)]);
    rows
}

fn timeline(departure: &str) -> Value {
    let events: Vec<_> = [
        ("run_started", 0),
        ("server_ready", 500),
        ("generator_started", 1000),
        ("generator_exited", 14000),
        ("cleanup_observed", 15000),
        ("server_stop_requested", 16000),
        ("server_exited", 17000),
    ]
    .into_iter()
    .map(|(event, elapsed)| json!({"event":event,"at":at(elapsed),"elapsedMs":elapsed}))
    .collect();
    json!({"schemaVersion":1,"departure":departure,"events":events,"workload":{
        "startedAt":at(1000).replace('Z', "+00:00"),"finishedAt":at(13900).replace('Z', "+00:00"),"rampUpSecs":1,"warmupSecs":2,"durationSecs":8}})
}

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(bytes: impl AsRef<[u8]>) -> Self {
        let directory =
            std::env::temp_dir().join(format!("simplestchat lifecycle {}", uuid::Uuid::new_v4()));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let path = directory.join("PRIVATE server.log");
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
    fn report(&self, timeline: &Value, expected: f64) -> Value {
        serde_json::to_value(read(&self.path, &timeline.to_string(), Some(expected))).unwrap()
    }
    fn replace(&self, lines: &[String]) {
        fs::write(&self.path, lines.join("\n") + "\n").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
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
fn retains_anonymous_phase_cleanup_and_prior_clamp_evidence_for_both_departures() {
    for departure in ["abrupt", "explicit-leave"] {
        let rows: Vec<_> = log(departure)
            .into_iter()
            .map(|line| {
                line.replace(
                    " DEBUG ",
                    " \u{1b}[34mDEBUG\u{1b}[0m dispatch{operation=consume}: ",
                )
            })
            .collect();
        let fixture = Fixture::new(rows.join("\n") + "\n");
        let report = fixture.report(&timeline(departure), 1.0);
        assert_eq!(
            report["coverage"]["complete"], true,
            "{}",
            report["coverage"]
        );
        assert_eq!(report["coverage"]["scope"], "expected_instrumented_markers");
        assert_eq!(
            report["clamps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["phase"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "setup",
                "measurement",
                "departure",
                "departure",
                "post_cleanup",
                "shutdown"
            ]
        );
        assert_eq!(
            report["timeline"]["nominalMeasurement"],
            json!({"startedAt":at(4000),"finishedAt":at(12000)})
        );
        assert_eq!(
            report["clamps"][2]["previouslyObserved"]["openTransportsIceDisconnected"],
            1
        );
        assert_eq!(
            report["clamps"][3]["previouslyObserved"]["transportsOpen"],
            0
        );
        assert_eq!(report["totals"]["participantsWithCleanupFinished"], 1);
        let text = report.to_string();
        for private in [
            PARTICIPANT,
            TRANSPORT,
            GENERATION,
            WORKER,
            "dispatch{",
            "PRIVATE",
            "\u{1b}",
        ] {
            assert!(!text.contains(private));
        }
    }
}

#[test]
fn missing_readiness_departure_and_cleanup_markers_cannot_establish_coverage() {
    let fixture = Fixture::new("");
    for event in [
        "transport_created",
        "transport_ice",
        "transport_dtls",
        "transport_closed",
        "media_cleanup_started",
        "media_cleanup_finished",
        "grace_started",
        "grace_cleanup_started",
    ] {
        let rows: Vec<_> = log("abrupt")
            .into_iter()
            .filter(|line| !line.contains(&format!("event=\"{event}")))
            .collect();
        fixture.replace(&rows);
        assert_eq!(
            fixture.report(&timeline("abrupt"), 1.0)["coverage"]["complete"],
            false,
            "missing {event}"
        );
    }
    fixture.replace(&log("abrupt"));
    assert!(
        issues(&fixture.report(&timeline("abrupt"), 2.0))
            .contains(&"expected_participant_cleanup_missing")
    );
    let rows: Vec<_> = log("explicit-leave")
        .into_iter()
        .filter(|line| !line.contains("explicit_leave_finished"))
        .collect();
    fixture.replace(&rows);
    assert!(
        issues(&fixture.report(&timeline("explicit-leave"), 1.0))
            .contains(&"explicit_leave_marker_missing")
    );
}

#[test]
fn rejects_unknown_fields_states_controls_and_conflicting_identities_without_echoing() {
    let mut rows = log("abrupt");
    rows.splice(
        4..4,
        [
            transport(2600, "transport_ice", Some("Closed")),
            format!(
                "{} credential=\"PRIVATE payload\"",
                marker(2700, "grace_started", "")
            ),
            marker(2800, "unrecognized_PRIVATE_event", ""),
            format!("{}\u{1f}PRIVATE payload", marker(2900, "grace_started", "")),
            transport(3000, "transport_created", None).replace(PARTICIPANT, WORKER),
        ],
    );
    let fixture = Fixture::new(rows.join("\n") + "\n");
    let report = fixture.report(&timeline("abrupt"), 1.0);
    for issue in [
        "invalid_lifecycle_record",
        "invalid_log_control",
        "transport_identity_conflict",
    ] {
        assert!(issues(&report).contains(&issue));
    }
    assert!(!report.to_string().contains("PRIVATE"));
    assert!(!report.to_string().contains(WORKER));
}

#[test]
fn preserves_exact_nanosecond_and_250_millisecond_tolerances() {
    let fixture = Fixture::new("");
    for (millis, expected) in [(4750, false), (4749, true)] {
        let mut rows = log("abrupt");
        rows.insert(5, clamp(millis));
        fixture.replace(&rows);
        assert_eq!(
            issues(&fixture.report(&timeline("abrupt"), 1.0)).contains(&"log_clock_regression"),
            expected
        );
    }
    let mut rows = log("abrupt");
    rows.insert(5, clamp(4750).replace(".750Z", ".749999999Z"));
    fixture.replace(&rows);
    assert!(issues(&fixture.report(&timeline("abrupt"), 1.0)).contains(&"log_clock_regression"));
    fixture.replace(&log("abrupt"));
    for (shift, expected) in [(250.0, false), (250.001, true)] {
        let mut input = timeline("abrupt");
        input["events"][3]["elapsedMs"] = json!(14000.0 + shift);
        assert_eq!(
            issues(&fixture.report(&input, 1.0)).contains(&"timeline_clock_shift"),
            expected
        );
    }
    for at in [
        "2026-02-30T12:00:00Z",
        "2026-09-11T24:00:00Z",
        "2026-09-11T12:00:60Z",
        "2026-09-11T12:00:00+01:00",
    ] {
        assert!(timeline::instant(at).is_none());
    }
    for at in [
        "0000-01-01T00:00:00.000000001Z",
        "9999-12-31T23:59:59.999999999+00:00",
    ] {
        assert!(timeline::instant(at).is_some());
    }
}

#[test]
fn bounded_lines_and_records_keep_the_valid_prefix() {
    let prefix = log("abrupt").join("\n") + "\n";
    let fixture = Fixture::new(format!("{prefix}{}", "x".repeat(LINE_BYTES + 1)));
    let report = fixture.report(&timeline("abrupt"), 1.0);
    assert!(issues(&report).contains(&"line_length_limit"));
    assert!(!report["lifecycleEvents"].as_array().unwrap().is_empty());
    fs::write(
        &fixture.path,
        format!("{prefix}{}", "ordinary line\n".repeat(MAX_LINES + 1)),
    )
    .unwrap();
    assert!(issues(&fixture.report(&timeline("abrupt"), 1.0)).contains(&"line_count_limit"));
    fs::write(
        &fixture.path,
        format!("{prefix}{}", (clamp(16600) + "\n").repeat(10001)),
    )
    .unwrap();
    let report = fixture.report(&timeline("abrupt"), 1.0);
    assert!(issues(&report).contains(&"record_limit"));
    assert_eq!(report["relevantRecords"], 10000);
}

#[test]
fn private_nofollow_and_fifo_guards_never_read_unsafe_inputs() {
    let fixture = Fixture::new(log("abrupt").join("\n") + "\n");
    let link = fixture.directory.join("linked");
    symlink(&fixture.path, &link).unwrap();
    let fifo = fixture.directory.join("fifo");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    for path in [&link, &fifo, &fixture.directory] {
        let report =
            serde_json::to_value(read(path, &timeline("abrupt").to_string(), Some(1.0))).unwrap();
        assert!(issues(&report).contains(&"unsafe_file"));
    }
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(issues(&fixture.report(&timeline("abrupt"), 1.0)).contains(&"unsafe_file"));
}
