//! Actual CLI admission with no environment, tools or existing host state.

use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-capacity-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(arguments)
            .current_dir(&self.0)
            .env_clear()
            .env("PATH", "")
            .output()
            .unwrap()
    }
    fn untouched(&self) {
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn capacity_help_uses_the_ops_program_without_selecting_an_engine_or_host() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["capacity", "--help"],
        vec!["capacity", "run", "--help"],
    ] {
        let output = fixture.run(&arguments);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("simplestchat-ops capacity run"));
        assert!(!text.contains("load_test"));
        fixture.untouched();
    }
}

#[test]
fn invalid_host_shapes_and_offline_commands_refuse_before_any_host_operation() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["capacity", "suggest", "--vcpus", "PRIVATE-INVOCATION-DATA"],
        vec![
            "capacity-vps",
            "serve",
            "--root",
            "PRIVATE-INVOCATION-DATA",
            "--transport-json",
            "PRIVATE-INVOCATION-DATA",
        ],
        vec![
            "capacity-vps",
            "worker",
            "--run",
            "PRIVATE-INVOCATION-DATA",
            "--root",
            "PRIVATE-INVOCATION-DATA",
        ],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.starts_with("Usage: simplestchat-ops "));
        assert!(!error.contains("PRIVATE-INVOCATION-DATA"));
        fixture.untouched();
    }
}
