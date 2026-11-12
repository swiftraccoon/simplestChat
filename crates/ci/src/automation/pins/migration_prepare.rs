//! The migration preparation playbook's controller-local verification,
//! read-only inspection, secret handling, certificate transfer and native
//! signed staging. The first pins are carried over from the Python migration
//! tests. Each pin reads the playbook offline; the selection guards are
//! evaluated by Ansible's own assert module.

use super::ansible::asserted;
use super::{document, first, located};
use serde_json::{Map, Value, json};

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

/// The names of a block's tasks, in order.
fn names(tasks: &[Value]) -> Vec<&str> {
    tasks
        .iter()
        .map(|task| task["name"].as_str().unwrap())
        .collect()
}

/// Typed selections a signed move passes to every play. Fictional values.
fn selection() -> Map<String, Value> {
    let revision = "a".repeat(40);
    let values = json!({
        "scmig_revision": revision,
        "scpub_release_repository": "swiftraccoon/simplestChat",
        "scpub_release_artifact_id": 1234,
        "scpub_release_ci_run": 5678,
        "scpub_release_expected_revision": revision,
        "scpub_release_revision": revision,
        "scpub_release_verified_directory": "/private/results/verify.1/artifact",
        "scpub_ops_artifact_id": 4321,
        "scpub_ops_selection": {"repository": "swiftraccoon/simplestChat",
            "revision": revision, "packageVersion": "0.1.0", "protocolVersion": 1,
            "target": "x86_64-unknown-linux-musl", "buildRunId": 5678, "runAttempt": 2},
        "scpub_ops_release_controller": "/private/controller/simplestchat-release",
        "scpub_ops_gh_executable": "/private/tools/gh",
        "ansible_host": "192.0.2.11",
        "ansible_user": "root",
        "ansible_ssh_private_key_file": "/private/controller key",
    });
    values.as_object().unwrap().clone()
}

/// The baseline with one variable replaced, or removed when `value` is null.
fn changed(key: &str, value: Value) -> Value {
    let mut values = selection();
    match value {
        Value::Null => drop(values.remove(key)),
        value => drop(values.insert(key.into(), value)),
    }
    Value::Object(values)
}

/// A selection whose Ops half differs in one field, or lacks or adds one.
fn other_ops(key: &str, value: Value) -> Value {
    let mut ops = selection()["scpub_ops_selection"].clone();
    match value {
        Value::Null => drop(ops.as_object_mut().unwrap().remove(key)),
        value => ops[key] = value,
    }
    changed("scpub_ops_selection", ops)
}

/// What the controller's own verification prints, as Ansible registers it.
fn verified(change: impl Fn(&mut Value)) -> Value {
    let mut summary = json!({"directory": "/private/results/verify.1/artifact",
        "image.tar": "c".repeat(64), "release.json": "d".repeat(64),
        "revision": "a".repeat(40)});
    change(&mut summary);
    json!({"rc": 0, "stdout": summary.to_string()})
}

