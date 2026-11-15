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
        let usage = text(&output.stderr);
        let whole = usage.starts_with("usage: simplestchat-security fast|deep|tools|rulesets ");
        let own = usage.starts_with("usage: simplestchat-security tools ");
        assert!(whole || own, "{arguments:?}: {usage}");
        assert_eq!(whole, !matches!(arguments, ["tools", ..]), "{arguments:?}");
    }
}

#[test]
fn a_rulesets_command_that_is_not_one_is_told_the_usage_and_asks_nobody() {
    let usage = "usage: simplestchat-security rulesets plan|check|apply [--revision COMMIT]\n";
    for arguments in [
        &["rulesets"][..],
        &["rulesets", "remove"],
        &["rulesets", "plan", "check"],
        &["rulesets", "apply", "--revision"],
        &["rulesets", "plan", "--rev", "x"],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert_eq!(text(&output.stderr), usage, "{arguments:?}");
    }
    let output = run(&["rulesets", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), usage);
    assert!(output.stderr.is_empty());
}

#[test]
fn a_rulesets_command_without_its_program_fails_with_one_line() {
    // The maintained policy is read first; then no directory of the search
    // path holds the program that asks GitHub, so nobody is asked.
    let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-security"))
        .args(["rulesets", "check"])
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .expect("the executable runs");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        text(&output.stderr),
        "Ruleset reconciliation failed: missing_security_prerequisite_gh\n"
    );
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

fn gate(arguments: &[&str], variables: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simplestchat-security"));
    command.args(arguments).env_clear().env("CI", "1");
    for (name, value) in variables {
        command.env(name, value);
    }
    command.output().expect("the executable runs")
}

#[test]
fn a_tier_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["fast", "--quick"][..],
        &["deep", "--deep-check"],
        &["fast", "an-image"],
        &["deep", "--engine", "nerdctl"],
    ] {
        let output = gate(arguments, &[]);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let complaint = text(&output.stderr);
        assert!(
            complaint.starts_with("usage: simplestchat-security fast|deep "),
            "{complaint}"
        );
    }
}

#[test]
fn a_tier_that_is_refused_says_so_in_one_line_and_leaves_its_summary() {
    let scratch = Scratch::new("tier");
    let evidence = scratch.0.join("gate");
    let named = evidence.to_str().expect("a path");
    let home = [("HOME", scratch.0.as_path())];
    let output = gate(&["fast", "--include-vendor", "--output", named], &home);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stderr), "");
    let real = evidence.canonicalize().expect("the evidence");
    let line = format!("Security fast: failed; evidence: {}\n", real.display());
    assert_eq!(text(&output.stdout), line);
    let summary = std::fs::read_to_string(evidence.join("summary.json")).expect("a summary");
    assert!(summary.contains("\"failure\": \"vendor_source_scan_local_only\""));
    let again = gate(&["fast", "--include-vendor", "--output", named], &home);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(text(&again.stdout), "");
    assert_eq!(
        text(&again.stderr),
        "Security gate failed: tool_entry_unavailable\n"
    );
    let other = scratch.0.join("other");
    let named = other.to_str().expect("a path");
    let homeless = gate(&["deep", "--include-vendor", "--output", named], &[]);
    assert_eq!(homeless.status.code(), Some(1));
    assert_eq!(text(&homeless.stdout), "");
    assert_eq!(
        text(&homeless.stderr),
        "Security gate failed: security_home_unset\n"
    );
}
