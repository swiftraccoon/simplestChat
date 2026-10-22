//! Complete unchanged JavaScript journey with inert children, wire and browsers.
//! The selected installed Node is the only real child; no guest program runs.

use crate::bench::process::{self, Command, NativeHost, Sink};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf,
    time::Duration,
};

const SOURCE: &str = "e9bbf8cf69626b2faf72620b5acbf57bc2c40aa1";
const REFERENCES: [(&[u8], &str); 3] = [
    (
        include_bytes!("reference/build/restart-browser-smoke.mjs"),
        "1c68528ede669695dcd63bb1a6f8a398d8e331ab18ac42b8e8f73e20ca34a485",
    ),
    (
        include_bytes!("reference/build/shutdown-smoke.mjs"),
        "573c35b173491b99046ef7a98c639cb5cdfcc9a4bf52f4db1a3ebbfdee398f39",
    ),
    (
        include_bytes!("reference/web/e2e/lifecycle-cleanup.cjs"),
        "c8707b96ddd3a1bc9582478e91401f980ba2dc692af077dc74a99576cfb23ac4",
    ),
];

fn common() -> Vec<Value> {
    [
        json!({}),
        json!({"browser":"firefox"}),
        json!({"browser":"webkit"}),
        json!({"dropBefore":true}),
        json!({"dropAfter":true}),
        json!({"noInitialJoin":true}),
        json!({"noRestartNotice":true}),
        json!({"noRejoin":true}),
        json!({"navigateOnRestart":true}),
        json!({"lostDraft":true}),
        json!({"terminalRoom":true}),
        json!({"capture":true}),
        json!({"malformed":true}),
        json!({"pageError":true}),
        json!({"external":true}),
        json!({"foreignSocket":true}),
        json!({"launchFailure":true}),
        json!({"connectFailure":true}),
    ]
    .into()
}
fn cases() -> Vec<Value> {
    let mut cases = common();
    cases.extend([
        json!({"invalidCapture":true}),
        json!({"serverExit42":true}),
        json!({"serverStartFailure":true}),
        json!({"killRefused":true}),
        json!({"serverTermHang":true}),
        json!({"closeTimeout":true}),
        json!({"cleanupFailure":true}),
        json!({"cancelLaunch":true}),
        json!({"cancelRestart":true}),
        json!({"cancelBefore":true}),
        json!({"fetchFailure":true}),
        json!({"serverEarlyExit":true}),
        json!({"missingBinary":true}),
        json!({"serverStartFailure":2}),
        json!({"browserExit":9}),
        json!({"cleanupFailure":true,"browserKillFailure":true}),
        json!({"ports":{"http":999,"media":45102}}),
        json!({"readyStatus":503}),
        json!({"readyBody":{"status":"wrong"}}),
        json!({"lateCleanupExternal":true}),
        json!({"lateCleanupPageError":true}),
    ]);
    cases
}
async fn reference(cases: &[Value]) -> Value {
    assert_eq!(SOURCE.len(), 40);
    for (bytes, expected) in REFERENCES {
        let actual: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected, "Retained source-76 reference changed");
    }
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bench/restart_browser");
    let captured_path = std::env::var_os("PATH").unwrap_or_default();
    let node = std::env::split_paths(&captured_path)
        .map(|component| component.join("node"))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .expect("Installed Node is required by the inert reference oracle")
        .canonicalize()
        .unwrap();
    let environment: BTreeMap<OsString, OsString> = [("PATH".into(), captured_path)].into();
    let input = serde_json::to_vec(&json!({"cases":cases})).unwrap();
    assert!(input.len() <= 256 * 1024);
    let output = process::run(
        &NativeHost::default(),
        Command {
            executable: node,
            arguments: vec![
                "--experimental-vm-modules".into(),
                "--no-warnings".into(),
                "oracle.mjs".into(),
            ],
            directory,
            environment,
            timeout: Duration::from_secs(30),
            stdout_limit: 2 * 1024 * 1024,
            stderr_limit: 65536,
            input,
            stdout: Sink::Capture,
            stderr: Sink::Capture,
            cleanup: false,
        },
    )
    .await
    .unwrap();
    assert!(output.exit.success(), "Actual reference oracle must settle");
    assert!(
        output.stderr.is_empty(),
        "Reference errors must remain private"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn actual_reference_preserves_complete_browser_assertions_artifacts_and_statuses() {
    let cases = cases();
    let result = reference(&cases).await;
    let rows = result["results"].as_array().unwrap();
    assert_eq!(rows.len(), 39);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row["passed"],
            !(3..37).contains(&index),
            "case {}",
            cases[index]
        );
    }
    let happy = &rows[0];
    assert_eq!(happy["observation"]["phase"], "complete");
    assert_eq!(happy["observation"]["draftPreserved"], true);
    assert_eq!(
        happy["observation"]["messageDelivery"],
        json!({"before":true,"after":true})
    );
    assert_eq!(
        happy["observation"]["captureCalls"],
        json!([{"user":0,"display":0},{"user":0,"display":0}])
    );
    assert_eq!(happy["browserCleanupPassed"], true);
    assert_eq!(happy["trace"]["spawn"], 2);
    assert_eq!(happy["trace"]["launches"], 1);
    assert_eq!(happy["trace"]["connections"], 1);
    assert_eq!(happy["trace"]["browserCloses"], 1);
    assert_eq!(happy["trace"]["browserKills"], 0);
    assert_eq!(
        result["cli"],
        json!([
            {"phase":"success","status":0},
            {"phase":"failure","status":1},
            {"phase":"cancelled","status":1},
            {"phase":"help","status":0},
            {"phase":"arguments","status":2}
        ])
    );
    // The runtime fixture executes the complete Rust owner. Only virtual time,
    // ports, UUIDs and paths are normalized; the browser observations are whole.
    for (options, reference) in common().iter().zip(rows) {
        let native = super::tests::oracle_case(options).await;
        let mut expected = reference.clone();
        expected.as_object_mut().unwrap().remove("trace");
        assert_eq!(native, expected, "Native/reference case {options}");
    }
    // Unknown capture fields are a named stricter projection domain. Original
    // JavaScript appends them then fails; native refuses before retaining them.
    let invalid_capture = super::tests::oracle_case(&cases[18]).await;
    assert_eq!(invalid_capture["passed"], false);
    // Original checks run before browser cleanup. Native keeps observing and
    // refuses a late external request/page error even after phase completion.
    for (index, counter) in [(37, "blockedExternalRequests"), (38, "pageErrors")] {
        assert_eq!(rows[index]["observation"][counter], 1);
        let native = super::tests::oracle_case(&cases[index]).await;
        assert_eq!(native["passed"], false);
        assert_eq!(native["observation"][counter], 1);
    }
}
