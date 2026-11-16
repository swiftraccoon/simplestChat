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

#[test]
fn an_elf_audit_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["elf"][..],
        &["elf", "--rootfs", "/nowhere"],
        &[
            "elf",
            "--rootfs",
            "/nowhere",
            "--output",
            "/nowhere/report",
            "--platform",
            "linux/riscv64",
        ],
        &[
            "elf",
            "--rootfs",
            "/nowhere",
            "--output",
            "/nowhere/report",
            "more",
        ],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        assert!(
            usage.starts_with("usage: simplestchat-security elf --rootfs DIR "),
            "{usage}"
        );
    }
}

#[test]
fn an_elf_audit_of_no_image_is_reported_and_ends_with_one() {
    let scratch = Scratch::new("elf");
    let report = scratch.0.join("report.json");
    let (missing, named) = (scratch.0.join("missing"), report.to_str().expect("a path"));
    let arguments = [
        "elf",
        "--rootfs",
        missing.to_str().expect("a path"),
        "--output",
        named,
    ];
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!((text(&output.stdout), text(&output.stderr)), ("", ""));
    let written = std::fs::read_to_string(&report).expect("a report");
    assert_eq!(
        written,
        "{\n  \"passed\": false,\n  \"reason\": \"image_root_invalid\"\n}\n"
    );
    let again = run(&arguments);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(
        text(&again.stderr),
        "ELF audit failed: tool_entry_unavailable\n"
    );
    assert_eq!(std::fs::read_to_string(&report).expect("a report"), written);
}

#[test]
fn a_vendor_check_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["vendor"][..],
        &["vendor", "verify"],
        &["vendor", "verify", "--cache", "/nowhere/cache"],
        &["vendor", "_download", "--url", "https://example.invalid/"],
        &[
            "vendor",
            "verify",
            "--cache",
            "/nowhere/cache",
            "--output",
            "/nowhere/evidence",
            "more",
        ],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        assert!(
            usage.starts_with("usage: simplestchat-security vendor verify --cache DIR "),
            "{usage}"
        );
    }
}