/// The signed application is verified by the controller alone, before any
/// task that contacts a host, and only its identity is retained.
#[test]
fn verification_runs_on_the_controller_before_any_host_and_retains_its_record() {
    let play = play();
    let step = &tasks(&play["tasks"])[0];
    let name = "Verify the signed application on the controller before either host is contacted";
    assert_eq!(step["name"], name);
    let only = [
        "scmig_action == 'verify'",
        "inventory_hostname == 'migration_source'",
    ];
    assert_eq!(step["when"], json!(only));
    assert_eq!(
        (&step["delegate_to"], &step["become"]),
        (&json!("localhost"), &json!(false))
    );
    let guard = &tasks(&play["pre_tasks"])[0]["ansible.builtin.assert"]["that"];
    let actions = "scmig_action in ['verify', 'inspect', 'bootstrap', 'stage']";
    assert!(strings(guard).contains(&actions));
    let steps = tasks(&step["block"]);
    let modules =
        ["assert", "command", "assert", "copy"].map(|module| format!("ansible.builtin.{module}"));
    assert_eq!(steps.len(), modules.len());
    for (step, module) in steps.iter().zip(&modules) {
        assert!(uses(step, module), "{module}");
        assert!(step.get("delegate_to").is_none() && step.get("when").is_none());
    }
    let command = &steps[1];
    assert_eq!(
        strings(&command["ansible.builtin.command"]["argv"]),
        [
            "{{ scpub_ops_release_controller }}",
            "verify",
            "--repository",
            "{{ scpub_release_repository }}",
            "--artifact-id",
            "{{ scpub_release_artifact_id }}",
            "--revision",
            "{{ scmig_revision }}",
            "--ci-run",
            "{{ scpub_release_ci_run }}",
            "--artifact-dir",
            "{{ scpub_release_verified_directory | default('') }}",
            "--output-parent",
            "{{ (playbook_dir ~ '/../../results') | realpath }}",
            "--timeout-seconds",
            "600",
            "--finalization-seconds",
            "60",
        ]
    );
    let path = "{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin";
    assert_eq!(command["environment"], json!({"PATH": path}));
    assert_eq!(
        (&command["changed_when"], &command["timeout"]),
        (&json!(false), &json!(660))
    );
    assert_eq!(command["register"], "scmig_verified_application");
    let record = &steps[3]["ansible.builtin.copy"];
    let kept = json!({"content": "{{ scmig_verified_application.stdout | from_json | to_json }}",
        "dest": "{{ scmig_evidence }}/application.json", "mode": "0600", "force": false});
    assert_eq!(record, &kept);
    // The selectors: exact integers, this repository, absolute trusted tools.
    let mut cases = vec![(Value::Object(selection()), true)];
    for (key, value) in [
        ("scpub_release_repository", json!("example/simplestChat")),
        ("scpub_release_artifact_id", json!("1234")),
        ("scpub_release_artifact_id", json!(0)),
        ("scpub_release_ci_run", json!(9007199254740992u64)),
        ("scpub_release_expected_revision", json!("b".repeat(40))),
        (
            "scpub_ops_release_controller",
            json!("simplestchat-release"),
        ),
        ("scpub_ops_release_controller", Value::Null),
        ("scpub_ops_gh_executable", json!("/private/tools/hub")),
        ("scpub_ops_gh_executable", json!("/private/tools:/other/gh")),
        ("scpub_ops_gh_executable", json!("gh")),
    ] {
        cases.push((changed(key, value), false));
    }
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[0]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    // The result: exactly the four fields, for exactly this revision.
    let parsed = steps[2]["vars"]["scmig_application"].clone();
    let result = |registered: Value| {
        let mut values = selection();
        values.insert("scmig_verified_application".into(), registered);
        values.insert("scmig_application".into(), parsed.clone());
        Value::Object(values)
    };
    let cases = [
        (verified(|_| {}), true),
        (
            verified(|summary| summary["revision"] = json!("b".repeat(40))),
            false,
        ),
        (
            verified(|summary| summary["directory"] = json!("results/verify.1")),
            false,
        ),
        (
            verified(|summary| summary["image.tar"] = json!("c".repeat(63))),
            false,
        ),
        (
            verified(|summary| summary["release.json"] = json!("D".repeat(64))),
            false,
        ),
        (verified(|summary| summary["extra"] = json!(true)), false),
        (
            verified(|summary| drop(summary.as_object_mut().unwrap().remove("image.tar"))),
            false,
        ),
        (
            json!({"rc": 1, "stdout": verified(|_| {})["stdout"]}),
            false,
        ),
        (json!({"rc": 0, "stdout": "[]"}), false),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| result(case.clone())).collect();
    let verdicts = asserted(&steps[2]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// A fresh destination is staged as a routine release stages a prepared one,
/// without the prepared-host preflight: the originals are verified again,
/// the signed Ops executable is delivered, and only then does the host fetch
/// and import the release through it. Nothing is built, started or
/// interpreted.
#[test]
fn stage_reverifies_delivers_the_signed_executable_then_fetches_and_imports_natively() {
    let play = play();
    let tasks = block(&play, "stage");
    assert_eq!(
        names(tasks),
        [
            "Require exact signed-release selectors and direct key-based SSH",
            "Verify the retained signed originals again through the native controller",
            "Require the completed native application verification result",
            "Use the completed native controller verification result",
            "Snapshot the controller-verified application predicate",
            "Snapshot the controller-verified application manifest",
            "Require the verified application originals before any Ops delivery",
            "Read the controller-verified application predicate",
            "Deliver the signed current Ops executable through native policy",
            "Create private immutable storage for the selected release",
            "Verify the exact GitHub release and fetch it directly onto the destination",
            "Import the verified production image without launching the app",
            "Copy only the original verified predicate without replacing existing evidence",
            "Recheck the retained predicate and the staged manifest",
            "Require the staged release to hold the exact signed originals",
        ]
    );
    let text = Value::from(tasks.to_vec()).to_string();
    for retired in [".py", "python", "release-public", "build/"] {
        assert!(!text.contains(retired), "{retired}");
    }
    let local = |task: &Value| task["delegate_to"] == "localhost" && task["become"] == false;
    let directory = "{{ scpub_release_verified_directory }}";
    let verify = strings(&tasks[1]["ansible.builtin.command"]["argv"]);
    assert_eq!(
        verify[..2],
        ["{{ scpub_ops_release_controller }}", "verify"]
    );
    let at = verify
        .iter()
        .position(|word| *word == "--artifact-dir")
        .unwrap();
    assert_eq!(verify[at + 1], directory);
    assert!(local(&tasks[1]) && tasks[1]["changed_when"] == false);
    assert_eq!(tasks[1]["register"], "scpub_native_validated_release");
    // The delivery's inputs are the registered originals, all read locally.
    let inputs = [
        (4, "scpub_app_predicate_file"),
        (5, "scpub_app_manifest_file"),
        (7, "scpub_app_predicate_bytes"),
    ];
    for (index, register) in inputs {
        assert!(local(&tasks[index]), "{register}");
        assert_eq!(tasks[index]["register"], register);
    }
    assert_eq!(tasks[7]["no_log"], true);
    assert_eq!(
        tasks[8]["ansible.builtin.import_tasks"],
        "tasks/signed-ops.yml"
    );
    let fetch = strings(&tasks[10]["ansible.builtin.command"]["argv"]);
    assert_eq!(fetch[..2], ["{{ scpub_ops_release_controller }}", "fetch"]);
    for (flag, value) in [
        ("--revision", "{{ scmig_revision }}"),
        ("--artifact-id", "{{ scpub_release_artifact_id }}"),
        ("--ci-run", "{{ scpub_release_ci_run }}"),
        ("--host", "{{ ansible_host }}"),
        ("--identity", "{{ ansible_ssh_private_key_file }}"),
        ("--verified-directory", directory),
        ("--timeout-seconds", "1020"),
    ] {
        let at = fetch.iter().position(|word| *word == flag).unwrap();
        assert_eq!(fetch[at + 1], value, "{flag}");
    }
    assert!(local(&tasks[10]));
    assert_eq!(tasks[10]["timeout"], 1080);
    let stage = &tasks[11];
    assert_eq!(
        strings(&stage["ansible.builtin.command"]["argv"]),
        [
            "systemd-run",
            "--unit=simplestchat-migration-stage-{{ scmig_run_id }}",
            "--property=Type=exec",
            "--property=RuntimeMaxSec=600",
            "--property=TimeoutStopSec=60",
            "--wait",
            "--collect",
            "/usr/local/libexec/simplestchat-public/simplestchat-ops",
            "release",
            "stage",
            "--revision",
            "{{ scmig_revision }}",
            "--timeout-seconds",
            "600",
            "--finalization-seconds",
            "60",
        ]
    );
    let environment = json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "",
        "GITHUB_TOKEN": ""});
    assert_eq!(stage["environment"], environment);
    assert!(stage.get("delegate_to").is_none());
    let predicate = &tasks[12]["ansible.builtin.copy"];
    assert_eq!(
        (&predicate["force"], &predicate["mode"]),
        (&json!(false), &json!("0600"))
    );
    let retained = "{{ scpub_root }}/releases/{{ scmig_revision }}/release-predicate.json";
    assert_eq!(predicate["dest"], retained);
    // The selectors, as Ansible evaluates them.
    let mut cases = vec![
        (Value::Object(selection()), true),
        (
            changed("scpub_ops_artifact_zip", json!("/private/ops/artifact.zip")),
            true,
        ),
        (changed("ansible_port", json!(2222)), true),
    ];
    for (key, value) in [
        ("scpub_release_repository", json!("example/simplestChat")),
        ("scpub_release_artifact_id", json!("1234")),
        ("scpub_release_ci_run", json!(0)),
        ("scpub_release_expected_revision", json!("b".repeat(40))),
        ("scpub_release_revision", json!("b".repeat(40))),
        (
            "scpub_release_verified_directory",
            json!("results/verify.1/artifact"),
        ),
        ("scpub_release_verified_directory", Value::Null),
        ("scpub_ops_artifact_id", Value::Null),
        ("scpub_ops_artifact_id", json!(true)),
        ("scpub_ops_selection", Value::Null),
        (
            "scpub_ops_release_controller",
            json!("simplestchat-release"),
        ),
        ("scpub_ops_gh_executable", json!("/private/tools/hub")),
        ("scpub_ops_artifact_zip", json!("artifact.zip")),
        ("ansible_host", json!("-oProxyCommand=id")),
        ("ansible_user", json!("")),
        ("ansible_ssh_private_key_file", json!("~/.ssh/id_ed25519")),
        ("ansible_port", json!(65536)),
        ("ansible_password", json!("")),
        ("ansible_become_pass", json!("")),
        ("ansible_ssh_common_args", json!("-J jump.example.test")),
    ] {
        cases.push((changed(key, value), false));
    }
    for (key, value) in [
        ("repository", json!("example/simplestChat")),
        ("revision", json!("b".repeat(40))),
        ("packageVersion", json!("0.2.0")),
        ("protocolVersion", json!(2)),
        ("target", json!("aarch64-unknown-linux-musl")),
        ("buildRunId", json!(5679)),
        ("runAttempt", json!(0)),
        ("runAttempt", Value::Null),
        ("signature", json!("unreviewed")),
    ] {
        cases.push((other_ops(key, value), false));
    }
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&tasks[0]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    // The second verification must name the very directory the first did.
    let parsed = tasks[2]["vars"]["scpub_native_verified_release"].clone();
    let result = |registered: Value| {
        let mut values = selection();
        values.insert("scpub_native_validated_release".into(), registered);
        values.insert("scpub_native_verified_release".into(), parsed.clone());
        Value::Object(values)
    };
    let cases = [
        (verified(|_| {}), true),
        (
            verified(|summary| summary["directory"] = json!("/private/results/verify.2/artifact")),
            false,
        ),
        (
            verified(|summary| summary["revision"] = json!("b".repeat(40))),
            false,
        ),
        (
            verified(|summary| summary["release.json"] = json!("d".repeat(63))),
            false,
        ),
        (
            json!({"rc": 2, "stdout": verified(|_| {})["stdout"]}),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| result(case.clone())).collect();
    let verdicts = asserted(&tasks[2]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}
