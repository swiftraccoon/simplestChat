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

/// Both machines are inspected by the executable this move authenticated,
/// run from a volatile private directory before anything is installed: the
/// source stays live, nothing is interpreted, and physical identities are
/// compared before preparation.
#[test]
fn inspection_runs_the_authenticated_executable_uninstalled_and_keeps_its_receipt_local() {
    let play = play();
    let all = tasks(&play["tasks"]);
    let inspection = first(all, |task| {
        task["name"] == "Inspect both selected machines without changing services or stored files"
    });
    assert_eq!(inspection["when"], "scmig_action == 'inspect'");
    let steps = tasks(&inspection["block"]);
    assert_eq!(
        names(steps),
        [
            "Snapshot the authenticated executable the controller retained",
            "Require exactly the executable this move authenticated",
            "Create a private volatile directory for the executable",
            "Transfer the authenticated executable without installing it",
            "Join the transferred bytes before their first execution",
            "Require the authenticated bytes on the host",
            "Read the physical identity and source configuration or empty target state",
            "Require two distinct successful physical host inspections",
            "Retain only the inspected identities in private controller evidence",
        ]
    );
    let text = inspection.to_string();
    for retired in [
        ".py",
        "python",
        "/usr/local/libexec",
        "ansible.builtin.fetch",
    ] {
        assert!(!text.contains(retired), "{retired}");
    }
    let local = |task: &Value| task["delegate_to"] == "localhost" && task["become"] == false;
    assert!(local(&steps[0]));
    assert_eq!(
        steps[0]["ansible.builtin.stat"]["path"],
        "{{ scmig_ops_executable }}"
    );
    let scratch = json!({"state": "directory", "path": "/run", "prefix": "simplestchat-inspect-"});
    assert_eq!(steps[2]["ansible.builtin.tempfile"], scratch);
    let held = "{{ scmig_inspector.path }}/simplestchat-ops";
    let transfer = json!({"src": "{{ scmig_ops_executable }}", "dest": held, "owner": "root",
        "group": "root", "mode": "0700", "force": false});
    assert_eq!(steps[3]["ansible.builtin.copy"], transfer);
    assert_eq!(steps[4]["ansible.builtin.stat"]["path"], held);
    let command = &steps[6];
    let role = "{{ 'source' if inventory_hostname == 'migration_source' else 'target' }}";
    let origin = concat!(
        "{{ scmig_source_origin if inventory_hostname == 'migration_source' ",
        "else scmig_destination_origin }}"
    );
    assert_eq!(
        strings(&command["ansible.builtin.command"]["argv"]),
        [held, "migrate", "inspect", role, origin]
    );
    let environment = json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "",
        "GITHUB_TOKEN": ""});
    assert_eq!(command["environment"], environment);
    assert_eq!(
        (&command["changed_when"], &command["timeout"]),
        (&json!(false), &json!(60))
    );
    assert_eq!(command["register"], "scmig_inspected");
    for step in &steps[1..8] {
        assert!(step.get("delegate_to").is_none(), "{}", step["name"]);
    }
    let rules = strings(&steps[7]["ansible.builtin.assert"]["that"]).join(" ");
    assert!(rules.contains("machineId !="));
    let receipt = &steps[8];
    assert!(local(receipt));
    assert_eq!(receipt["ansible.builtin.copy"]["mode"], "0600");
    assert_eq!(receipt["ansible.builtin.copy"]["force"], false);
    // Whatever happened, the executable does not stay on either machine.
    let always = tasks(&inspection["always"]);
    let removal = json!({"path": "{{ scmig_inspector.path }}", "state": "absent"});
    assert_eq!(always.len(), 1);
    assert_eq!(always[0]["ansible.builtin.file"], removal);
    assert_eq!(always[0]["when"], "scmig_inspector.path is defined");
    assert_eq!(play["strategy"], "linear");
    assert_eq!(play["any_errors_fatal"], true);
    // The two guards, as Ansible evaluates them.
    let digest = "e".repeat(64);
    let stat = |checksum: &str, change: &dyn Fn(&mut Value)| {
        let mut stat = json!({"isreg": true, "islnk": false, "nlink": 1, "uid": 0,
            "mode": "0700", "checksum": checksum});
        change(&mut stat);
        json!({"stat": stat})
    };
    let evidence = "/private/results/migration.1";
    let executable = format!("{evidence}/ops/verified/simplestchat-ops");
    let controller = |executable: &str, pin: &str, original: Value| {
        json!({"scmig_evidence": evidence, "scmig_ops_executable": executable,
            "scmig_ops_sha256": pin, "scmig_inspector_original": original})
    };
    let cases = [
        (
            controller(&executable, &digest, stat(&digest, &|_| {})),
            true,
        ),
        (
            controller(
                "/private/elsewhere/simplestchat-ops",
                &digest,
                stat(&digest, &|_| {}),
            ),
            false,
        ),
        (controller(&executable, "E", stat("E", &|_| {})), false),
        (
            controller(&executable, &digest, stat(&"f".repeat(64), &|_| {})),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["islnk"] = json!(true)),
            ),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["isreg"] = json!(false)),
            ),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["nlink"] = json!(2)),
            ),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[1]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    let host = |copy: Value| json!({"scmig_ops_sha256": digest, "scmig_inspector_copy": copy});
    let cases = [
        (host(stat(&digest, &|_| {})), true),
        (host(stat(&"f".repeat(64), &|_| {})), false),
        (
            host(stat(&digest, &|stat| stat["uid"] = json!(1000))),
            false,
        ),
        (
            host(stat(&digest, &|stat| stat["mode"] = json!("0755"))),
            false,
        ),
        (
            host(stat(&digest, &|stat| stat["islnk"] = json!(true))),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[5]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
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
    // Only a signed release is staged; no other route selects this step.
    let only = [
        "scmig_action == 'stage'",
        "inventory_hostname == 'migration_target'",
    ];
    assert_eq!(stage["when"], json!(only));
    let staging = tasks
        .iter()
        .filter(|task| task["when"].to_string().contains("'stage'"));
    assert_eq!(staging.count(), 1);
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
    let modules = [
        "assert", "command", "assert", "copy", "set_fact", "copy", "command", "command", "stat",
        "assert", "copy",
    ]
    .map(|module| format!("ansible.builtin.{module}"));
    assert_eq!(steps.len(), modules.len());
    for (index, (step, module)) in steps.iter().zip(&modules).enumerate() {
        assert!(uses(step, module), "{module}");
        // Only the token is read conditionally: when the caller supplied none.
        assert!(step.get("delegate_to").is_none(), "{module}");
        assert_eq!(step.get("when").is_some(), index == 6, "{module}");
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

/// The executable the inspection runs is authenticated on the controller in
/// the same host-free step: one pinned request, the token only in the child's
/// environment, and an executable that must hash to the digest the signed
/// attestation's policy names.
#[test]
fn the_ops_executable_is_authenticated_on_the_controller_before_any_host() {
    let play = play();
    let step = &tasks(&play["tasks"])[0];
    let steps = tasks(&step["block"]);
    assert_eq!(
        names(&steps[4..]),
        [
            "Serialize one original Ops acquisition request",
            "Retain the original acquisition request privately",
            "Read the user's gh token only when GH_TOKEN was not supplied",
            "Authenticate the signed Ops executable on the controller",
            "Snapshot the authenticated executable",
            "Require the executable the verifier's signed policy pins",
            "Retain the authenticated executable's identity in private controller evidence",
        ]
    );
    let request = steps[4]["ansible.builtin.set_fact"]["scmig_ops_request"]
        .as_str()
        .unwrap();
    for part in [
        "'schemaVersion': 1",
        "'selection': scpub_ops_selection",
        "'artifactId': scpub_ops_artifact_id",
        "'output': scmig_evidence ~ '/ops'",
        "'authentication': 'gh-token'",
        "{'artifactZip': scpub_ops_artifact_zip}",
        "| to_json",
    ] {
        assert!(request.contains(part), "{part}");
    }
    let retained = json!({"content": "{{ scmig_ops_request }}",
        "dest": "{{ scmig_evidence }}/ops-request.json", "mode": "0600", "force": false});
    assert_eq!(steps[5]["ansible.builtin.copy"], retained);
    let token = &steps[6];
    assert_eq!(
        token["when"],
        "lookup('ansible.builtin.env', 'GH_TOKEN') | length == 0"
    );
    assert_eq!(token["no_log"], true);
    let acquire = &steps[7];
    assert_eq!(
        strings(&acquire["ansible.builtin.command"]["argv"]),
        [
            "{{ scpub_ops_release_controller }}",
            "ops-artifact",
            "acquire",
            "--request",
            "{{ scmig_evidence }}/ops-request.json",
            "--request-sha256",
            "{{ scmig_ops_request | hash('sha256') }}",
        ]
    );
    let environment = acquire["environment"].as_object().unwrap();
    let keys: Vec<&str> = environment.keys().map(String::as_str).collect();
    assert_eq!(keys, ["GH_TOKEN", "GITHUB_TOKEN", "PATH"]);
    assert_eq!(environment["GITHUB_TOKEN"], "");
    let path = "{{ scpub_ops_gh_executable | realpath | dirname }}:/usr/bin:/bin";
    assert_eq!(environment["PATH"], path);
    assert_eq!(
        (&acquire["no_log"], &acquire["changed_when"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(acquire["register"], "scmig_ops_acquired");
    let held = "{{ scmig_evidence }}/ops/verified/simplestchat-ops";
    assert_eq!(steps[8]["ansible.builtin.stat"]["path"], held);
    assert_eq!(steps[8]["ansible.builtin.stat"]["follow"], false);
    let record = &steps[10]["ansible.builtin.copy"];
    assert_eq!(record["dest"], "{{ scmig_evidence }}/ops.json");
    assert_eq!(
        (&record["mode"], &record["force"]),
        (&json!("0600"), &json!(false))
    );
    for field in [
        "'path': scmig_evidence ~ '/ops/verified/simplestchat-ops'",
        "'sha256'",
        "'bytes'",
    ] {
        assert!(
            record["content"].as_str().unwrap().contains(field),
            "{field}"
        );
    }
    let always = tasks(&step["always"]);
    assert_eq!(always.len(), 1);
    assert_eq!(
        always[0]["ansible.builtin.set_fact"]["scmig_ops_credential"],
        json!({})
    );
    assert_eq!(always[0]["no_log"], true);
    // The observation and the retained bytes, as Ansible judges them.
    let digest = "e".repeat(64);
    let observed = |change: &dyn Fn(&mut Value)| {
        let mut observation = json!({
            "kind": "ops-artifact-acquisition-observation",
            "scope": "AuthenticatedRetainedBytesWithoutExecutionOrInstallation",
            "artifact": {"artifactId": 4321},
            "selection": selection()["scpub_ops_selection"],
            "verification": {"matchedVerifierPolicy": {"revision": "a".repeat(40),
                "buildRunId": 5678, "binarySha256": digest}}});
        change(&mut observation);
        json!({"rc": 0, "stdout": observation.to_string()})
    };
    let binary = |change: &dyn Fn(&mut Value)| {
        let mut stat = json!({"isreg": true, "islnk": false, "nlink": 1, "size": 22229136,
            "checksum": digest});
        change(&mut stat);
        json!({"stat": stat})
    };
    let parsed = steps[9]["vars"]["scmig_ops_observed"].clone();
    let case = |acquired: Value, stat: Value| {
        let mut values = selection();
        values.insert("scmig_ops_acquired".into(), acquired);
        values.insert("scmig_ops_observed".into(), parsed.clone());
        values.insert("scmig_ops_binary".into(), stat);
        Value::Object(values)
    };
    let policy = "/verification/matchedVerifierPolicy";
    let cases = [
        (case(observed(&|_| {}), binary(&|_| {})), true),
        (
            case(
                observed(&|value| value["kind"] = json!("other")),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["scope"] = json!("Unverified")),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["artifact"]["artifactId"] = json!(4322)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["selection"]["runAttempt"] = json!(3)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| {
                    value.pointer_mut(policy).unwrap()["revision"] = json!("b".repeat(40))
                }),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value.pointer_mut(policy).unwrap()["buildRunId"] = json!(5679)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| {
                    value.pointer_mut(policy).unwrap()["binarySha256"] = json!("f".repeat(64))
                }),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["checksum"] = json!("f".repeat(64))),
            ),
            false,
        ),
        (
            case(observed(&|_| {}), binary(&|stat| stat["size"] = json!(0))),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["size"] = json!(268435457)),
            ),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["islnk"] = json!(true)),
            ),
            false,
        ),
        (
            case(observed(&|_| {}), binary(&|stat| stat["nlink"] = json!(2))),
            false,
        ),
        (
            case(
                json!({"rc": 1, "stdout": observed(&|_| {})["stdout"]}),
                binary(&|_| {}),
            ),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[9]["ansible.builtin.assert"]["that"], &variables);
    for (index, (accepted, (_, expected))) in verdicts.iter().zip(&cases).enumerate() {
        assert_eq!(accepted, expected, "case {index}");
    }
}
