//! Compare actual supported policy exports; never slice source text or run load.

use super::{commands, comparison, options, resources, subscription, summary, verdict};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn root() -> PathBuf {
    std::env::var_os("BENCH_REFERENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        })
}

fn oracle(cases: &[Value]) -> Vec<Value> {
    let script = r#"
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
const policy = await import(pathToFileURL(join(process.argv[1], 'load_tests/benchmark-local.mjs')));
const rows = JSON.parse(readFileSync(0, 'utf8'));
const output = rows.map(row => {
  try { return {ok:true, value:policy[row.op](...row.args)}; }
  catch { return {ok:false}; }
});
process.stdout.write(JSON.stringify(output));
"#;
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(root())
        .current_dir(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "reference import must succeed");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn outcome(result: anyhow::Result<impl serde::Serialize>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(_) => json!({"ok":false}),
    }
}

fn native(case: &Value) -> Value {
    let args = case["args"].as_array().unwrap();
    match case["op"].as_str().unwrap() {
        "parseOptions" => {
            let args: Vec<_> = args[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| OsString::from(value.as_str().unwrap()))
                .collect();
            outcome(options::parse(&args, &root()))
        }
        "resourceSummary" => {
            let samples =
                serde_json::from_value::<Vec<resources::Sample>>(args[0].clone()).unwrap();
            let role = if args[1] == "server" {
                resources::Role::Server
            } else {
                resources::Role::Generator
            };
            outcome(resources::summarize(
                &samples,
                role,
                args[2].as_f64().unwrap(),
                args[3].as_f64().unwrap(),
            ))
        }
        "cpuSeconds" => outcome(resources::cpu_seconds(args[0].as_str().unwrap())),
        "linuxProcessSample" => outcome(resources::linux_sample(
            args[0].as_str().unwrap(),
            args[1].as_f64().unwrap(),
            args[2].as_f64().unwrap(),
        )),
        "performanceRunStatus" => outcome(Ok(verdict::performance(args[0].as_array().unwrap()))),
        "diagnosticRunStatus" => outcome(Ok(verdict::diagnostic(args[0].as_array().unwrap()))),
        "comparison" => outcome(comparison::compare(args[0].as_array().unwrap())),
        "expectedSubscriptionPlan" => outcome(subscription::expected(
            &serde_json::from_value(args[0].clone()).unwrap(),
            &serde_json::from_value(args[1].clone()).unwrap(),
        )),
        "verifySubscriptionReport" => outcome(subscription::verify(
            &args[0],
            &args[1],
            &serde_json::from_value(args[2].clone()).unwrap(),
            &serde_json::from_value(args[3].clone()).unwrap(),
        )),
        "lifecycleWorkload" => outcome(Ok(summary::lifecycle_workload(
            &args[0],
            &serde_json::from_value(args[1].clone()).unwrap(),
            args[2].as_str().unwrap(),
        ))),
        "admitsDiagnosticEvidence" => outcome(Ok(summary::admits_diagnostics(
            &args[0],
            &serde_json::from_value(args[1].clone()).unwrap(),
            args[2].as_str().unwrap(),
            args[3].as_bool().unwrap(),
        ))),
        "serverDiagnosticEnvironment" => outcome(Ok(commands::diagnostic_environment(
            &serde_json::from_value(args[0].clone()).unwrap(),
            std::path::Path::new(args[1].as_str().unwrap()),
        ))),
        "captureArguments" => outcome(commands::capture(
            &serde_json::from_value(args[0].clone()).unwrap(),
            std::path::Path::new(args[1].as_str().unwrap()),
        )),
        _ => panic!("unrecognized test operation"),
    }
}

pub(super) fn compare(cases: &[Value]) {
    let expected = oracle(cases);
    assert_eq!(expected.len(), cases.len());
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        // JSON artifact consumers use one Number domain. Rust's Value equality
        // distinguishes the integer and float spellings of the same JS number.
        fn numbers(value: &mut Value) {
            match value {
                Value::Number(number) => {
                    *number = serde_json::Number::from_f64(number.as_f64().unwrap()).unwrap()
                }
                Value::Array(values) => values.iter_mut().for_each(numbers),
                Value::Object(values) => values.values_mut().for_each(numbers),
                _ => {}
            }
        }
        let mut actual = native(case);
        let mut expected = expected;
        numbers(&mut actual);
        numbers(&mut expected);
        assert_eq!(actual, expected, "case{index}: {case}");
    }
}

