//! The runtime-profile and ledger commands answer one document or one
//! ledger with one fixed line, and never print what they were given.
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn ops(arguments: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn an_image_configuration_gets_one_fixed_line() {
    let held = br#"{"User":"10001:10001","WorkingDir":"/app","Cmd":["/app/simplestChat"],"Env":["PATH=/usr/bin"]}"#;
    let output = ops(&["release", "profile", "image"], held);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"{\"passed\": true}\n");
    assert!(output.stderr.is_empty());
    let overridden = br#"{"User":"10001:10001","WorkingDir":"/app","Cmd":["/app/simplestChat"],"Env":["LD_PRELOAD=/PRIVATE_VALUE.so"]}"#;
    let output = ops(&["release", "profile", "image"], overridden);
    assert_eq!(output.status.code(), Some(1));
    let line = "{\"passed\": false, \"failure\": \"runtime_environment_override\"}\n";
    assert_eq!(String::from_utf8(output.stdout).unwrap(), line);
    assert!(output.stderr.is_empty());
    let line = "{\"passed\": false, \"failure\": \"runtime_configuration_size\"}\n";
    // Nothing, and one byte more than a document may take.
    for input in [Vec::new(), vec![b' '; 2 * 1024 * 1024 + 1]] {
        let output = ops(&["release", "profile", "image"], &input);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), line);
    }
    // Exactly as much as a document may take is read and judged.
    let mut padded = vec![b' '; 2 * 1024 * 1024 - held.len()];
    padded.extend_from_slice(held);
    let output = ops(&["release", "profile", "image"], &padded);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_compose_model_is_judged_against_the_named_image() {
    let selected = format!("sha256:{}", "a".repeat(64));
    let output = ops(
        &["release", "profile", "compose", "--image-id", &selected],
        b"{}",
    );
    assert_eq!(output.status.code(), Some(1));
    let line = "{\"passed\": false, \"failure\": \"runtime_configuration\"}\n";
    assert_eq!(String::from_utf8(output.stdout).unwrap(), line);
    let output = ops(
        &["release", "profile", "compose", "--image-id", "latest"],
        br#"{"services":{}}"#,
    );
    assert_eq!(output.status.code(), Some(1));
    let line = "{\"passed\": false, \"failure\": \"runtime_image_selection\"}\n";
    assert_eq!(String::from_utf8(output.stdout).unwrap(), line);
}

#[test]
fn another_command_line_is_a_usage_error_before_anything_is_read() {
    let revision = "a".repeat(40);
    for arguments in [
        &["release", "profile"][..],
        &["release", "profile", "compose"],
        &["release", "profile", "image", "--image-id", "PRIVATE_VALUE"],
        &["release", "profile", "container"],
        &["release", "ledger"],
        &[
            "release",
            "ledger",
            "--revision",
            "PRIVATE_VALUE",
            "--ledger",
            "/absent",
        ],
        &[
            "release",
            "ledger",
            "--ledger",
            "/absent",
            "--revision",
            &revision,
        ],
    ] {
        let output = ops(arguments, b"PRIVATE_VALUE");
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("simplestchat-ops release profile image|compose --image-id ID\n"));
        assert!(stderr.contains("simplestchat-ops release ledger --revision REV --ledger PATH\n"));
        assert!(!stderr.contains("PRIVATE_VALUE"), "{arguments:?}");
    }
}

#[test]
fn a_ledger_that_cannot_be_read_or_has_no_checkout_is_refused_in_one_line() {
    let revision = "a".repeat(40);
    let absent = [
        "release",
        "ledger",
        "--revision",
        &revision,
        "--ledger",
        "/absent/ledger.txt",
    ];
    let output = ops(&absent, b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"Operational precondition failed\n");
    // This machine keeps no verified checkout of that revision.
    let empty = std::env::temp_dir().join(format!("ledger-cli-{}", std::process::id()));
    std::fs::write(&empty, b"").unwrap();
    let present = [
        "release",
        "ledger",
        "--revision",
        &revision,
        "--ledger",
        empty.to_str().unwrap(),
    ];
    let output = ops(&present, b"");
    std::fs::remove_file(&empty).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"Migration ledger differs from the verified source image\n"
    );
}
