//! The browser asset store tasks and its expiry unit. The installed Ops
//! executable initializes and expires the store wherever a host has one; the
//! directory the proxy mounts exists on every host. Nothing connects to a host.

use super::ansible::evaluate;
use super::{checkout_file, document};
use serde_json::{Value, json};

const OPS: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
const TASKS: &str = "ops/ansible/tasks/browser-assets.yml";
const INITIALIZE: &str = "Initialize the bounded public browser asset store before startup";

/// The task file's tasks, each a named mapping.
fn tasks() -> Vec<Value> {
    let tasks = document(TASKS).as_array().unwrap().clone();
    assert!(tasks.iter().all(|task| task["name"].is_string()));
    tasks
}

/// The store directory exists before anything mounts it, and only the
/// installed executable initializes it, with a fixed environment, under the
/// revision the caller selected.
#[test]
fn asset_tasks_initialize_only_through_the_installed_ops_executable() {
    let tasks = tasks();
    let names = tasks.iter().map(|task| task["name"].as_str().unwrap());
    assert_eq!(
        names.collect::<Vec<_>>(),
        [
            "Ensure the public browser asset store directory the proxy mounts",
            "Look for the installed Ops executable without following links",
            INITIALIZE,
            "Record the installed reviewed proxy capability on initial provisioning",
            "Install bounded public asset expiration service and timer",
            "Enable public asset expiration between deployments",
        ]
    );
    assert_eq!(
        tasks[0]["ansible.builtin.file"],
        json!({
            "path": "{{ scpub_root }}/browser-assets", "state": "directory",
            "owner": "root", "group": "root", "mode": "0755",
        })
    );
    assert_eq!(
        tasks[1]["ansible.builtin.stat"],
        json!({"path": OPS, "follow": false, "get_checksum": false})
    );
    assert_eq!(tasks[1]["register"], "scpub_browser_asset_ops");
    let initialize = &tasks[2];
    assert_eq!(
        initialize["ansible.builtin.command"]["argv"],
        json!([
            OPS,
            "release",
            "assets",
            "initialize",
            "--revision",
            "{{ scpub_browser_assets_revision | default(scbench_revision) }}",
        ])
    );
    assert_eq!(
        initialize["environment"],
        json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "", "GITHUB_TOKEN": ""})
    );
    assert_eq!(
        initialize["when"],
        json!([
            "not ansible_check_mode",
            "scpub_browser_asset_ops.stat.exists"
        ])
    );
    assert_eq!(initialize["register"], "scpub_browser_asset_initialization");
    for task in &tasks {
        for refusal in ["failed_when", "ignore_errors", "ignore_unreachable"] {
            assert!(task.get(refusal).is_none(), "{}", task["name"]);
        }
    }
    let text = checkout_file(TASKS);
    assert!(!text.contains("python") && !text.contains(".py"));
}

/// The change the task reports is the command's own receipt, decided by
/// Ansible: a first initialization or a new selection changes, a repeat and a
/// store another operation holds do not.
#[test]
fn initialization_reports_only_the_change_its_receipt_names() {
    let tasks = tasks();
    let condition = tasks[2]["changed_when"].as_str().unwrap();
    let receipt = |text: &str| json!({"scpub_browser_asset_initialization": {"stdout": text}});
    let cases = [
        receipt("{\"passed\":true,\"changed\":true}\n"),
        receipt("{\"passed\":true,\"changed\":false}\n"),
        receipt("{\"passed\":true,\"busy\":true,\"changed\":false}\n"),
    ];
    let verdicts = evaluate(&[condition], &cases);
    assert_eq!(verdicts, [[json!(true)], [json!(false)], [json!(false)]]);
}

/// The expiry unit runs the same executable under its unchanged sandbox, and
/// is skipped on a host that has none: such a host has retained nothing.
#[test]
fn expiry_unit_runs_the_installed_ops_executable_where_one_exists() {
    let unit = checkout_file("ops/ansible/templates/browser-assets.service.j2");
    let lines = unit.lines().collect::<Vec<_>>();
    assert!(lines.contains(&format!("ConditionPathExists={OPS}").as_str()));
    assert!(lines.contains(&format!("ExecStart={OPS} release assets prune").as_str()));
    let executions = lines.iter().filter(|line| line.starts_with("Exec"));
    assert_eq!(executions.count(), 1);
    for kept in [
        "Type=oneshot",
        "UMask=0077",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/srv/simplestchat-public/browser-assets /run/simplestchat-bench",
    ] {
        assert!(lines.contains(&kept), "{kept}");
    }
    assert!(!unit.contains("python") && !unit.contains("{{") && !unit.contains("{%"));
}
