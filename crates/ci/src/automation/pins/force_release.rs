//! The forced-release playbook: explicit authorization of one unsigned
//! export, the signed Ops executable an earlier release delivered, and the
//! ordinary staging and deployment after its admission. The authorization
//! guard is evaluated by Ansible's own assert module. Nothing connects to a
//! host.

use super::ansible::asserted;
use super::checkout_file;
use super::monitoring::{
    LOOK, OPS, REQUIRE, environment, named, names, play, requirement, section,
};
use serde_json::{Map, Value, json};

const FORCED: &str = "ops/ansible/force-release.yml";
const GUARD: &str = "Require explicit force authorization and one prepared public host";
const PREFLIGHT: &str = "Verify the prepared host through the installed Ops executable";
const IDENTITY: &str = "Require the preflight's fresh operation identity";
const RUN: &str = "(scpub_force_preflight.stdout | from_json).runId";

/// What the controller gives the playbook for one forced release.
fn authorized() -> Map<String, Value> {
    let values = json!({
        "scpub_force_release": true,
        "scpub_enabled": true,
        "scpub_config": "/etc/simplestchat-public",
        "scpub_root": "/srv/simplestchat-public",
        "scpub_release_revision": "a".repeat(40),
        "scpub_force_directory": "/private/export",
        "scpub_force_request": "/private/evidence/force.json",
        "scpub_release_quiet_seconds": 0,
    });
    values.as_object().unwrap().clone()
}

/// The bounded transient unit one step of the release runs in.
fn unit(name: &str, seconds: [u32; 2], command: &[&str]) -> Value {
    let mut argv = vec![
        "systemd-run".to_owned(),
        format!("--unit=simplestchat-{name}-{{{{ {RUN} }}}}"),
        "--property=Type=exec".to_owned(),
        format!("--property=RuntimeMaxSec={}", seconds[0]),
        format!("--property=TimeoutStopSec={}", seconds[1]),
        "--wait".to_owned(),
        OPS.to_owned(),
    ];
    argv.extend(command.iter().map(|word| (*word).to_owned()));
    json!({
        "ansible.builtin.command": {"argv": argv},
        "environment": environment(),
        "changed_when": true,
    })
}

/// Whether `task` is exactly the unit `expected`, apart from its name and
/// the interpreter Ansible's own module runs under.
fn runs(task: &Value, expected: &Value) -> bool {
    let mut fields = task.as_object().unwrap().clone();
    fields.remove("name");
    let interpreter = fields.remove("vars");
    interpreter == Some(json!({"ansible_python_interpreter": "/usr/bin/python3"}))
        && Value::Object(fields) == *expected
}

/// The guard admits exactly the controller's forced request: an explicit
/// true flag, one prepared public host, a full revision, absolute local
/// paths, a bounded quiet time, and no signed selection beside it. Then come
/// the signed executable an earlier release delivered and its preflight,
/// before anything is created on the host.
#[test]
fn a_forced_release_needs_explicit_authorization_and_the_signed_ops_executable() {
    let play = play(FORCED);
    assert_eq!(play["serial"], 1);
    assert_eq!(play["gather_facts"], false);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [GUARD, LOOK, REQUIRE, PREFLIGHT, IDENTITY]
    );
    let guard = named(pre_tasks, GUARD);
    assert!(guard.get("when").is_none());
    let with = |changes: &[(&str, Value)]| {
        let mut values = authorized();
        for (key, value) in changes {
            values.insert((*key).to_owned(), value.clone());
        }
        Value::Object(values)
    };
    let mut cases = vec![
        (with(&[]), true),
        (with(&[("scpub_release_quiet_seconds", json!(600))]), true),
        (with(&[("scpub_release_quiet_seconds", json!("30"))]), true),
    ];
    for key in authorized().keys() {
        let mut values = authorized();
        values.remove(key);
        cases.push((Value::Object(values), false));
    }
    for (key, value) in [
        ("scpub_force_release", json!("true")),
        ("scpub_force_release", json!(1)),
        ("scpub_force_release", json!(false)),
        ("scpub_enabled", json!(false)),
        ("scpub_config", json!("/etc/other")),
        ("scpub_root", json!("/srv/other")),
        ("scpub_release_revision", json!("main")),
        ("scpub_release_revision", json!("A".repeat(40))),
        ("scpub_release_revision", json!("a".repeat(39))),
        ("scpub_force_directory", json!("results/export")),
        ("scpub_force_request", json!("force.json")),
        ("scpub_release_quiet_seconds", json!(-1)),
        ("scpub_release_quiet_seconds", json!(601)),
        ("scpub_release_artifact_id", json!(1234)),
        ("scpub_release_ci_run", json!(5678)),
    ] {
        cases.push((with(&[(key, value)]), false));
    }
    let rules = &guard["ansible.builtin.assert"]["that"];
    let (values, expected): (Vec<Value>, Vec<bool>) = cases.into_iter().unzip();
    assert_eq!(asserted(rules, &values), expected);
    requirement(pre_tasks, "scpub_force_installed");
    let preflight = named(pre_tasks, PREFLIGHT);
    assert_eq!(
        preflight["ansible.builtin.command"],
        json!({"argv": [OPS, "release", "preflight"]})
    );
    assert_eq!(preflight["environment"], environment());
    assert_eq!(preflight["register"], "scpub_force_preflight");
    assert_eq!(preflight["changed_when"], false);
    assert_eq!(
        named(pre_tasks, IDENTITY)["ansible.builtin.assert"]["that"],
        json!([
            "(scpub_force_preflight.stdout | from_json).schemaVersion == 1",
            format!("{RUN} is match('^[a-f0-9]{{32}}$')"),
        ])
    );
}

