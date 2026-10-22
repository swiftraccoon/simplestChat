//! Actual CLI against independently owned snapshots. No signature verifier,
//! process/host runtime, network or installed helper is exercised.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    envelope: PathBuf,
    receipt: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-receipt-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Self {
            envelope: root.join("envelope.json"),
            receipt: root.join("receipt.json"),
            root,
        };
        fixture.reset();
        fixture
    }
    fn reset(&self) {
        fs::write(
            &self.envelope,
            serde_json::to_vec_pretty(&envelope()).unwrap(),
        )
        .unwrap();
        fs::write(
            &self.receipt,
            serde_json::to_vec_pretty(&receipt()).unwrap(),
        )
        .unwrap();
    }
    fn run(&self) -> Output {
        run(&self.envelope, &self.receipt)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn run(envelope: &Path, receipt: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args([
            "release".as_ref(),
            "receipt".as_ref(),
            "--envelope".as_ref(),
            envelope.as_os_str(),
            "--receipt".as_ref(),
            receipt.as_os_str(),
        ])
        .output()
        .unwrap()
}
fn envelope() -> Value {
    let files = [
        "image.tar",
        "release.json",
        "outcome.json",
        "source.json",
        "sbom.spdx.json",
        "image-security.json",
        "runtime-proof.json",
        "vex.openvex.json",
    ]
    .into_iter()
    .map(|k| (k, "c".repeat(64)))
    .collect::<std::collections::BTreeMap<_, _>>();
    let verification = [
        "image.tar",
        "sbom.spdx.json",
        "runtime-proof.json",
        "vex.openvex.json",
    ]
    .into_iter()
    .map(|k| (k, "e".repeat(64)))
    .collect::<std::collections::BTreeMap<_, _>>();
    json!({"schemaVersion":1,"repository":"owner/repo","revision":"a".repeat(40),
        "artifactId":123,"buildRunId":789,"ciRunId":789,"runAttempt":1,"artifactZipBytes":100,
        "zipSha256":"f".repeat(64),"attestation":{
        "predicate":{"schemaVersion":1,"repository":"owner/repo","revision":"a".repeat(40),
            "runId":789,"runAttempt":1,"workflow":".github/workflows/ci.yml","ref":"refs/heads/main",
            "event":"push","platform":"linux/amd64","imageId":format!("sha256:{}","b".repeat(64)),
            "securityPassed":true,"fileDigests":files},
        "bundleSha256":"d".repeat(64),"verificationSha256":verification}})
}

fn receipt() -> Value {
    json!({"schemaVersion":1,"status":"complete","passed":true,"settled":true,"phase":"complete",
        "failureClass":null,"evidence":format!("/srv/simplestchat-public/releases/{}/download.fixture","a".repeat(40)),
        "revision":"a".repeat(40),"artifactId":123,"zipSha256":"f".repeat(64),"artifactZipBytes":100,
        "downloadedBytes":100,"archiveSha256":"c".repeat(64),"manifestSha256":"c".repeat(64),
        "elapsedSeconds":1.25,"finalized":true})
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn refused(output: Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        b"{\"passed\":false,\"failure\":\"invalid_release_receipt\"}\n"
    );
}
#[test]
fn output_contains_only_the_sixteen_selected_fields_and_both_original_identities() {
    let fixture = Fixture::new();
    let mut source = receipt();
    source["private_unrecognized"] = json!({"token":"PRIVATE_SENTINEL"});
    let text = serde_json::to_string_pretty(&source).unwrap();
    let mut original = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        original.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&fixture.receipt, &original).unwrap();
    let env_source = serde_json::to_string_pretty(&envelope()).unwrap();
    let mut env_original = vec![0, 0, 0xfe, 0xff];
    for ch in env_source.chars() {
        env_original.extend_from_slice(&u32::from(ch).to_be_bytes());
    }
    fs::write(&fixture.envelope, &env_original).unwrap();
    let output = fixture.run();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE_SENTINEL"));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.as_object().unwrap().len(), 5);
    assert_eq!(result["receipt"], receipt());
    assert_eq!(result["receipt"].as_object().unwrap().len(), 16);
    assert_eq!(result["envelopeSha256"], sha(&env_original));
    assert_eq!(result["envelopeBytes"], env_original.len());
    assert_eq!(result["receiptSha256"], sha(&original));
    assert_eq!(result["receiptBytes"], original.len());
    assert!(result.get("passed").is_none());
    assert!(result.get("attestation").is_none());
    assert_eq!(fs::read(&fixture.envelope).unwrap(), env_original);
    assert_eq!(fs::read(&fixture.receipt).unwrap(), original);
}
#[test]
fn a_consistent_failed_receipt_is_inspected_without_becoming_a_success() {
    let fixture = Fixture::new();
    let mut failed = receipt();
    failed["status"] = "failed".into();
    failed["passed"] = false.into();
    failed["finalized"] = false.into();
    failed["phase"] = "download".into();
    failed["failureClass"] = "download_size_mismatch".into();
    failed["downloadedBytes"] = 12.into();
    failed["archiveSha256"] = Value::Null;
    failed["manifestSha256"] = Value::Null;
    fs::write(&fixture.receipt, serde_json::to_vec(&failed).unwrap()).unwrap();
    let output = fixture.run();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["receipt"], failed);
    assert_eq!(result["receipt"]["passed"], false);
}
#[test]
fn identity_hash_type_and_unsettled_success_mismatches_are_fixed_private_failures() {
    let fixture = Fixture::new();
    for (key, value) in [
        ("revision", json!("0".repeat(40))),
        ("artifactId", json!(true)),
        ("zipSha256", json!("0".repeat(64))),
        ("archiveSha256", json!("0".repeat(64))),
        ("manifestSha256", json!("0".repeat(64))),
        ("settled", json!(false)),
        ("finalized", json!(false)),
        ("failureClass", json!("PRIVATE_SIGNED_URL")),
    ] {
        let mut changed = receipt();
        changed[key] = value;
        fs::write(&fixture.receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        refused(fixture.run());
    }
    fixture.reset();
    let mut changed = envelope();
    changed["attestation"]["predicate"]["runId"] = json!(790);
    fs::write(&fixture.envelope, serde_json::to_vec(&changed).unwrap()).unwrap();
    refused(fixture.run());
}
#[test]
fn both_inputs_enforce_bounded_regular_nonlink_snapshots() {
    let fixture = Fixture::new();
    let link = fixture.root.join("PRIVATE_LINK");
    let fifo = fixture.root.join("PRIVATE_FIFO");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    symlink(&fixture.receipt, &link).unwrap();
    for path in [
        &link,
        &fifo,
        &fixture.root,
        &fixture.root.join("PRIVATE_MISSING"),
    ] {
        refused(run(path, &fixture.receipt));
        refused(run(&fixture.envelope, path));
    }
    let directory_link = fixture.root.join("ancestor");
    symlink(&fixture.root, &directory_link).unwrap();
    refused(run(&directory_link.join("envelope.json"), &fixture.receipt));
    refused(run(&fixture.envelope, &directory_link.join("receipt.json")));
    fs::write(&fixture.envelope, vec![b' '; 16 * 1024 + 1]).unwrap();
    refused(fixture.run());
    fixture.reset();
    fs::write(&fixture.receipt, vec![b' '; 64 * 1024 + 1]).unwrap();
    refused(fixture.run());
    fixture.reset();
    fs::write(&fixture.receipt, b"{\"PRIVATE_SECRET\":NaN}").unwrap();
    refused(fixture.run());
}
#[test]
fn only_the_exact_argument_form_can_read_inputs() {
    for args in [
        vec!["release", "receipt"],
        vec![
            "release",
            "receipt",
            "--receipt",
            "PRIVATE_PATH",
            "--envelope",
            "PRIVATE_PATH",
        ],
        vec![
            "release",
            "receipt",
            "--envelope",
            "PRIVATE_PATH",
            "--receipt",
            "PRIVATE_PATH",
            "extra",
        ],
        vec![
            "release",
            "receipt",
            "--envelope",
            "PRIVATE_PATH",
            "--envelope",
            "PRIVATE_PATH",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let usage = String::from_utf8(output.stderr).unwrap();
        assert!(usage.contains("release receipt --envelope PATH --receipt PATH"));
        assert!(!usage.contains("PRIVATE_PATH"));
    }
}
