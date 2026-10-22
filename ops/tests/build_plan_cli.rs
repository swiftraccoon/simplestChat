//! Actual CLI against immutable owned synthetic inputs, with empty PATH. The
//! recipe cannot build these files; no candidate or second command is executed.

#[path = "../src/distribution_build_fixture.rs"]
mod fixture;
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-plan-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        fixture::populate(&path.join("source"));
        fs::write(
            path.join("selection.json"),
            serde_json::to_vec(
                &serde_json::json!({"repository":"owner/repo","revision":"a".repeat(40),
                "packageVersion":"0.1.0","protocolVersion":1,
                "target":"x86_64-unknown-linux-musl","buildRunId":123,"runAttempt":1}),
            )
            .unwrap(),
        )
        .unwrap();
        Self(path)
    }
    fn run(&self) -> Output {
        cli(&[
            "distribution".as_ref(),
            "build-plan".as_ref(),
            "--selection".as_ref(),
            self.0.join("selection.json").as_os_str(),
            "--source-root".as_ref(),
            self.0.join("source").as_os_str(),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(args)
        .env("PATH", "")
        .output()
        .unwrap()
}

#[test]
fn nonexecuting_plan_projects_only_bounded_claims_inventory_and_unresolved_slots() {
    let root = Fixture::new();
    let source_before = fs::read(root.0.join("source/ops/src/main.rs")).unwrap();
    let selection_before = fs::read(root.0.join("selection.json")).unwrap();
    let output = root.run();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["kind"], "ops-build-plan");
    assert_eq!(value["inventory"]["files"], 11);
    assert_eq!(value["recipe"]["program"], "cargoExecutable");
    assert_eq!(value["recipe"]["arguments"][1]["value"], "--frozen");
    let public = String::from_utf8(output.stdout).unwrap();
    for hidden in [
        "PRIVATE-INPUT",
        root.0.to_str().unwrap(),
        "passed",
        "binarySha256",
    ] {
        assert!(!public.contains(hidden));
    }
    assert_eq!(
        fs::read(root.0.join("source/ops/src/main.rs")).unwrap(),
        source_before
    );
    assert_eq!(
        fs::read(root.0.join("selection.json")).unwrap(),
        selection_before
    );
    assert!(!root.0.join("source/target").exists());
}

#[test]
fn input_failures_have_one_fixed_private_projection() {
    for file in ["selection.json", "source/Cargo.lock"] {
        let root = Fixture::new();
        fs::remove_file(root.0.join(file)).unwrap();
        let output = root.run();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            b"{\"passed\":false,\"failure\":\"invalid_ops_build_plan\"}\n"
        );
    }
}

#[test]
fn invalid_argument_order_is_usage_only() {
    let output = cli(&[
        "distribution".as_ref(),
        "build-plan".as_ref(),
        "--source-root".as_ref(),
        "PRIVATE".as_ref(),
        "--selection".as_ref(),
        "PRIVATE".as_ref(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("PRIVATE")
    );
}
