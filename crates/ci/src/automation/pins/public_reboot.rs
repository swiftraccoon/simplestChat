//! The reboot's runtime upgrade preflight: the candidate Compose is
//! downloaded and authenticated by its checksum, and the installed Ops
//! executable compares it with the installed Compose before any service
//! stops, without printing the resolved configuration it compares. The
//! comparison itself is replayed against the recorded answers of the
//! program it replaced in `ops/src/reboot/compose_tests.rs`. Nothing
//! contacts Docker.

use super::ansible::evaluate;
use super::document;
use super::monitoring::{OPS, environment, named, play, section};
use serde_json::json;

const PREFLIGHT: &str = "ops/ansible/tasks/runtime-upgrade-preflight.yml";
/// Where the candidate is kept for the comparison and removed afterwards.
const CANDIDATE: &str = "/usr/local/libexec/simplestchat-public/compose-upgrade-candidate";
const VERSION: &str = "{{ scbench_docker_compose_version.split('-')[0] }}";

/// The candidate is authenticated before it runs and compared by the
/// installed executable with the version its package selects; the step's
/// answer is never shown, a difference fails it, and the candidate is
/// removed whatever happened.
#[test]
fn a_runtime_upgrade_compares_compose_through_the_installed_ops_executable() {
    let tasks = document(PREFLIGHT);
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    let download = &tasks[0]["ansible.builtin.get_url"];
    assert_eq!(download["dest"], CANDIDATE);
    assert_eq!(
        download["checksum"],
        "sha256:{{ scbench_docker_compose_binary_sha256 }}"
    );
    assert_eq!(
        (&download["owner"], &download["mode"]),
        (&json!("root"), &json!("0700"))
    );
    let compared = &tasks[1];
    assert_eq!(
        compared["ansible.builtin.command"],
        json!({"argv": [OPS, "reboot", "compare-compose", CANDIDATE, VERSION]})
    );
    assert_eq!(compared["environment"], environment());
    assert_eq!(compared["changed_when"], false);
    assert_eq!(compared["no_log"], true);
    for softened in ["failed_when", "ignore_errors", "when"] {
        assert!(compared.get(softened).is_none(), "{softened}");
    }
    let selected = evaluate(
        &[&VERSION[3..VERSION.len() - 3]],
        &[json!({"scbench_docker_compose_version": "5.6.0-1~debian.13~trixie"})],
    );
    assert_eq!(selected, [[json!("5.6.0")]]);
    let text = super::checkout_file(PREFLIGHT);
    assert!(!text.contains("python") && !text.contains(".py"));
    // The reboot runs the comparison before the workflow that stops
    // services, and always removes the candidate.
    let reboot = play("ops/ansible/reboot.yml");
    let steps = section(&reboot, "tasks");
    let first = named(
        steps,
        "Verify runtime upgrade compatibility before stopping any services",
    );
    assert_eq!(
        first["ansible.builtin.include_tasks"],
        "tasks/runtime-upgrade-preflight.yml"
    );
    let workflow = named(
        steps,
        "Prepare, reboot once and recover the same public containers",
    );
    let removed = &section(workflow, "always")[0];
    assert_eq!(
        removed["ansible.builtin.file"],
        json!({"path": CANDIDATE, "state": "absent"})
    );
}
