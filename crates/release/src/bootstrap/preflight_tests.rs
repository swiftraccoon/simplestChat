//! Host facts against recorded answers; the two shell programs are checked
//! for syntax here and run for real only where the platform has their tools.

use super::super::ssh::tests::target;
use super::super::tests::Recorded;
use super::*;

fn answer(pairs: &[(&str, &str)]) -> String {
    let lines = pairs
        .iter()
        .map(|(name, value)| format!("{RECORD}{name}={value}"));
    format!("Legal banner\n{}", lines.collect::<Vec<_>>().join("\n"))
}

fn debian(overrides: &[(&str, &str)]) -> String {
    let mut pairs = vec![
        ("distribution", "debian"),
        ("version", "13"),
        ("architecture", "x86_64"),
        ("kernel", "6.12.48+deb13-amd64"),
        ("cpus", "6"),
        ("freeBytes", "42949672960"),
        ("freeInodes", "5000000"),
        ("systemd", "true"),
        ("passwordlessSudo", "true"),
        ("dockerInstalled", "false"),
        ("python", "3.13.5"),
    ];
    for (name, value) in overrides {
        let slot = pairs.iter_mut().find(|(known, _)| known == name).unwrap();
        slot.1 = value;
    }
    answer(&pairs)
}

fn idle() -> String {
    answer(&[("held", "false"), ("active", "false"), ("journal", "")])
}

fn code(result: Result<Value>) -> &'static str {
    result.unwrap_err().0
}

/// From `test_preflight_ignores_banner_and_requires_one_metadata_record`:
/// banners may precede the records, and every record appears exactly once.
#[test]
fn preflight_ignores_banners_and_requires_each_record_once() {
    let runner = Recorded::new(&[(0, &debian(&[])), (0, &idle())]);
    let facts = preflight(&runner, &target(22), 20).unwrap();
    assert_eq!(
        facts,
        json!({
            "distribution": "debian", "version": "13", "architecture": "x86_64",
            "cpus": 6, "freeBytes": 42_949_672_960u64, "freeInodes": 5_000_000,
            "kernel": "6.12.48+deb13-amd64", "systemd": true, "passwordlessSudo": true,
            "python": "3.13.5", "dockerInstalled": false,
        })
    );
    let calls = runner.calls();
    assert!(calls[0].last().unwrap().starts_with("sh -c 'set -eu"));
    assert!(
        calls[1]
            .last()
            .unwrap()
            .starts_with("sudo -n sh -c 'set -eu")
    );
    assert!(
        calls
            .iter()
            .all(|argv| argv.contains(&"BatchMode=yes".to_owned()))
    );
    let twice = format!("{}\n{RECORD}version=13", debian(&[]));
    let missing = answer(&[("distribution", "debian")]);
    let foreign = format!("{}\n{RECORD}extra=1", debian(&[]));
    let malformed = format!("{}\n{RECORD}novalue", debian(&[]));
    for text in [twice, missing, foreign, malformed] {
        let refused = preflight(&Recorded::new(&[(0, &text)]), &target(22), 20);
        assert_eq!(code(refused), "invalid_preflight_envelope");
    }
    let failed = preflight(&Recorded::new(&[(255, "")]), &target(22), 20);
    assert_eq!(code(failed), "remote_preflight_failed");
}

/// A value that is not a plain token, count or flag is refused, never kept.
#[test]
fn record_values_are_validated_before_they_become_evidence() {
    for (name, value) in [
        ("kernel", "6.12 $(reboot)"),
        ("distribution", "debian\"},{"),
        ("cpus", "six"),
        ("freeBytes", "-1"),
        ("systemd", "yes"),
        ("passwordlessSudo", ""),
    ] {
        let text = debian(&[(name, value)]);
        let refused = preflight(&Recorded::new(&[(0, &text)]), &target(22), 20);
        assert_eq!(code(refused), "invalid_preflight_envelope", "{name}");
    }
    let bare = debian(&[("python", ""), ("passwordlessSudo", "false")]);
    let facts = preflight(&Recorded::new(&[(0, &bare)]), &target(22), 20).unwrap();
    assert_eq!(facts["python"], Value::Null);
}