/// The export's four files and the request arrive in one new private
/// directory and never replace anything; the installed executable admits
/// them, stages the image and replaces the application, each in its own
/// bounded unit and unconditionally. The playbook installs nothing, runs no
/// interpreter of its own and contacts no signed source.
#[test]
fn a_forced_release_is_admitted_staged_and_deployed_by_the_installed_ops_executable() {
    let play = play(FORCED);
    let tasks = section(&play, "tasks");
    assert_eq!(
        names(tasks),
        [
            "Create private storage without overwriting a retained release",
            "Transfer only the four exported build artifact files",
            "Retain the exact explicit unsigned authorization and file hashes",
            "Admit the unsigned artifact through the installed Ops executable",
            "Import and verify the image while the public service remains running",
            "Replace only the application through the ordinary deployment",
        ]
    );
    let operation = format!("{{{{ scpub_root }}}}/forced/{{{{ {RUN} }}}}");
    assert_eq!(
        tasks[0]["loop"],
        json!([
            "{{ scpub_root }}/releases",
            "{{ scpub_root }}/forced",
            operation
        ])
    );
    assert_eq!(tasks[0]["ansible.builtin.file"]["mode"], "0700");
    assert_eq!(
        tasks[1]["loop"],
        json!(["image.tar", "release.json", "source.json", "outcome.json"])
    );
    assert_eq!(
        tasks[1]["ansible.builtin.copy"],
        json!({"src": "{{ scpub_force_directory }}/{{ item }}",
            "dest": format!("{operation}/{{{{ item }}}}"),
            "owner": "root", "group": "root", "mode": "0600", "force": false})
    );
    assert_eq!(
        tasks[2]["ansible.builtin.copy"],
        json!({"src": "{{ scpub_force_request }}", "dest": format!("{operation}/force.json"),
            "owner": "root", "group": "root", "mode": "0600", "force": false})
    );
    let request = format!("{operation}/force.json");
    let admit = unit(
        "force",
        [900, 60],
        &["release", "admit-unsigned", "--request", &request],
    );
    assert!(runs(&tasks[3], &admit));
    let revision = "{{ scpub_release_revision }}";
    let named_unit = |kind: &str| format!("{kind}-{{{{ scpub_release_revision[:12] }}}}");
    let limits =
        |seconds: &'static str| ["--timeout-seconds", seconds, "--finalization-seconds", "60"];
    let mut stage = vec!["release", "stage", "--revision", revision];
    stage.extend(limits("600"));
    assert!(runs(
        &tasks[4],
        &unit(&named_unit("stage"), [600, 60], &stage)
    ));
    let quiet = "{{ scpub_release_quiet_seconds }}";
    let mut deploy = vec![
        "release",
        "deploy",
        "--revision",
        revision,
        "--quiet-seconds",
        quiet,
    ];
    deploy.extend(limits("1800"));
    assert!(runs(
        &tasks[5],
        &unit(&named_unit("release"), [1800, 240], &deploy)
    ));
    for task in tasks {
        assert!(task.get("when").is_none() && task.get("ignore_errors").is_none());
        assert!(task.get("delegate_to").is_none());
    }
    let text = checkout_file(FORCED);
    for absent in [
        ".py",
        "fetch",
        "receive",
        "localhost",
        "scpub_release_prepared",
        "helpers",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
    // Ansible's own modules need an interpreter; nothing else names one.
    let interpreter = "ansible_python_interpreter: /usr/bin/python3";
    assert_eq!(text.matches(interpreter).count(), 4);
    assert!(!text.replace(interpreter, "").contains("python"));
}
