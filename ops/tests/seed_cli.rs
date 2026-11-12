//! The lobby-seeding command takes no argument, and an unprivileged caller
//! gets one report line that names the refusal and touches nothing.
use std::process::{Command, Output};

fn ops(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn any_argument_is_a_usage_error_without_effects() {
    for arguments in [
        &["release", "seed-lobby", "--help"][..],
        &["release", "seed-lobby", "PRIVATE_VALUE"],
        &["release", "seed-lobby", "--config", "PRIVATE_VALUE"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("simplestchat-ops release seed-lobby\n"));
        assert!(!stderr.contains("PRIVATE_VALUE"), "{arguments:?}");
    }
}

#[test]
fn an_unprivileged_caller_gets_one_report_line_naming_the_refusal() {
    if rustix::process::geteuid().is_root() {
        // As root this would seed the machine's own deployment.
        return;
    }
    let output = ops(&["release", "seed-lobby"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            r#"{"schemaVersion":1,"passed":false,"changed":false,"ownerCreated":false,"#,
            r#""roomCreated":false,"ownerFileCreated":false,"existingSettingsPreserved":false,"#,
            r#""refreshSessionRevoked":null,"failure":"root_required","#,
            r#""ownerCredentialFile":"/etc/simplestchat-public/owner.json"}"#,
            "\n"
        )
    );
}