/// From `test_busy_capacity_journal_and_unsupported_os_block_provisioning`:
/// the read-only gates cover the platform, the disk and the shared ownership
/// journal of the capacity runner.
#[test]
fn busy_journal_and_unsupported_platforms_block_provisioning() {
    let encoded = |journal: &str| STANDARD.encode(journal);
    let finished = encoded(r#"{"schemaVersion":1,"operation":"capacity","finalized":true}"#);
    let unfinished = encoded(r#"{"schemaVersion":1,"operation":"capacity","finalized":false}"#);
    let future = encoded(r#"{"schemaVersion":2,"finalized":true}"#);
    let busy = |held: &str, active: &str, journal: &str| {
        answer(&[("held", held), ("active", active), ("journal", journal)])
    };
    for answer in [
        busy("true", "false", ""),
        busy("false", "true", ""),
        busy("false", "false", &unfinished),
        busy("false", "false", &future),
    ] {
        let runner = Recorded::new(&[(0, &debian(&[])), (0, &answer)]);
        let refused = preflight(&runner, &target(22), 20);
        assert_eq!(code(refused), "host_workload_active_or_unfinished");
    }
    let settled = Recorded::new(&[(0, &debian(&[])), (0, &busy("false", "false", &finished))]);
    preflight(&settled, &target(22), 20).unwrap();
    let garbled = Recorded::new(&[(0, &debian(&[])), (0, &busy("false", "false", "!!"))]);
    assert_eq!(
        code(preflight(&garbled, &target(22), 20)),
        "invalid_preflight_envelope"
    );
    for (overrides, expected) in [
        (&[("version", "12")][..], "unsupported_distribution"),
        (&[("distribution", "ubuntu")], "unsupported_distribution"),
        (&[("architecture", "aarch64")], "unsupported_host_platform"),
        (&[("systemd", "false")], "unsupported_host_platform"),
        (&[("freeBytes", "21474836479")], "insufficient_disk_space"),
    ] {
        let runner = Recorded::new(&[(0, &debian(overrides))]);
        assert_eq!(code(preflight(&runner, &target(22), 20)), expected);
    }
    // Without passwordless sudo the ownership check cannot run and is skipped.
    let unprivileged = Recorded::new(&[(0, &debian(&[("passwordlessSudo", "false")]))]);
    preflight(&unprivileged, &target(22), 20).unwrap();
    assert_eq!(unprivileged.calls().len(), 1);
}

/// Both programs are valid POSIX shell and pass ShellCheck for `dash`, the
/// shell a Debian host runs them with.
#[test]
fn host_programs_parse_and_pass_shellcheck() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    for program in [FACTS, BUSY] {
        for argv in [
            &["sh", "-n", "/dev/stdin"][..],
            &["shellcheck", "--shell=dash", "/dev/stdin"],
        ] {
            let mut child = Command::new(argv[0]);
            child.args(&argv[1..]).stdin(Stdio::piped());
            let mut child = child.stdout(Stdio::piped()).spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(program.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "{argv:?}\n{text}");
        }
    }
}

/// On Linux both programs run for real against this machine: each exits
/// zero and prints exactly its records, with values of the accepted shapes.
/// The ownership program needs systemd, as the host it is written for has.
#[cfg(target_os = "linux")]
#[test]
fn host_programs_run_on_linux_and_print_exactly_their_records() {
    let run = |program: &str| {
        let mut shell = std::process::Command::new("/bin/sh");
        let output = shell.args(["-c", program]).output().unwrap();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{errors}");
        String::from_utf8(output.stdout).unwrap()
    };
    let found = parse(&run(FACTS), &FACT_NAMES).unwrap();
    for name in [
        "distribution",
        "version",
        "architecture",
        "kernel",
        "python",
    ] {
        token(&found[name]).unwrap();
    }
    assert!(!found["distribution"].is_empty() && !found["architecture"].is_empty());
    assert!(count(&found["cpus"]).unwrap() >= 1);
    assert!(count(&found["freeBytes"]).unwrap() > 0);
    count(&found["freeInodes"]).unwrap();
    for name in ["systemd", "passwordlessSudo", "dockerInstalled"] {
        flag(&found[name]).unwrap();
    }
    if flag(&found["systemd"]).unwrap() && !std::path::Path::new("/run/simplestchat-bench").exists()
    {
        let found = parse(&run(BUSY), &["held", "active", "journal"]).unwrap();
        assert_eq!(found["held"], "false");
        assert_eq!(found["active"], "false");
        assert_eq!(found["journal"], "");
    }
}
