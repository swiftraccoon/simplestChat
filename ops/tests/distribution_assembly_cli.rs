//! Only the policy tool under test runs. Candidate files are inert ELF metadata.

#[path = "../src/distribution_fixture.rs"]
mod elf;
#[path = "../src/distribution_manifest_fixture.rs"]
mod fixture;

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-assembly-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let binary = elf::fixture(true, false, false);
        let evidence = b"PRIVATE-PAYLOAD no build or signature claim\n";
        fs::write(path.join("candidate"), &binary).unwrap();
        fs::write(path.join("evidence.bin"), evidence).unwrap();
        let mut manifest = fixture::manifest(&binary);
        manifest["evidenceDigests"] = json!({"PRIVATE-NAME/build.json":fixture::digest(evidence)});
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            path.join("selection.json"),
            serde_json::to_vec(&fixture::selection()).unwrap(),
        )
        .unwrap();
        fs::write(
            path.join("mapping.json"),
            serde_json::to_vec(&json!({
                "PRIVATE-NAME/build.json":path.join("evidence.bin"),
            }))
            .unwrap(),
        )
        .unwrap();
        Self(path)
    }
    fn run(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(["distribution", "prepare-artifact", "--manifest"])
            .arg(self.0.join("manifest.json"))
            .arg("--path")
            .arg(self.0.join("candidate"))
            .arg("--selection")
            .arg(self.0.join("selection.json"))
            .arg("--evidence")
            .arg(self.0.join("mapping.json"))
            .arg("--output")
            .arg(self.0.join("output"))
            .env("PATH", "")
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn refused(result: Output) {
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stderr.is_empty());
    assert_eq!(
        result.stdout,
        b"{\"passed\":false,\"failure\":\"invalid_ops_preparation\"}\n"
    );
}

#[test]
fn cli_only_writes_a_new_private_unsigned_artifact_and_fixed_public_projection() {
    let root = Fixture::new();
    let binary = fs::read(root.0.join("candidate")).unwrap();
    let manifest = fs::read(root.0.join("manifest.json")).unwrap();
    let result = root.run();
    assert!(result.status.success(), "{:?}", result.stderr);
    assert!(result.stderr.is_empty());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["kind"], "ops-unsigned-artifact");
    assert_eq!(value["evidenceCount"], 1);
    assert_eq!(value["manifestSha256"], fixture::digest(&manifest));
    assert_eq!(value["manifestBytes"], manifest.len());
    let public = String::from_utf8(result.stdout).unwrap();
    for secret in [
        "PRIVATE-PAYLOAD",
        "PRIVATE-NAME",
        "passed",
        root.0.to_str().unwrap(),
    ] {
        assert!(!public.contains(secret));
    }
    assert_eq!(
        fs::read(root.0.join("output/ops-release.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(root.0.join("output/simplestchat-ops")).unwrap(),
        binary
    );
    assert_eq!(fs::read(root.0.join("candidate")).unwrap(), binary);
    refused(root.run());
}

#[test]
fn bad_selection_mapping_candidate_and_output_fail_privately_without_final_record() {
    for kind in ["selection", "mapping", "evidence", "binary", "output-link"] {
        let root = Fixture::new();
        match kind {
            "selection" => {
                let mut selected = fixture::selection();
                selected["revision"] = "b".repeat(40).into();
                fs::write(
                    root.0.join("selection.json"),
                    serde_json::to_vec(&selected).unwrap(),
                )
                .unwrap();
                fs::remove_file(root.0.join("candidate")).unwrap();
            }
            "mapping" => fs::write(root.0.join("mapping.json"), b"{\"PRIVATE\":false}").unwrap(),
            "evidence" => fs::write(root.0.join("evidence.bin"), b"changed").unwrap(),
            "binary" => fs::write(root.0.join("candidate"), b"changed").unwrap(),
            _ => symlink(root.0.join("unrelated"), root.0.join("output")).unwrap(),
        }
        refused(root.run());
        assert!(!root.0.join("output/ops-preparation.json").exists());
        assert!(!root.0.join("unrelated").exists());
    }
}

#[test]
fn invalid_argument_order_is_usage_only_and_cannot_choose_another_operation() {
    let result = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(["distribution", "prepare-artifact", "--output", "PRIVATE"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let usage = String::from_utf8(result.stderr).unwrap();
    assert!(usage.contains("distribution prepare-artifact --manifest PATH"));
    assert!(!usage.contains("PRIVATE"));
}
