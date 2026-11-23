//! The reboot's runtime upgrade preflight, carried over from the Python
//! reboot tests: the playbook's own comparison of a candidate Compose with
//! the installed one runs against answers given by fake commands, and every
//! difference stops it without printing the resolved configuration. Nothing
//! contacts Docker.

use super::ansible::Scratch;
use super::document;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const PREFLIGHT: &str = "ops/ansible/tasks/runtime-upgrade-preflight.yml";
/// The installed Compose, as the comparison names it.
const INSTALLED: &str = "\"/usr/bin/docker\"";
/// The candidate version the comparison is given.
const VERSION: &str = "5.6.0";
/// A value of the resolved configuration that must never be printed.
const PRIVATE: &str = "private fixture value";

/// The comparison's inline program, the third word of its command.
fn program() -> String {
    let tasks = document(PREFLIGHT);
    let argv = tasks[1]["ansible.builtin.command"]["argv"]
        .as_array()
        .unwrap();
    argv[2].as_str().unwrap().to_owned()
}

/// Write an executable POSIX shell command that answers as Compose does for
/// the comparison: `version` its version, `--hash` the identity of the
/// service it ends with (another one for `changed`), anything else the
/// resolved configuration with `value`, each ending with `status`.
fn fake(path: &Path, version: &str, changed: &str, value: &str, status: u8) {
    let script = format!(
        "#!/bin/sh\nfor last do :; done\nfor word do\n  case $word in\n    \
         version) printf '%s' '{version}'; exit {status};;\n    \
         --hash)\n      if [ \"$last\" = '{changed}' ]; then printf '%s' changed\n      \
         else printf '%s identity' \"$last\"; fi\n      exit {status};;\n  esac\ndone\n\
         printf '%s' '{{\"environment\": {{\"SECRET\": \"{value}\"}}}}'\nexit {status}\n"
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// From `test_compose_upgrade_refuses_changed_configuration_hash_or_version`:
/// the actual preflight accepts a candidate only of the selected version
/// that resolves the same configuration and the same identity of each of
/// the three services, and never prints the configuration it compares.
#[test]
fn compose_upgrade_refuses_changed_configuration_hash_or_version() {
    let program = program();
    assert_eq!(program.matches(INSTALLED).count(), 1);
    let identity = "Candidate Compose changes a service identity; separate migration required\n";
    for (changed, said) in [
        (
            "none",
            "Candidate Compose preserves configuration and all three service identities\n",
        ),
        (
            "configuration",
            "Candidate Compose changes resolved configuration; separate migration required\n",
        ),
        ("postgres", identity),
        ("simplestchat", identity),
        ("caddy", identity),
        (
            "version",
            "Candidate Compose does not match the selected package\n",
        ),
        (
            "command",
            "Compose compatibility command failed; no services were stopped\n",
        ),
    ] {
        let scratch = Scratch::new();
        let (installed, candidate) = (
            scratch.path().join("installed"),
            scratch.path().join("candidate"),
        );
        let version = if changed == "version" {
            "unexpected"
        } else {
            VERSION
        };
        fake(&installed, version, "", PRIVATE, 0);
        let value = if changed == "configuration" {
            "changed"
        } else {
            PRIVATE
        };
        let status = u8::from(changed == "command");
        fake(&candidate, version, changed, value, status);
        let named = format!("\"{}\"", installed.display());
        assert!(!named.trim_matches('"').contains(['"', '\\']));
        let output = Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg(program.replace(INSTALLED, &named))
            .arg(&candidate)
            .arg(VERSION)
            .output()
            .unwrap();
        let (printed, refused) = (
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        if changed == "none" {
            assert!(output.status.success(), "{changed}");
            assert_eq!((printed.as_ref(), refused.as_ref()), (said, ""));
        } else {
            assert_eq!(output.status.code(), Some(1), "{changed}");
            assert_eq!((printed.as_ref(), refused.as_ref()), ("", said));
        }
        assert!(!printed.contains(PRIVATE) && !refused.contains(PRIVATE));
    }
}
