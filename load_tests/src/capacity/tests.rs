//! Pure policy and offline CLI parity with the retained Python capacity tool.

use super::*;
use serde_json::json;
use std::fs;
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

fn native(arguments_: &[&str]) -> Result<String> {
    execute(arguments(&arguments_.iter().map(OsString::from).collect::<Vec<_>>()).unwrap())
}

fn oracle(arguments: &[&str]) -> std::process::Output {
    Process::new("python3")
        .arg(source().join("build/capacity.py"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn original_reference_suggestions_respect_workers_memory_ports_and_explicit_quotas() {
    let ceilings = model::reference();
    let normal = model::suggest(
        model::Host {
            vcpus: 4,
            memory_mib: 12_288,
            port_mbps: 1000.0,
        },
        &ceilings,
        None,
        None,
    )
    .unwrap();
    assert_eq!(normal.deployment.workers(), 3);
    assert_eq!(normal.deployment.memory_mib, 9216);
    assert_eq!((normal.meetings, normal.meeting_limit), (255, "cpu"));
    assert_eq!((normal.viewers, normal.viewer_limit), (525, "cpu"));
    assert_eq!((normal.room, normal.connections), (25, 525));
    for setting in [
        "MEDIA_WORKERS=3",
        "SIMPLESTCHAT_CPUS=3",
        "SIMPLESTCHAT_MEMORY_LIMIT=9216m",
        "MAX_CONNECTIONS=525",
        "MAX_ROOMS=525",
        "MAX_PARTICIPANTS_PER_ROOM=525",
        "MAX_BROADCASTERS_PER_ROOM=30",
        "RTC_PORT_END=40002",
    ] {
        assert!(normal.settings().iter().any(|line| line == setting));
    }
    assert_eq!(
        normal
            .settings()
            .iter()
            .filter(|line| line.starts_with("MAX_PARTICIPANTS"))
            .count(),
        1
    );
    let slow = model::suggest(
        model::Host {
            vcpus: 16,
            memory_mib: 65_536,
            port_mbps: 100.0,
        },
        &ceilings,
        None,
        None,
    )
    .unwrap();
    assert_eq!(slow.deployment.workers(), 15);
    assert_eq!((slow.meetings, slow.meeting_limit), (33, "network"));
    assert_eq!((slow.viewers, slow.viewer_limit), (80, "network"));
    let pinned = model::suggest(
        model::Host {
            vcpus: 16,
            memory_mib: 65_536,
            port_mbps: 1000.0,
        },
        &ceilings,
        Some(2.0),
        Some(2048),
    )
    .unwrap();
    assert_eq!(
        (
            pinned.deployment.workers(),
            pinned.deployment.memory_mib,
            pinned.viewers
        ),
        (2, 2048, 350)
    );
    let small = model::suggest(
        model::Host {
            vcpus: 8,
            memory_mib: 2048,
            port_mbps: 1000.0,
        },
        &ceilings,
        None,
        None,
    )
    .unwrap();
    assert_eq!((small.viewers, small.viewer_limit), (400, "memory"));
    let large = model::suggest(
        model::Host {
            vcpus: 128,
            memory_mib: 262_144,
            port_mbps: 40_000.0,
        },
        &ceilings,
        None,
        None,
    )
    .unwrap();
    assert_eq!(large.deployment.workers(), 64);
    assert_eq!(large.viewers, 11200);
    assert!(
        large
            .settings()
            .contains(&"MAX_PARTICIPANTS_PER_ROOM=10000".into())
    );
}

#[test]
fn suggest_stdout_matches_python_for_original_examples_and_quota_edges() {
    for arguments in [
        vec!["suggest", "--vcpus", "8", "--memory-gib", "16"],
        vec![
            "suggest",
            "--vcpus",
            "4",
            "--memory-gib",
            "12",
            "--included-egress-tb",
            "5",
        ],
        vec!["suggest", "--vcpus", "4", "--memory-gib", "12", "--env"],
        vec![
            "suggest",
            "--vcpus",
            "16",
            "--memory-gib",
            "64",
            "--port-mbps",
            "100",
        ],
        vec![
            "suggest",
            "--vcpus",
            "1",
            "--memory-gib",
            "0.125",
            "--app-cpus",
            "0",
            "--app-memory-mib",
            "0",
        ],
        vec![
            "suggest",
            "--vcpus",
            "8",
            "--memory-gib",
            "16",
            "--app-cpus",
            "2.75",
            "--app-memory-mib",
            "2048",
        ],
    ] {
        let expected = oracle(&arguments);
        assert!(
            expected.status.success(),
            "{}",
            String::from_utf8_lossy(&expected.stderr)
        );
        assert_eq!(
            native(&arguments).unwrap().as_bytes(),
            expected.stdout,
            "{arguments:?}"
        );
    }
    assert!(native(&["suggest", "--vcpus", "0", "--memory-gib", "16"]).is_err());
}

#[test]
fn lexical_numeric_adapter_preserves_numbers_legacy_nonfinite_and_string_identity() {
    let value = json::parse(
        br#"{"a": 1.0000000000000002, "b": 1e999, "c":NaN,
        "d":-Infinity, "e":"@capacity:NaN", "f":"NaN", "g":123456789012345678901234567890}"#,
    )
    .unwrap();
    assert_eq!(value["a"].number("a").unwrap(), 1.0000000000000002);
    assert_eq!(value["b"].number("b").unwrap(), f64::INFINITY);
    assert!(value["c"].number("c").unwrap().is_nan());
    assert_eq!(value["d"].number("d").unwrap(), f64::NEG_INFINITY);
    assert_eq!(value["e"].string(), Some("@capacity:NaN"));
    assert_eq!(value["f"].string(), Some("NaN"));
    assert_eq!(value["g"].display(), "123456789012345678901234567890");
    for source in [
        "{1:2}",
        "{NaN:0}",
        "[01]",
        "[1.]",
        "[.1]",
        "[-]",
        "[1e]",
        "[+1]",
        "[1 2]",
        "//comment\n{}",
        "[InfinityX]",
        "{\"a\":1,}",
    ] {
        assert!(json::parse(source.as_bytes()).is_err(), "accepted {source}");
    }
    assert!(
        json::parse(b"{\"a\":true}").unwrap()["a"]
            .number("a")
            .is_err()
    );
}

#[test]
fn formatter_matches_reference_rounding_general_exponents_and_commas() {
    let mut command = Process::new("python3").args(["-c", "import json,sys; print(json.dumps([[format(x,'g'),repr(x),format(x,',.0f')] for x in json.load(sys.stdin)]))"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let values = [
        0.0, -0.0, 1.25, 999999.9, 1e6, 1e-4, 1e-5, 1e16, 1e15, 4648.9, 11111.1,
    ];
    command
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&values).unwrap().as_bytes())
        .unwrap();
    let output = command.wait_with_output().unwrap();
    let expected: Vec<Vec<String>> = serde_json::from_slice(&output.stdout).unwrap();
    for (value, expected) in values.into_iter().zip(expected) {
        assert_eq!(
            [
                format::general(value),
                format::repr(value),
                format::grouped(value)
            ],
            expected.as_slice()
        );
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-capacity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, value: &serde_json::Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, value.to_string()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn calibration(label: &str, price: Option<f64>) -> serde_json::Value {
    json!({"label":label, "host":{"logicalCpus":4,"cpuModel":"EPYC test machine"},
        "measurement":{"meetingSize":5,"browser":{"capture":"720p","speakers":1,
            "viewportWidth":1440,"pixelRatio":2.0,"layout":"classic"}},
        "ceilings":{"meetings":{"ceiling":85,"workers":1,"egressMbps":203.15,
            "memoryPeakBytes":178257920,"bound":"measured"},
            "large-meeting":{"ceiling":28,"workers":2,"egressMbps":84.0,
                "memoryPeakBytes":88080384,"bound":"measured"},
            "webinar":{"ceiling":175,"workers":1,"egressMbps":175.0,
                "memoryPeakBytes":234881024,"bound":"measured"}},
        "projection":{"meetingParticipants":255,"lowerBound":false,"egressMbpsPerParticipant":2.39},
        "cost":price.map(|price|json!({"dollarsPer1000ParticipantHours":price/255.0/730.0*1000.0,
            "prices":{"monthly":price}}))})
}

#[test]
fn calibration_legacy_defaults_camel_case_and_missing_workloads_match_reference() {
    let fixture = Fixture::new();
    let mut value = calibration("legacy", None);
    value["ceilings"]
        .as_object_mut()
        .unwrap()
        .remove("large-meeting");
    value["ceilings"]["webinar"] = json!({"ceiling":300,"egressMbps":300.0,"workers":2});
    let path = fixture.write("legacy.json", &value);
    let args = [
        "suggest",
        "--vcpus",
        "4",
        "--memory-gib",
        "12",
        "--calibration",
        path.to_str().unwrap(),
    ];
    let expected = oracle(&args);
    assert!(
        expected.status.success(),
        "{}",
        String::from_utf8_lossy(&expected.stderr)
    );
    let output = native(&args).unwrap();
    assert_eq!(output.as_bytes(), expected.stdout);
    assert!(output.contains("about 450 viewers"));
    assert!(output.contains("All-publishing rooms: 0 people"));
    // Python's writer emits Infinity for a zero-price report; unused cost fields
    // must not make a previously usable calibration unreadable.
    let source = value
        .to_string()
        .replace("\"cost\":null", "\"cost\":{\"unused\":Infinity}");
    fs::write(&path, source).unwrap();
    assert_eq!(native(&args).unwrap(), output);
    assert!(oracle(&args).status.success());
}

#[test]
fn nonreference_calibration_shapes_refuse_before_printing_any_settings() {
    let fixture = Fixture::new();
    for (field, change) in [
        ("meetingSize", json!(30)),
        ("audioOnly", json!(true)),
        ("speakers", json!(5)),
        ("chatIntervalMs", json!(30000)),
        ("layout", json!("modern")),
    ] {
        let mut report = calibration("changed", Some(10.0));
        if field == "meetingSize" {
            report["measurement"][field] = change;
        } else {
            report["measurement"]["browser"][field] = change;
        }
        let path = fixture.write("changed.json", &report);
        let args = [
            "suggest",
            "--vcpus",
            "6",
            "--memory-gib",
            "12",
            "--calibration",
            path.to_str().unwrap(),
            "--env",
        ];
        let expected = oracle(&args);
        assert!(!expected.status.success());
        assert!(expected.stdout.is_empty());
        let error = native(&args).unwrap_err().to_string();
        assert_eq!(format!("capacity: {error}\n").as_bytes(), expected.stderr);
    }
}

#[test]
fn comparison_rank_lower_bounds_absent_measurements_and_unicode_match_reference() {
    let fixture = Fixture::new();
    let mut dear = calibration("dear", Some(40.0));
    dear["host"]["cpuModel"] = json!("processor 🦊 é repeated long model text");
    dear["projection"]["lowerBound"] = json!(true);
    dear["ceilings"]["large-meeting"]["bound"] = json!("generator-limited");
    let cheap = calibration("cheap", Some(5.0));
    let mut unknown = calibration("unpriced", None);
    unknown["projection"]["lowerBound"] = json!(true);
    unknown["projection"]["meetingParticipants"] = json!(0);
    for workload in model::WORKLOADS {
        unknown["ceilings"][workload]["ceiling"] = serde_json::Value::Null;
    }
    let paths = [
        fixture.write("dear.json", &dear),
        fixture.write("cheap.json", &cheap),
        fixture.write("unknown.json", &unknown),
    ];
    let args = [
        "compare",
        paths[0].to_str().unwrap(),
        paths[1].to_str().unwrap(),
        paths[2].to_str().unwrap(),
    ];
    let expected = oracle(&args);
    assert!(expected.status.success());
    let output = native(&args).unwrap();
    assert_eq!(output.as_bytes(), expected.stdout);
    let rows = output.lines().collect::<Vec<_>>();
    assert!(rows[1].starts_with("cheap"));
    assert!(rows[2].starts_with("dear"));
    assert!(rows[3].starts_with("unpriced"));
    assert!(!output.contains("None"));
    assert!(!output.contains("≥0"));
}

#[test]
fn comparison_rejects_mixed_profiles_before_emitting_any_rankings() {
    let fixture = Fixture::new();
    let normal = fixture.write("video.json", &calibration("video", Some(10.0)));
    let mut changed = calibration("audio", Some(10.0));
    changed["measurement"]["browser"]["audioOnly"] = json!(true);
    let changed = fixture.write("audio.json", &changed);
    let args = [
        "compare",
        normal.to_str().unwrap(),
        changed.to_str().unwrap(),
    ];
    let expected = oracle(&args);
    assert!(!expected.status.success());
    assert!(expected.stdout.is_empty());
    assert_eq!(
        format!("capacity: {}\n", native(&args).unwrap_err()).as_bytes(),
        expected.stderr
    );
}

#[test]
fn legacy_nonfinite_profile_values_keep_reference_comparison_identity() {
    let fixture = Fixture::new();
    let mut paths = Vec::new();
    for name in ["first", "second"] {
        let mut report = calibration(name, None);
        report["measurement"]["browser"]["pixelRatio"] = serde_json::Value::Null;
        let path = fixture.write(&format!("{name}.json"), &report);
        let source = fs::read_to_string(&path)
            .unwrap()
            .replace("\"pixelRatio\":null", "\"pixelRatio\":NaN");
        fs::write(&path, source).unwrap();
        paths.push(path);
    }
    let args = [
        "compare",
        paths[0].to_str().unwrap(),
        paths[1].to_str().unwrap(),
    ];
    let expected = oracle(&args);
    assert!(!expected.status.success());
    assert!(expected.stdout.is_empty());
    assert_eq!(
        format!("capacity: {}\n", native(&args).unwrap_err()).as_bytes(),
        expected.stderr
    );
    let single = ["compare", paths[0].to_str().unwrap()];
    let expected = oracle(&single);
    assert!(expected.status.success());
    assert_eq!(native(&single).unwrap().as_bytes(), expected.stdout);
}

#[test]
fn report_reads_refuse_final_symlinks_devices_and_files_over_the_original_bound() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let path = fixture.write("report.json", &calibration("original", None));
    let alias = fixture.0.join("alias.json");
    symlink(&path, &alias).unwrap();
    assert!(json::read(&alias).is_err());
    assert!(json::read(std::path::Path::new("/dev/null")).is_err());
    let file = fs::File::create(&path).unwrap();
    file.set_len(128 * 1024 * 1024 + 1).unwrap();
    assert!(
        json::read(&path)
            .unwrap_err()
            .to_string()
            .contains("result_too_large")
    );
}

#[test]
fn generated_host_suggestions_match_python_without_running_a_workload() {
    let mut cases = Vec::new();
    for index in 0..160 {
        let cpus = [1, 2, 4, 8, 16, 65, 128, 256][index % 8];
        let memory = [0.125, 0.5, 1.0, 2.0, 3.125123, 12.0, 128.0, 2048.0][(index / 2) % 8];
        let port = [0.0, 0.125, 100.0, 999.999, 1000.0, 10000.0, -1.0][(index / 3) % 7];
        let mut arguments = vec![
            "suggest".to_owned(),
            "--vcpus".into(),
            cpus.to_string(),
            "--memory-gib".into(),
            memory.to_string(),
            "--port-mbps".into(),
            port.to_string(),
        ];
        if index % 3 == 0 {
            arguments.extend(["--env".into()]);
        }
        if index % 4 == 0 {
            arguments.extend(["--included-egress-tb".into(), "5.25".into()]);
        }
        if index % 5 == 0 {
            arguments.extend([
                "--app-cpus".into(),
                "2.75".into(),
                "--app-memory-mib".into(),
                "2048".into(),
            ]);
        }
        cases.push(arguments);
    }
    let mut child = Process::new("python3")
        .args([
            "-c",
            r"
import contextlib, io, json, pathlib, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]) / 'build'))
import capacity
outputs=[]
for args in json.load(sys.stdin):
    stream=io.StringIO()
    with contextlib.redirect_stdout(stream):
        assert capacity.main(args) == 0
    outputs.append(stream.getvalue())
print(json.dumps(outputs))
",
            source().to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    for (arguments_, expected) in cases.into_iter().zip(expected) {
        let command =
            arguments(&arguments_.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        assert_eq!(execute(command).unwrap(), expected, "{arguments_:?}");
    }
}

#[test]
fn generated_calibrated_suggestions_preserve_cpu_memory_network_and_fallback_policy() {
    let mut cases = Vec::new();
    for index in 0..120 {
        let meetings = 5 * (1 + index % 31);
        let viewers = 10 * (1 + index % 37);
        let mut ceilings = json!({
            "meetings":{"ceiling":meetings,"egressMbps":meetings as f64 * (0.5 + (index % 7) as f64),
                "memoryPeakBytes":(index % 8) * 64 * 1048576,"workers":1 + index % 4},
            "large-meeting":{"ceiling":10 + index % 30,"memoryPeakBytes":128 * 1048576,"workers":2},
            "webinar":{"ceiling":viewers,"egress_mbps":viewers as f64 * 0.75,
                "memory_peak_bytes":(index % 7) * 64 * 1048576,"workers":1 + index % 3},
        });
        if index % 11 == 0 {
            ceilings.as_object_mut().unwrap().remove("meetings");
        }
        if index % 13 == 0 {
            ceilings.as_object_mut().unwrap().remove("webinar");
        }
        cases.push(json!({"host":[1 + index % 65, (512 + index % 24 * 512),
            ([10.0, 100.0, 1000.0, 10000.0][index as usize % 4])],
            "ceilings":ceilings,"cpus":if index%3==0 {Some(2.75)} else {None},
            "memory":if index%5==0 {Some(1536)} else {None}}));
    }
    let mut child = Process::new("python3")
        .args([
            "-c",
            r"
import json,pathlib,sys
sys.path.insert(0,str(pathlib.Path(sys.argv[1])/'build'))
import capacity
results=[]
for case in json.load(sys.stdin):
    ceilings=capacity.ceilings_from_report({'ceilings':case['ceilings']})
    value=capacity.suggest_for(capacity.Host(*case['host']),ceilings,
        app_cpus=case['cpus'],app_memory_mib=case['memory'])
    results.append(capacity.record(value))
print(json.dumps(results))
",
            source().to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    for (case, expected) in cases.into_iter().zip(expected) {
        let report =
            json::parse(&serde_json::to_vec(&json!({"ceilings":case["ceilings"]})).unwrap())
                .unwrap();
        let host = model::Host {
            vcpus: case["host"][0].as_i64().unwrap(),
            memory_mib: case["host"][1].as_i64().unwrap(),
            port_mbps: case["host"][2].as_f64().unwrap(),
        };
        let value = model::suggest(
            host,
            &model::ceilings(&report).unwrap(),
            case["cpus"].as_f64(),
            case["memory"].as_i64(),
        )
        .unwrap();
        let actual = json!({"deployment":{"app_cpus":value.deployment.cpus,
            "memory_mib":value.deployment.memory_mib,"worker_threshold":0.7},
            "meetingParticipants":value.meetings,"meetingLimitedBy":value.meeting_limit,
            "webinarViewers":value.viewers,"webinarLimitedBy":value.viewer_limit,
            "roomParticipants":value.room,"maxConnections":value.connections,"settings":value.settings()});
        assert_eq!(actual, expected, "{case}");
    }
}
