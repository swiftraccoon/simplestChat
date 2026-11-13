//! The executable's own answers: where each line goes and how it exits.

use std::{
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-security"))
        .args(arguments)
        .env_clear()
        .output()
        .expect("the executable runs")
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8 output")
}

/// A directory of one test's own, removed with it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-security-cli-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_command_that_is_not_one_is_told_the_usage() {
    for arguments in [
        &[][..],
        &["tools"],
        &["tools", "remove"],
        &["tools", "path"],
        &["install"],
        &["scan", "fast"],
        &["scanners", "path", "no-such-tool"],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(text(&output.stderr).starts_with("usage: simplestchat-security tools "));
    }
}

#[test]
fn a_refusal_is_one_line_with_its_code_and_status_one() {
    let output = run(&["tools", "path", "no-such-tool"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        text(&output.stderr),
        "Security tool setup failed: unknown_tool\n"
    );
    let output = run(&[
        "tools",
        "install",
        "--tools",
        "gh",
        "--platform",
        "plan9-mips",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "Security tool setup failed: unsupported_tool_platform\n"
    );
}

/// An installation of one tool of the committed lock, written by hand: the
/// receipt names the lock, the tool's version and archive, and the file here.
fn installation(root: &Path, tool: &str, platform: &str) {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/security-tools.lock.json");
    let lock = std::fs::read(lock).expect("the committed lock");
    let parsed: serde_json::Value = serde_json::from_slice(&lock).expect("lock");
    let entry = &parsed["tools"][tool];
    let asset = &entry["platforms"][platform];
    let hex = |bytes: &[u8]| {
        use sha2::Digest as _;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let content = b"fixture executable\n";
    let executable = entry["executable"].as_str().expect("executable");
    let receipt = serde_json::json!({
        "schemaVersion": 1,
        "lockSha256": hex(&lock),
        "platform": platform,
        "tools": {tool: {
            "version": entry["version"],
            "archiveSha256": asset["sha256"],
            "files": {executable: hex(content)},
        }},
    });
    let write = |path: PathBuf, bytes: &[u8], mode: u32| {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .and_then(|mut file| file.write_all(bytes))
            .expect("fixture file");
    };
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join("bin"))
        .expect("bin");
    write(root.join("bin").join(executable), content, 0o700);
    write(
        root.join("receipt.json"),
        receipt.to_string().as_bytes(),
        0o600,
    );
}

#[test]
fn a_verified_installation_is_answered_with_its_paths() {
    let scratch = Scratch::new("verified");
    let root = scratch.0.to_str().expect("a UTF-8 path");
    let common = ["--directory", root, "--platform", "linux-x86_64"];
    let output = run(&[&["tools", "path", "actionlint"][..], &common].concat());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "Security tool setup failed: tool_entry_missing\n"
    );
    installation(&scratch.0, "actionlint", "linux-x86_64");
    let output = run(&[&["tools", "path", "actionlint"][..], &common].concat());
    assert_eq!(text(&output.stderr), "");
    assert_eq!(text(&output.stdout), format!("{root}/bin/actionlint\n"));
    assert_eq!(output.status.code(), Some(0));
    // An existing directory is verified for the tools asked for, never fetched.
    let output = run(&[&["tools", "install", "--tools", "actionlint"][..], &common].concat());
    assert_eq!(text(&output.stdout), format!("{root}\n"));
    assert_eq!(output.status.code(), Some(0));
    let output = run(&[&["tools", "install", "--tools", "gh"][..], &common].concat());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "Security tool setup failed: tool_not_installed\n"
    );
}
