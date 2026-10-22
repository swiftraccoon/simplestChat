//! Actual byte-retained JavaScript smoke controllers, never a service/workload.

use crate::bench::process::{self, Command, NativeHost, Sink};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

const SOURCE: &str = "d835ec261c15d926a32328bd7d7fcb0ec49257e0";
const REFERENCES: [(&[u8], &str); 4] = [
    (
        include_bytes!("reference/build/shutdown-smoke.mjs"),
        "573c35b173491b99046ef7a98c639cb5cdfcc9a4bf52f4db1a3ebbfdee398f39",
    ),
    (
        include_bytes!("reference/build/diagnostics-smoke.mjs"),
        "f60f5bced00f7e57abb83907d0d11149eef1980af695d1eeb7a0c721977ba7c9",
    ),
    (
        include_bytes!("reference/load_tests/report-command.mjs"),
        "4faab1a50dd5d411b3886883cfc4773b46725570941685b69a95f837bfcb78f9",
    ),
    (
        include_bytes!("reference/load_tests/diagnostic-report.mjs"),
        "8a8fa4b0a027d6338059eed3050ffce81433aef0bb2f46c6892bb072d4ecbdcb",
    ),
];

fn cases() -> Vec<Value> {
    let mut result = Vec::new();
    for options in [
        json!({}),
        json!({"missingBinary":true}),
        json!({"spawnError":true}),
        json!({"earlyExit":true}),
        json!({"status":503}),
        json!({"fetchError":true}),
        json!({"readyBody":{}}),
        json!({"readyJsonError":true}),
        json!({"neverOpen":true}),
        json!({"socketError":true}),
        json!({"neverJoin":true}),
        json!({"admissionError":true}),
        json!({"noRestartNotice":true}),
        json!({"wrongReason":true}),
        json!({"terminalRoom":true}),
        json!({"noClose":true}),
        json!({"closeCode":1006}),
        json!({"cleanClose":false}),
        json!({"peerCloseCode":1006}),
        json!({"peerCloseClean":false}),
        json!({"noPeerClose":true}),
        json!({"exitCode":42}),
        json!({"exitSignal":"SIGTERM"}),
        json!({"killRefused":true}),
        json!({"stalled":true}),
        json!({"cleanupFails":true,"output":"x".repeat(40_000)}),
        json!({"neverOpen":true,"closeThrows":true}),
        json!({"cancelBefore":true}),
        json!({"status":503,"cancelPolling":true}),
    ] {
        result.push(json!({"family":"shutdown","options":options}));
    }
    let operation = json!({"schemaVersion":1,"kind":"operation","operationId":1,
        "connectionId":1,"operation":"join_room","stage":null,"outcome":"ok",
        "startedUs":1,"elapsedUs":100});
    for options in [
        json!({}),
        json!({"missing":true}),
        json!({"noSummary":true}),
        json!({"summary":{"dropped":1}}),
        json!({"summary":{"unfinished":1}}),
        json!({"raw":"{\"PRIVATE_SENTINEL\":1}\n"}),
        json!({"records":[operation]}),
        json!({"exitCode":42}),
        json!({"exitSignal":"SIGTERM"}),
        json!({"malformedReport":true}),
        json!({"invalidReportJson":true}),
        json!({"reportProcessCode":42}),
        json!({"reportProcessCode":1,"reportKilled":true}),
        json!({"reportStatus":1}),
        json!({"noSummary":true,"reportStatus":0}),
        json!({"failWrite":"server-diagnostics-report.json"}),
        json!({"failWrite":"diagnostics-smoke.json"}),
        json!({"cancelBefore":true}),
        json!({"cancelReport":true}),
        json!({"cancelPublication":true}),
        json!({"cleanupFails":true}),
        json!({"cleanupFails":true,"failLog":true}),
        json!({"status":503,"cancelPolling":true}),
    ] {
        result.push(json!({"family":"diagnostics","options":options}));
    }
    result
}

