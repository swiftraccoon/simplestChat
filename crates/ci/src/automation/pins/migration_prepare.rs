//! The migration preparation playbook's read-only inspection, secret handling,
//! certificate transfer and staging, carried over from the Python migration
//! tests. Each pin reads the playbook offline.

use super::{document, first, located};
use serde_json::{Value, json};

/// The single play of the migration preparation playbook.
fn play() -> Value {
    document("ops/ansible/migration-prepare.yml")[0].clone()
}

/// A list whose every task is a mapping.
fn tasks(value: &Value) -> &[Value] {
    let tasks = value.as_array().unwrap();
    assert!(tasks.iter().all(Value::is_object));
    tasks
}

/// The block of the first task selected by one migration action.
fn block<'a>(play: &'a Value, action: &str) -> &'a [Value] {
    let selector = format!("scmig_action == '{action}'");
    let selected = first(tasks(&play["tasks"]), |task| {
        task["when"].to_string().contains(&selector)
    });
    tasks(&selected["block"])
}

/// A list whose every item must be text.
fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap().iter();
    items.map(|item| item.as_str().unwrap()).collect()
}

/// Whether a task uses `module`.
fn uses(task: &Value, module: &str) -> bool {
    task.get(module).is_some()
}

/// From `test_inspection_is_read_only_remote_and_receipt_is_private_local`:
/// the source stays live, and physical identities are compared before
/// preparation.
#[test]
fn inspection_is_read_only_remote_and_receipt_is_private_local() {
    let play = play();
    let tasks = block(&play, "inspect");
    let command = first(tasks, |task| uses(task, "ansible.builtin.command"));
    assert_eq!(command["changed_when"], false);
    assert!(command.to_string().contains("migration_inspect.py"));
    for task in tasks {
        assert!(!uses(task, "ansible.builtin.file") && !uses(task, "ansible.builtin.fetch"));
    }
    let receipt = first(tasks, |task| uses(task, "ansible.builtin.copy"));
    assert_eq!(receipt["delegate_to"], "localhost");
    assert_eq!(receipt["become"], false);
    assert_eq!(receipt["ansible.builtin.copy"]["mode"], "0600");
    let guard = first(tasks, |task| uses(task, "ansible.builtin.assert"));
    let rules = strings(&guard["ansible.builtin.assert"]["that"]).join(" ");
    assert!(rules.contains("machineId !="));
    assert_eq!(play["strategy"], "linear");
    assert_eq!(play["any_errors_fatal"], true);
}

/// From `test_secret_copy_requires_exact_digest_without_owner_or_secret_logging`:
/// credentials stay private, and an existing differing destination is never
/// overwritten.
#[test]
fn secret_copy_requires_exact_digest_without_owner_or_secret_logging() {
    let play = play();
    let tasks = block(&play, "bootstrap");
    let fetch = first(tasks, |task| uses(task, "ansible.builtin.fetch"));
    assert_eq!(fetch["no_log"], true);
    let secrets = "/etc/simplestchat-public/secrets.json";
    let copies = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task["ansible.builtin.copy"]["dest"] == secrets)
        .collect::<Vec<_>>();
    let [(copied, target)] = copies[..] else {
        panic!("exactly one task copies the secrets");
    };
    assert_eq!(target["when"], "inventory_hostname == 'migration_target'");
    assert_eq!(target["no_log"], true);
    assert_eq!(target["ansible.builtin.copy"]["force"], false);
    assert_eq!(target["ansible.builtin.copy"]["mode"], "0600");
    assert!(
        !Value::from(tasks.to_vec())
            .to_string()
            .contains("owner.json")
    );
    let guards = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| uses(task, "ansible.builtin.assert"))
        .filter(|(_, task)| task.to_string().contains("checksum"))
        .collect::<Vec<_>>();
    assert_eq!(guards.len(), 3);
    assert!(guards.iter().all(|(_, task)| task["no_log"] == true));
    assert!(guards[1].0 < copied);
    assert!(guards[2].0 > copied);
}

/// From `test_same_origin_tls_transfer_is_private_and_precedes_staging`: the
/// exact hostname subtree is validated on both hosts without logging its
/// payload.
#[test]
fn same_origin_tls_transfer_is_private_and_precedes_staging() {
    let play = play();
    let tasks = tasks(&play["tasks"]);
    let (transferred, tls) = located(tasks, |task| {
        task["name"] == "Preserve the unchanged hostname certificate before DNS cutover"
    });
    assert_eq!(
        tls["when"],
        json!([
            "scmig_action == 'bootstrap'",
            "scmig_source_origin == scmig_destination_origin"
        ])
    );
    let steps = self::tasks(&tls["block"]);
    assert!(steps.iter().all(|step| step["no_log"] == true));
    let last = &steps[steps.len() - 1];
    assert_eq!(steps[0]["when"], "inventory_hostname == 'migration_source'");
    assert_eq!(last["when"], "inventory_hostname == 'migration_target'");
    assert!(strings(&steps[0]["ansible.builtin.command"]["argv"]).contains(&"export"));
    assert!(strings(&last["ansible.builtin.command"]["argv"]).contains(&"import"));
    let copy = first(steps, |step| uses(step, "ansible.builtin.copy"));
    assert_eq!(copy["ansible.builtin.copy"]["force"], false);
    assert_eq!(copy["ansible.builtin.copy"]["mode"], "0600");
    let (staged, stage) = located(tasks, |task| {
        task["name"] == "Transfer and stage the exact verified CI artifact only on the destination"
    });
    assert!(transferred < staged);
    assert!(strings(&stage["when"]).contains(&"not (scmig_force_release | default(false) | bool)"));
}

/// From `test_stage_uses_verified_receiver_then_only_bounded_image_import`:
/// fresh hosts use the signed receiver and the real stage action, without
/// builds or startup.
#[test]
fn stage_uses_verified_receiver_then_only_bounded_image_import() {
    let play = play();
    let tasks = block(&play, "stage");
    let commands = tasks
        .iter()
        .filter(|task| uses(task, "ansible.builtin.command"))
        .collect::<Vec<_>>();
    let [transfer, stage] = commands[..] else {
        panic!("exactly two commands");
    };
    let fetch = strings(&transfer["ansible.builtin.command"]["argv"]);
    assert!(fetch.contains(&"{{ playbook_dir }}/../../build/fetch-release.py"));
    assert_eq!(
        fetch[fetch.len() - 2..],
        [
            "--verified-directory",
            "{{ scpub_release_verified_directory }}"
        ]
    );
    assert_eq!(transfer["delegate_to"], "localhost");
    assert_eq!(transfer["become"], false);
    let argv = stage["ansible.builtin.command"]["argv"].as_array().unwrap();
    assert!(
        argv.iter()
            .any(|word| word == "--property=RuntimeMaxSec=600")
    );
    assert_eq!(
        argv[argv.len() - 3..],
        [
            "/usr/local/libexec/simplestchat-public/release-public.py",
            "stage",
            "{{ scmig_revision }}"
        ]
    );
    assert!(argv.iter().all(|word| word != "deploy"));
}
