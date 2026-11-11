//! The relay playbook and its certificate timer. Certificate preparation and
//! refresh run through the installed Ops executable, on a host a signed
//! release has delivered it to. Nothing connects to a host.

use super::checkout_file;
use super::monitoring::{
    LOOK, OPS, REQUIRE, addressed, environment, named, names, play, requirement, section,
};
use serde_json::json;

const TURN: &str = "ops/ansible/turn.yml";
const UNIT: &str = "ops/ansible/templates/turn-certificate.service.j2";
const PREPARATION: &str = "Prepare the separately owned relay without advertising it to clients";

/// Every guard runs whatever tags are selected: the explicit opt-in, the
/// executable a signed release delivered, the host verified through it, and
/// the activation's own identity check.
#[test]
fn relay_steps_need_the_signed_ops_executable_and_a_verified_host() {
    let play = play(TURN);
    assert_eq!(play["serial"], 1);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [
            "Require the explicitly enabled relay on a prepared public host",
            LOOK,
            REQUIRE,
            "Verify the prepared public host before any relay step",
            "Require reviewed installed helpers and the selected relay identity before activation",
        ]
    );
    requirement(pre_tasks, "scpub_turn_ops");
    for task in pre_tasks {
        assert_eq!(task["tags"], json!(["always"]), "{}", task["name"]);
    }
    let preflight = &pre_tasks[3];
    assert_eq!(
        preflight["ansible.builtin.command"],
        json!({"argv": [OPS, "release", "preflight"]})
    );
    assert_eq!(preflight["environment"], environment());
    assert_eq!(preflight["changed_when"], false);
    assert_eq!(preflight["check_mode"], false);
    for softened in ["when", "failed_when", "ignore_errors"] {
        assert!(preflight.get(softened).is_none(), "{softened}");
    }
    assert_eq!(pre_tasks[4]["when"], "scpub_turn_activate | bool");
}

/// The certificate is published before the image is pulled and the relay
/// started, and the relay must present it before the timer is installed.
/// Both steps are the installed executable with a fixed environment; the
/// address override reaches only the refresh.
#[test]
fn the_relay_certificate_is_prepared_and_verified_through_the_ops_executable() {
    let play = play(TURN);
    let preparation = named(section(&play, "tasks"), PREPARATION);
    assert_eq!(preparation["when"], "not scpub_turn_activate | bool");
    let block = section(preparation, "block");
    let order = names(block);
    let at = |name: &str| order.iter().position(|known| *known == name).unwrap();
    let prepare = "Prepare a verified copy of the current Caddy certificate";
    let pull = "Pull the checksum-pinned coturn image without building on the VPS";
    let start = "Start only the relay after configuration and certificate validation";
    let verify = "Verify trusted relay TLS before scheduling certificate maintenance";
    let timer = "Install hourly certificate maintenance";
    assert!(at(prepare) < at(pull) && at(pull) < at(start));
    assert!(at(start) < at(verify) && at(verify) < at(timer));
    let prepared = named(block, prepare);
    assert_eq!(
        prepared["ansible.builtin.command"],
        json!({"argv": [OPS, "turn", "prepare-certificate"]})
    );
    let verified = named(block, verify);
    assert_eq!(
        addressed(verified),
        [
            json!([OPS, "turn", "refresh-certificate"]),
            json!([OPS, "turn", "refresh-certificate"]),
            json!([
                OPS,
                "turn",
                "refresh-certificate",
                "--verify-address",
                "192.0.2.45"
            ]),
        ]
    );
    for step in [prepared, verified] {
        assert_eq!(step["environment"], environment(), "{}", step["name"]);
        assert_eq!(step["changed_when"], true, "{}", step["name"]);
        for softened in ["when", "failed_when", "ignore_errors", "ignore_unreachable"] {
            assert!(step.get(softened).is_none(), "{}", step["name"]);
        }
    }
}

/// The hourly unit runs the refresh as the installed executable, once, in
/// the sandbox it had: only the relay's configuration, the results and the
/// lock directory are writable.
#[test]
fn the_certificate_timer_runs_the_ops_executable_in_its_sandbox() {
    let text = checkout_file(UNIT);
    let lines: Vec<&str> = text.lines().collect();
    let execution = format!("ExecStart={OPS} turn refresh-certificate");
    assert!(lines.contains(&execution.as_str()));
    let executions = lines.iter().filter(|line| line.starts_with("Exec"));
    assert_eq!(executions.count(), 1);
    for kept in [
        "Type=oneshot",
        "Requires=docker.service",
        "UMask=0077",
        "TimeoutStartSec=60",
        "NoNewPrivileges=true",
        "PrivateTmp=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        concat!(
            "ReadWritePaths=/etc/simplestchat-turn ",
            "/srv/simplestchat-public/results /run/simplestchat-bench",
        ),
        "MemoryMax=128M",
        "TasksMax=32",
        "RestrictSUIDSGID=true",
        "LockPersonality=true",
    ] {
        assert!(lines.contains(&kept), "{kept}");
    }
    assert!(!text.contains("python") && !text.contains("{{") && !text.contains("{%"));
}