pub(super) fn required() -> Vec<String> {
    [
        "--baseline-root",
        ".",
        "--baseline-bin",
        "./server",
        "--candidate-root",
        ".",
        "--candidate-bin",
        "./server",
        "--generator",
        "./generator",
        "--output",
        "./new output",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn options_match_reference_admission_duration_graph_and_diagnostic_policy() {
    let mut cases = vec![json!({"op":"parseOptions","args":[required()]})];
    let mut add = |extra: Vec<String>| {
        let mut args = required();
        args.extend(extra);
        cases.push(json!({"op":"parseOptions","args":[args]}));
    };
    for (key, values) in [
        (
            "--duration",
            vec![
                "2", "3", "180", "181", "3.0", "3e0", " 3 ", "0x10", "0b11", "0o10", "3e", "NaN",
            ],
        ),
        (
            "--clients",
            vec![
                "2", "9", "10", "30", "100", "101", "2,2", "2,3", "", "2,", "0xA",
            ],
        ),
        ("--workers", vec!["0", "1", "4", "5"]),
        ("--udp-port", vec!["1023", "1024", "65531", "65532"]),
        (
            "--scenarios",
            vec![
                "conference",
                "multi-room",
                "audio",
                "webinar",
                "churn",
                "audio,audio",
                "PRIVATE",
            ],
        ),
        (
            "--subscription-plan",
            vec!["fifo", "ring-v1", "hotspot-v1", "PRIVATE"],
        ),
    ] {
        for value in values {
            add(vec![key.to_owned(), value.to_owned()]);
        }
    }
    for clients in [2, 9, 10, 11, 30, 31, 40, 100] {
        for scenario in ["conference", "multi-room", "churn"] {
            for ramp in [5, 61, 65, 202] {
                add(vec![
                    "--clients".into(),
                    clients.to_string(),
                    "--scenarios".into(),
                    scenario.into(),
                    "--ramp-up".into(),
                    ramp.to_string(),
                ]);
            }
        }
    }
    for purpose in ["performance", "diagnostic"] {
        for detail in ["full", "capture-only"] {
            for capture in [None, Some("lo0"), Some("lo"), Some("eth0")] {
                for departure in [None, Some("abrupt"), Some("explicit-leave")] {
                    let mut extra = vec![
                        "--purpose".into(),
                        purpose.into(),
                        "--diagnostic-detail".into(),
                        detail.into(),
                    ];
                    if let Some(capture) = capture {
                        extra.extend(["--capture-interface".into(), capture.into()]);
                    }
                    if let Some(departure) = departure {
                        extra.extend(["--departure".into(), departure.into()]);
                    }
                    add(extra);
                }
            }
        }
    }
    for mode in ["fifo", "ring-v1", "hotspot-v1"] {
        for seed in ["0", "17", "4294967295", "4294967296", "01", "-1", "1.0"] {
            add(vec![
                "--subscription-plan".into(),
                mode.into(),
                "--subscription-seed".into(),
                seed.into(),
            ]);
        }
    }
    compare(&cases);
}

#[test]
fn resource_accounting_matches_reference_measured_deltas_missing_samples_and_decimal_inputs() {
    let mut cases = Vec::new();
    let mut seed = 0x5829_61a3_u32;
    for count in 2..32 {
        for missing in [0, 1, 3, 6] {
            let mut samples = Vec::new();
            for index in 0..count {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let jitter = (seed as f64 / u32::MAX as f64) * 10.0;
                let reading = if missing != 0 && index % missing == 0 {
                    Value::Null
                } else {
                    json!({"rssKiB":1024.0 + jitter * index as f64, "cpuSeconds":index as f64 * 0.23948274})
                };
                samples.push(json!({"elapsedMs":index as f64 * 500.0 + jitter,
                    "server":reading,"generator":{"rssKiB":4096,"cpuSeconds":index}}));
            }
            for role in ["server", "generator"] {
                cases.push(json!({"op":"resourceSummary","args":[samples,role,0,count * 500]}));
            }
        }
    }
    for text in [
        "01:30",
        "1:01:30.12",
        "2-03:04:05.125",
        "0:00.0",
        "1",
        "PRIVATE",
        "0:0:0:0",
        "3e0:0x10",
    ] {
        cases.push(json!({"op":"cpuSeconds","args":[text]}));
    }
    let stat =
        "42 (owned worker (name)) S 0 1 1 0 -1 4194560 100 0 0 0 100 20 0 0 20 0 3 0 5 1000 200 0";
    for (ticks, page) in [(100, 4096), (1000, 16384), (0, 4096)] {
        cases.push(json!({"op":"linuxProcessSample","args":[stat,ticks,page]}));
    }
    compare(&cases);
}

#[test]
fn independent_run_verdicts_and_paired_comparisons_match_reference() {
    let mut cases = Vec::new();
    for workload in [Value::Null, json!(true), json!(false), json!("true")] {
        for exit in [
            Value::Null,
            json!({"code":0}),
            json!({"code":1}),
            json!({"code":0,"signal":"SIGTERM"}),
            json!({"code":0,"error":"PRIVATE"}),
            json!({"code":0,"signal":null,"error":""}),
        ] {
            let base = json!({"workloadPassed":workload,"serverExit":exit});
            cases.push(json!({"op":"performanceRunStatus","args":[[base]]}));
            for evidence in [
                Value::Null,
                json!({"requested":false}),
                json!({"requested":true,"complete":true}),
                json!({"requested":true,"complete":false}),
            ] {
                let mut row = base.clone();
                for key in [
                    "diagnosticCoverage",
                    "mediaDiagnosticCoverage",
                    "lifecycleDiagnosticCoverage",
                ] {
                    row[key] = evidence.clone();
                }
                for stall in [
                    None,
                    Some(Value::Null),
                    Some(json!({"requested":false})),
                    Some(json!({"requested":true,"written":true,"available":true})),
                    Some(json!({"requested":true,"written":false,"available":true})),
                ] {
                    let mut row = row.clone();
                    if let Some(stall) = stall {
                        row["receiverStallReport"] = stall;
                    }
                    cases.push(json!({"op":"diagnosticRunStatus","args":[[row]]}));
                }
            }
        }
    }
    cases.push(json!({"op":"performanceRunStatus","args":[[]]}));
    cases.push(json!({"op":"diagnosticRunStatus","args":[[]]}));
    let fields = [
        "joinP99Ms",
        "sendReadyP99Ms",
        "receiveReadyP99Ms",
        "receivedPacketsPerSecond",
        "serverCpuPercent",
        "serverPeakRssMiB",
        "generatorCpuPercent",
        "generatorPeakRssMiB",
    ];
    for count in 1..=5 {
        let mut rows = Vec::new();
        for scenario in ["conference-10", "audio-10"] {
            for variant in ["baseline", "candidate"] {
                for repeat in 0..count {
                    let mut row = json!({"scenario":scenario,"variant":variant,"workloadPassed":true,"serverExit":{"code":0}});
                    for (index, field) in fields.iter().enumerate() {
                        row[*field] = json!(
                            (index + repeat) as f64 * 2.145837259
                                / if variant == "baseline" { 1.0 } else { 0.89134 }
                        );
                    }
                    rows.push(row);
                }
            }
        }
        cases.push(json!({"op":"comparison","args":[rows]}));
        for mutate in [
            "missing-peer",
            "mismatched-graph",
            "failed-shutdown",
            "diagnostic",
            "missing-field",
        ] {
            let mut rows = rows.clone();
            match mutate {
                "missing-peer" => {
                    rows.pop();
                }
                "mismatched-graph" => {
                    rows[0]["subscriptionPlan"] = json!("PRIVATE");
                }
                "failed-shutdown" => {
                    rows[0]["serverExit"]["code"] = json!(1);
                }
                "diagnostic" => {
                    rows[0]["purpose"] = json!("diagnostic");
                }
                "missing-field" => {
                    rows[0].as_object_mut().unwrap().remove("joinP99Ms");
                }
                _ => unreachable!(),
            }
            cases.push(json!({"op":"comparison","args":[rows]}));
        }
    }
    compare(&cases);
}