async fn reference(cases: &[Value]) -> Value {
    assert_eq!(SOURCE.len(), 40);
    for (bytes, expected) in REFERENCES {
        let actual: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected, "Retained source-73 reference changed");
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let node = std::env::split_paths(&path)
        .map(|part| part.join("node"))
        .find(|path| path.is_file())
        .expect("Installed Node is required by the inert reference oracle");
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bench/smoke");
    let environment: BTreeMap<OsString, OsString> = [("PATH".into(), path)].into();
    let input = serde_json::to_vec(&json!({"cases":cases})).unwrap();
    assert!(input.len() <= 128 * 1024);
    let output = process::run(
        &NativeHost::default(),
        Command {
            executable: node,
            arguments: vec!["oracle.mjs".into()],
            directory,
            environment,
            timeout: Duration::from_secs(15),
            stdout_limit: 1024 * 1024,
            stderr_limit: 65536,
            input,
            stdout: Sink::Capture,
            stderr: Sink::Capture,
            cleanup: false,
        },
    )
    .await
    .unwrap();
    assert!(
        output.exit.success(),
        "Inert actual JavaScript oracle must finish"
    );
    assert!(
        output.stderr.is_empty(),
        "The oracle must not expose private diagnostics"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn actual_retained_smokes_cover_protocol_verdict_artifacts_and_legacy_cli_statuses() {
    let cases = cases();
    let result = reference(&cases).await;
    let rows = result["results"].as_array().unwrap();
    assert_eq!(rows.len(), 52);
    for (case, row) in cases.iter().zip(rows) {
        assert_eq!(
            row["passed"],
            case["options"].as_object().unwrap().is_empty()
        );
        assert_eq!(row["trace"]["reportAfterExit"], true);
    }
    let shutdown = &rows[0];
    assert_eq!(shutdown["shutdown"]["peerCloseCode"], 1000);
    assert_eq!(shutdown["shutdown"]["closeCode"], 1001);
    assert_eq!(shutdown["trace"]["messages"].as_array().unwrap().len(), 1);
    let diagnostics = &rows[29];
    assert_eq!(diagnostics["diagnostic"]["records"], 2);
    assert_eq!(
        diagnostics["diagnostic"]["evidence"],
        json!({"joinRoom":true,"roomLockWait":true})
    );
    assert_eq!(
        diagnostics["artifactNames"],
        json!([
            "diagnostics-smoke.json",
            "server-diagnostics-report.json",
            "server-diagnostics.jsonl"
        ])
    );
    for index in [49, 50] {
        assert_eq!(rows[index]["trace"]["reportReads"], 0);
        assert_eq!(
            rows[index]["diagnostic"]["artifactIssues"][0],
            "server_not_stopped"
        );
    }
    for index in [47, 48] {
        assert_eq!(rows[index]["savedPassed"], false);
        assert!(
            rows[index]["diagnostic"]["artifactIssues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "smoke_cancelled")
        );
    }
    let cli = result["cli"].as_array().unwrap();
    assert_eq!(cli.len(), 10);
    for (index, expected) in [0, 2, 0, 1, 0, 0, 2, 0, 1, 1].into_iter().enumerate() {
        assert_eq!(cli[index]["status"], expected);
    }
    // The old shutdown CLI has no post-run cancellation fence. Native final
    // cancellation checks intentionally strengthen this observed legacy case.
    assert_eq!(cli[4]["phase"], "late-cancellation");
    assert_eq!(cli[4]["status"], 0);
}

fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_), Value::Number(_)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| same(left, right)))
        }
        _ => left == right,
    }
}

#[tokio::test]
async fn complete_native_smokes_match_the_actual_reference_on_shared_runtime_cases() {
    let cases: Vec<_> = cases()
        .into_iter()
        .filter(|case| {
            if case["family"] == "shutdown" {
                return true;
            }
            // Native uses the linked typed reader after positive writer/group/
            // stream retirement. External-generator failures and independent
            // publication/admission fault seams are retained-reference cases,
            // rather than an invented native report subprocess ecosystem.
            let options = case["options"].as_object().unwrap();
            options.is_empty()
                || options.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "noSummary" | "summary" | "raw" | "records" | "exitCode" | "exitSignal"
                    )
                })
        })
        .collect();
    assert_eq!(cases.len(), 37);
    let expected = reference(&cases).await;
    let rows = expected["results"].as_array().unwrap();
    assert_eq!(rows.len(), cases.len());
    for (index, (case, expected)) in cases.iter().zip(rows).enumerate() {
        let diagnostics = case["family"] == "diagnostics";
        let actual = super::tests::oracle_case(&case["options"], diagnostics).await;
        assert_eq!(actual["passed"], expected["passed"], "case {index}");
        // The diagnostics facade internally retains successful shutdown facts
        // even if coverage fails; the JS wrapper exposes only its result row.
        if diagnostics {
            assert!(
                same(&actual["diagnostic"], &expected["diagnostic"]),
                "diagnostic case {index}: actual={actual},expected={expected}"
            );
        } else {
            assert!(
                same(&actual["shutdown"], &expected["shutdown"]),
                "shutdown case {index}: actual={actual},expected={expected}"
            );
        }
    }
}
