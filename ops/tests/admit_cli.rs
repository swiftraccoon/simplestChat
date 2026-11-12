//! The unsigned admission takes exactly one named request, and a caller
//! without the prepared host's authority gets one fixed line and no effect.
use std::process::{Command, Output};

const REQUEST: &str = "/srv/simplestchat-public/forced/00000000000000000000000000000000/force.json";

fn ops(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn anything_but_one_named_request_is_a_usage_error_without_effects() {
    for arguments in [
        &["release", "admit-unsigned"][..],
        &["release", "admit-unsigned", "--help"],
        &["release", "admit-unsigned", "--request"],
        &["release", "admit-unsigned", "PRIVATE_VALUE"],
        &[
            "release",
            "admit-unsigned",
            "--request",
            REQUEST,
            "PRIVATE_VALUE",
        ],
        &["release", "admit-unsigned", "--force", "--request", REQUEST],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("simplestchat-ops release admit-unsigned --request ABS\n"));
        assert!(!stderr.contains("PRIVATE_VALUE"), "{arguments:?}");
    }
}

/// Unprivileged, the caller itself is refused; as root on a machine that is
/// not a prepared host, the request's place is. Either way: one fixed line.
#[test]
fn a_refused_admission_prints_one_fixed_line_and_nothing_else() {
    for arguments in [
        &["release", "admit-unsigned", "--request", REQUEST][..],
        &["release", "admit-unsigned", "--request=relative/force.json"],
        &["release", "admit-unsigned", "--request="],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "{\"passed\":false,\"failure\":\"force_release_failed\"}\n",
            "{arguments:?}"
        );
    }
}
