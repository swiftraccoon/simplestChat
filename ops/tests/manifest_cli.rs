//! Execute the actual small binary against owned files; no host/engine/network
//! boundary or Python orchestration callsite is involved.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-ops-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(args)
        .output()
        .unwrap()
}
fn source() -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({"schemaVersion":1,"revision":"a".repeat(40),
        "platform":"linux/amd64","archiveSha256":"b".repeat(64),
        "imageTag":format!("simplestchat-release/production:{}","a".repeat(40)),
        "migrations":{"1":"c".repeat(96)},"createdAt":"2026-10-21T00:00:00Z"}))
    .unwrap()
}
#[test]
fn successful_output_binds_original_bytes_and_cannot_claim_signed_release_success() {
    let root = Fixture::new();
    let path = root.0.join("release.json");
    let source = source();
    fs::write(&path, &source).unwrap();
    let output = cli(&[
        "release".as_ref(),
        "manifest".as_ref(),
        "--path".as_ref(),
        path.as_os_str(),
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["manifest"],
        serde_json::from_slice::<Value>(&source).unwrap()
    );
    assert_eq!(result["manifestBytes"], source.len());
    assert_eq!(
        result["manifestSha256"],
        Sha256::digest(&source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert!(result.get("passed").is_none());
    assert!(result.get("attestation").is_none());
    assert_eq!(fs::read(path).unwrap(), source);
}
#[test]
fn failures_are_fixed_and_do_not_echo_paths_or_private_values() {
    let root = Fixture::new();
    let path = root.0.join("PRIVATE_PATH.json");
    fs::write(&path, b"{\"PRIVATE_SECRET\":NaN}").unwrap();
    let link = root.0.join("symlink");
    symlink(&path, &link).unwrap();
    for path in [&path, &link, &root.0.join("missing")] {
        let output = cli(&[
            "release".as_ref(),
            "manifest".as_ref(),
            "--path".as_ref(),
            path.as_os_str(),
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            b"{\"passed\":false,\"failure\":\"invalid_release_manifest\"}\n"
        );
    }
}
#[test]
fn usage_and_help_never_read_a_manifest_or_start_another_command() {
    for args in [
        vec![],
        vec!["release"],
        vec!["release", "manifest", "--wrong", "PRIVATE_PATH"],
        vec!["release", "manifest", "--path", "PRIVATE_PATH", "extra"],
        vec!["deploy"],
    ] {
        let output = cli(&args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            concat!(
                "Usage: simplestchat-ops release build --output PATH ",
                "[--timeout-seconds N] [--image-id ID]\n",
                "       simplestchat-ops release verify [OPTIONS]\n",
                "       simplestchat-ops release fetch [OPTIONS]\n",
                "       simplestchat-ops release receive [OPTIONS]\n",
                "       simplestchat-ops release stage [OPTIONS]\n",
    "       simplestchat-ops release attestation prepare [OPTIONS]\n",
                "       simplestchat-ops release attestation verify [OPTIONS]\n",
                "       simplestchat-ops release manifest --path PATH\n",
                "       simplestchat-ops release archive --manifest PATH --path PATH\n",
                "       simplestchat-ops release receipt --envelope PATH --receipt PATH\n",
                "       simplestchat-ops release preflight [--helpers-json JSON]\n",
                "       simplestchat-ops release assets initialize [--revision REV]\n",
                "       simplestchat-ops release assets prune|inspect\n",
                "       simplestchat-ops release assets select --revision REV\n",
                "       simplestchat-ops release assets capture --path TAR\n",
                "       simplestchat-ops release assets publish --capture REV TAR [--capture REV TAR]\n",
                "       simplestchat-ops distribution inspect-executable --path PATH\n",
                "       simplestchat-ops distribution install --request ABS --request-sha256 SHA\n",
                "       simplestchat-ops distribution verify-pair --manifest PATH ",
                "--path PATH --selection PATH\n",
                "       simplestchat-ops distribution build-plan ",
                "--selection PATH --source-root PATH\n",
                "       simplestchat-ops distribution prepare-artifact --manifest PATH ",
                "--path PATH --selection PATH --evidence PATH --output PATH\n",
                "       simplestchat-ops capacity run --server-image IMAGE --generator-image IMAGE [OPTIONS]\n",
                "       simplestchat-ops capacity-vps serve --transport-json JSON --origin-monotonic-ns N\n",
                "       simplestchat-ops capacity-vps worker --run RUN --request-sha256 SHA\n",
            )
            .as_bytes()
        );
    }
    let output = cli(&["--help".as_ref()]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}