#[test]
fn a_vendor_check_that_fails_says_what_is_wrong_in_one_line() {
    let scratch = Scratch::new("vendor");
    let path = |name: &str| scratch.0.join(name);
    let (root, cache, evidence) = (path("root"), path("cache"), path("evidence"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("a checkout");
    let named = |path: &Path| path.to_str().expect("a path").to_owned();
    let arguments = [
        "vendor".to_owned(),
        "verify".to_owned(),
        "--offline".to_owned(),
        "--root".to_owned(),
        named(&root),
        "--cache".to_owned(),
        named(&cache),
        "--output".to_owned(),
        named(&evidence),
    ];
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let real = root.canonicalize().expect("the checkout");
    let line = format!(
        "Vendor integrity failed: Not a source directory: {}/vendor\n",
        real.display()
    );
    assert_eq!(text(&output.stderr), line);
    // The cache and the evidence were made, private, and nothing is in them.
    for made in [&cache, &evidence] {
        use std::os::unix::fs::PermissionsExt as _;
        let held = made.metadata().expect("a directory");
        assert_eq!(held.permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_dir(made).expect("entries").count(), 0);
    }
    // Evidence is never written over.
    let again = run(&arguments);
    assert_eq!(again.status.code(), Some(1));
    let line = format!(
        "Vendor integrity failed: Directory cannot be made: {}\n",
        evidence.display()
    );
    assert_eq!(text(&again.stderr), line);
}

#[test]
fn a_vendor_check_that_may_fetch_nothing_asks_for_nothing() {
    use sha2::Digest as _;
    let scratch = Scratch::new("vendor-offline");
    let path = |name: &str| scratch.0.join(name);
    let vendor = path("root/vendor");
    std::fs::create_dir_all(&vendor).expect("a vendored directory");
    let license = b"license\n";
    let digest: String = sha2::Sha256::digest(license)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let record = serde_json::json!({
        "sources": [{
            "id": "license",
            "url": "https://example.invalid/license",
            "sha256": digest,
            "format": "file",
        }],
        "trees": [],
        "files": [{"path": "vendor/LICENSE", "source": "license"}],
        "maintained_files": ["vendor/README.md"],
        "wraps": [],
    });
    for (name, content) in [
        ("integrity.json", record.to_string().as_bytes()),
        ("LICENSE", &license[..]),
        ("README.md", b"A vendored directory.\n"),
    ] {
        std::fs::write(vendor.join(name), content).expect("a file");
    }
    let named = |name: &str| path(name).to_str().expect("a path").to_owned();
    let (root, cache) = (named("root"), named("cache"));
    let check = |evidence: &str, more: &[&str]| {
        let evidence = named(evidence);
        let mut arguments = vec!["vendor", "verify", "--root", &root, "--cache", &cache];
        arguments.extend(["--output", &evidence]);
        arguments.extend(more);
        run(&arguments)
    };
    // The source is not in the cache, and nothing may be fetched.
    let output = check("first", &["--offline"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "Vendor integrity failed: Offline source missing: license\n"
    );
    // With the source in the cache, the directory is what its record says.
    std::fs::write(path("cache").join(&digest), license).expect("a source");
    let output = check("second", &["--offline"]);
    assert_eq!((text(&output.stdout), text(&output.stderr)), ("", ""));
    assert_eq!(output.status.code(), Some(0));
    let report = std::fs::read_to_string(path("second/report.json")).expect("a report");
    assert!(
        report.starts_with("{\n  \"diff_sha256\": \"e3b0c442"),
        "{report}"
    );
    assert_eq!(
        std::fs::read(path("second/vendor.diff")).expect("differences"),
        b""
    );
    // And so it is when sources may be fetched: none is asked for.
    let output = check("third", &[]);
    assert_eq!((text(&output.stdout), text(&output.stderr)), ("", ""));
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn native_evidence_that_is_not_asked_for_properly_is_told_the_usage() {
    let all = [
        "--vendor-report",
        "r",
        "--cargo-messages",
        "m",
        "--cargo-home",
        "h",
        "--openssl-prefix",
        "o",
        "--aws-lc-prefix",
        "a",
        "--output",
        "out",
    ];
    fn joined<'a>(all: &[&'a str], more: &[&'a str]) -> Vec<&'a str> {
        [&["native"][..], all, more].concat()
    }
    let with = |more: &[&'static str]| joined(&all, more);
    for arguments in [
        vec!["native"],
        [&["native"][..], &all[..10]].concat(),
        [&["native"][..], &all[2..]].concat(),
        with(&["more"]),
        with(&["--output", "again"]),
        with(&["--root"]),
        with(&["--root", ""]),
        with(&["--root", "--output"]),
        with(&["--system-root", "/usr"]),
    ] {
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        assert!(
            usage.starts_with("usage: simplestchat-security native --vendor-report FILE "),
            "{usage}"
        );
    }
}

#[test]
fn native_evidence_that_fails_says_what_is_wrong_in_one_line_and_writes_nothing() {
    let scratch = Scratch::new("native");
    let path = |name: &str| scratch.0.join(name).to_str().expect("a path").to_owned();
    let (root, report) = (path("root"), path("report.json"));
    std::fs::create_dir(&root).expect("a checkout");
    let arguments = [
        "native",
        "--root",
        &root,
        "--vendor-report",
        &path("vendor-evidence/report.json"),
        "--cargo-messages",
        &path("cargo-build.json"),
        "--cargo-home",
        &path("cargo-home"),
        "--openssl-prefix",
        &path("openssl"),
        "--aws-lc-prefix",
        &path("aws-lc"),
        "--output",
        &report,
    ];
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let real = Path::new(&root).canonicalize().expect("the checkout");
    let line = format!(
        "Native provenance failed: Unreadable file: {}/vendor/native-components.json\n",
        real.display()
    );
    assert_eq!(text(&output.stderr), line);
    assert!(!Path::new(&report).exists());
}

#[cfg(feature = "image")]
#[test]
fn an_image_archive_check_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["image"][..],
        &["image", "archive"],
        &[
            "image",
            "extract",
            "--archive",
            "a",
            "--manifest",
            "m",
            "--output",
            "o",
        ],
        &["image", "archive", "--archive", "a", "--manifest", "m"],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "m",
            "--output",
        ],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "m",
            "--output",
            "",
        ],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "--output",
            "o",
        ],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "-m",
            "--output",
            "o",
        ],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "m",
            "--output",
            "o",
            "--output",
            "p",
        ],
        &[
            "image",
            "archive",
            "--archive",
            "a",
            "--manifest",
            "m",
            "--output",
            "o",
            "more",
        ],
        &[
            "image",
            "archive",
            "--image",
            "a",
            "--manifest",
            "m",
            "--output",
            "o",
        ],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        assert!(
            usage.starts_with("usage: simplestchat-security image archive --archive FILE "),
            "{usage}"
        );
    }
}

#[cfg(feature = "image")]
#[test]
fn an_image_scan_or_preparation_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["image", "scan"][..],
        &[
            "image",
            "scan",
            "sha256:a",
            "--output",
            "o",
            "--tools-directory",
            "t",
        ],
        &[
            "image",
            "scan",
            "sha256:a",
            "--artifact-dir",
            "a",
            "--output",
            "o",
        ],
        &[
            "image",
            "scan",
            "--artifact-dir",
            "a",
            "--output",
            "o",
            "--tools-directory",
            "t",
        ],
        &["image", "prepare-database"],
        &[
            "image",
            "prepare-database",
            "--output",
            "o",
            "--tools-directory",
            "t",
        ],
        &[
            "image",
            "prepare-database",
            "--output",
            "o",
            "--database-cache",
            "c",
        ],
        &["image", "prepare-database", "sha256:a", "--output", "o"],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        let expected = "usage: simplestchat-security image scan IMAGE --artifact-dir DIR ";
        assert!(usage.starts_with(expected), "{usage}");
        assert!(usage.contains("\n       simplestchat-security image prepare-database "));
    }
}

#[cfg(feature = "image")]
#[test]
fn an_image_tier_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["image", "check"][..],
        &["image", "check", "--output", "o"],
        &["image", "check", "sha256:a", "sha256:b"],
        &["image", "check", "sha256:a", "--engine", "rkt"],
        &["image", "check", "sha256:a", "--image-tools"],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = text(&output.stderr);
        let expected = "usage: simplestchat-security image check IMAGE [--output DIR] ";
        assert!(usage.starts_with(expected), "{usage}");
    }
}

#[cfg(feature = "image")]
#[test]
fn a_cache_selector_that_is_not_asked_for_properly_is_told_the_usage() {
    for arguments in [
        &["image", "cache", "--status"][..],
        &["image", "cache", "--prefix", "more"],
        &["image", "cache", "--key"],
        &["image", "cache", "--status", "a", "b"],
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(text(&output.stdout), "", "{arguments:?}");
        let usage = "usage: simplestchat-security image cache [--prefix | --status FILE]\n";
        assert_eq!(text(&output.stderr), usage, "{arguments:?}");
    }
}

/// The selector of the database cache, run with only the named variables.
#[cfg(feature = "image")]
fn selector(arguments: &[&str], variables: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simplestchat-security"));
    command.args(arguments).env_clear();
    for (name, value) in variables {
        command.env(name, value);
    }
    command.output().expect("the executable runs")
}

#[cfg(feature = "image")]
#[test]
fn a_cache_prefix_is_of_the_locked_scanner_and_of_the_trust_of_the_run() {
    let untrusted = selector(&["image", "cache", "--prefix"], &[]);
    assert_eq!(untrusted.status.code(), Some(0));
    let line = text(&untrusted.stdout);
    let pin = line.strip_prefix("grype-db-v2-untrusted-");
    let pin = pin
        .and_then(|rest| rest.strip_suffix("-\n"))
        .expect("a prefix");
    let small = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    assert!(pin.len() == 64 && pin.bytes().all(small), "{line}");
    let main = [
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_EVENT_NAME", "push"),
    ];
    let trusted = selector(&["image", "cache", "--prefix"], &main);
    assert_eq!(text(&trusted.stdout), format!("grype-db-v2-main-{pin}-\n"));
    let pull = [
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_EVENT_NAME", "pull_request"),
    ];
    let proposed = selector(&["image", "cache", "--prefix"], &pull);
    assert_eq!(text(&proposed.stdout), line);
}

#[cfg(feature = "image")]
#[test]
fn a_cache_key_is_of_the_generation_that_the_update_reported() {
    let scratch = Scratch::new("cache");
    let status = scratch.0.join("status.json");
    let source = format!(
        "https://grype.anchore.io/databases/v6/db.tar.zst?checksum=sha256%3A{}",
        "a".repeat(64)
    );
    let written = format!("{{\"from\": \"{source}\"}}");
    std::fs::write(&status, written).expect("a status");
    let named = status.to_str().expect("a path");
    let prefix = selector(&["image", "cache", "--prefix"], &[]);
    let output = selector(&["image", "cache", "--status", named], &[]);
    assert_eq!(output.status.code(), Some(0));
    let expected = format!("{}{}\n", text(&prefix.stdout).trim_end(), "a".repeat(64));
    assert_eq!(text(&output.stdout), expected);
    let missing = selector(&["image", "cache", "--status", "/nowhere/status.json"], &[]);
    assert_eq!(missing.status.code(), Some(1));
    let refused = text(&missing.stderr);
    assert!(
        refused.starts_with("Image database cache failed: "),
        "{refused}"
    );
    assert_eq!(text(&missing.stdout), "");
}

#[cfg(feature = "image")]
#[test]
fn an_image_archive_check_that_fails_says_its_code_in_one_line_and_writes_nothing() {
    let scratch = Scratch::new("image");
    let path = |name: &str| scratch.0.join(name).to_str().expect("a path").to_owned();
    let (image, manifest, evidence) = (path("image.tar"), path("release.json"), path("evidence"));
    let arguments = [
        "image",
        "archive",
        "--output",
        &evidence,
        "--manifest",
        &manifest,
        "--archive",
        &image,
    ];
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert_eq!(
        text(&output.stderr),
        "Image archive check failed: image_archive_file\n"
    );
    assert!(!Path::new(&evidence).exists());
    // An archive that ends at once has no end blocks.
    std::fs::write(&image, b"").expect("an archive");
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "Image archive check failed: image_tar_terminator\n"
    );
    assert!(!Path::new(&evidence).exists());
}
