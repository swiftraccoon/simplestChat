//! Current explicit deploy caller contracts. These inspect actual YAML and shared
//! actions; they do not interpret Jinja, authenticate a payload or operate a host.
//! API review: C-SERDE/C-VALIDATE use the bounded duplicate-rejecting YAML decoder;
//! C-GOOD-ERR keeps policy refusal distinct from actual command success.

use super::*;

const INPUTS: &str = "Require exact native deployment inputs before any remote preflight";
const DEPLOY: &str = "Replace only the application when deployment was explicitly selected";
const SELECTED: &str = "scpub_release_deploy | default(false) | bool";
const INPUT_ASSERTIONS: [&str; 36] = [
    "scpub_release_repository == 'swiftraccoon/simplestChat'",
    "scpub_release_artifact_id is integer",
    "scpub_release_artifact_id > 0",
    "scpub_release_artifact_id <= 9007199254740991",
    "scpub_release_ci_run is integer",
    "scpub_release_ci_run > 0",
    "scpub_release_ci_run <= 9007199254740991",
    "scpub_ops_artifact_id is defined",
    "scpub_ops_artifact_id is integer",
    "scpub_ops_artifact_id > 0",
    "scpub_ops_artifact_id <= 9007199254740991",
    "scpub_ops_selection is defined",
    "scpub_ops_selection is mapping",
    concat!(
        "scpub_ops_selection.keys() | list | sort == ",
        "['buildRunId', 'packageVersion', 'protocolVersion', 'repository', ",
        "'revision', 'runAttempt', 'target']",
    ),
    "scpub_ops_selection.repository == scpub_release_repository",
    "scpub_ops_selection.revision == scpub_release_expected_revision",
    "scpub_ops_selection.packageVersion == '0.1.0'",
    "scpub_ops_selection.protocolVersion is integer",
    "scpub_ops_selection.protocolVersion == 1",
    "scpub_ops_selection.target == 'x86_64-unknown-linux-musl'",
    "scpub_ops_selection.buildRunId is integer",
    "scpub_ops_selection.buildRunId == scpub_release_ci_run",
    "scpub_ops_selection.runAttempt is integer",
    "scpub_ops_selection.runAttempt > 0",
    "scpub_ops_selection.runAttempt <= 9007199254740991",
    "scpub_ops_xtask is defined",
    "scpub_ops_xtask is string",
    "scpub_ops_xtask is match('^/')",
    "scpub_ops_gh_executable is defined",
    "scpub_ops_gh_executable is string",
    "scpub_ops_gh_executable is match('^/')",
    "scpub_ops_gh_executable | realpath | basename == 'gh'",
    "scpub_release_quiet_seconds | default(600) is integer",
    "scpub_release_quiet_seconds | default(600) >= 0",
    "scpub_release_quiet_seconds | default(600) <= 600",
    concat!(
        "scpub_ops_artifact_zip is not defined or ",
        "(scpub_ops_artifact_zip is string and scpub_ops_artifact_zip is match('^/'))",
    ),
];

fn selected() -> Value {
    json!(["not ansible_check_mode", SELECTED])
}

fn routine_contract(document: &Value, shared: &Value) -> Check<()> {
    release_contract(document)?;
    let p = play(document)?;
    require(
        p["gather_facts"] == false
            && p["vars"]["ansible_pipelining"] == true
            && p["vars"].get("ansible_python_interpreter").is_none(),
        "facts-free-routine",
    )?;
    let pre = sequence(&p["pre_tasks"])?;
    let (_, host) = named(
        pre,
        "Require an explicitly selected artifact and prepared public host",
    )?;
    assertions(
        host,
        &[
            "scpub_enabled | bool",
            "scpub_release_directory is not defined",
            "scpub_release_artifact_id is defined",
            "scpub_config == '/etc/simplestchat-public'",
            "scpub_root == '/srv/simplestchat-public'",
        ],
    )?;
    let (input_at, inputs) = named(pre, INPUTS)?;
    default_failure(inputs)?;
    require(inputs["when"] == SELECTED, "explicit-inputs")?;
    assertions(inputs, &INPUT_ASSERTIONS)?;
    let (verify_at, _) = named(
        pre,
        "Verify the signed release on the controller before any remote preflight",
    )?;
    let (preflight_at, preflight) = named(
        pre,
        "Verify the host and exact prepared helpers without broad fact gathering",
    )?;
    require(
        input_at < verify_at
            && verify_at < preflight_at
            && preflight["check_mode"] == false
            && preflight["changed_when"] == false
            && preflight["timeout"] == 60
            && preflight["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && arguments(preflight)?.starts_with(&["/usr/bin/python3", "-B", "-c"]),
        "inputs-before-remote",
    )?;
    let tasks = sequence(&p["tasks"])?;
    let (_, stage) = named(
        tasks,
        "Import and verify the image while the public service remains running",
    )?;
    require(
        stage["when"] == "not ansible_check_mode"
            && stage["changed_when"] == true
            && stage["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && arguments(stage)?
                == [
                    "systemd-run",
                    concat!(
                        "--unit=simplestchat-stage-{{ scpub_release_revision[:12] }}-",
                        "{{ (scpub_release_preflight.stdout | from_json).runId }}",
                    ),
                    "--property=Type=exec",
                    "--property=RuntimeMaxSec=600",
                    "--property=TimeoutStopSec=60",
                    "--wait",
                    "/usr/bin/python3",
                    "-B",
                    "/usr/local/libexec/simplestchat-public/release-public.py",
                    "stage",
                    "{{ scpub_release_revision }}",
                ],
        "unchanged-stage-only-command",
    )?;
    let (retained_at, _) = named(tasks, PREDICATE_CUSTODY_TASKS[6])?;
    let (delivery_at, _) = named(tasks, DELIVERY)?;
    let (deploy_at, deploy) = named(tasks, DEPLOY)?;
    require(
        retained_at + 1 == delivery_at
            && delivery_at + 1 == deploy_at
            && deploy_at + 1 == tasks.len(),
        "stage-delivery-deploy-order",
    )?;
    default_failure(deploy)?;
    require(
        deploy["when"] == selected()
            && deploy["changed_when"] == true
            && deploy["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && deploy["environment"]
                == json!({
                    "PATH":"/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN":"", "GITHUB_TOKEN":""
                })
            && arguments(deploy)?
                == [
                    "systemd-run",
                    concat!(
                        "--unit=simplestchat-release-{{ scpub_release_revision[:12] }}-",
                        "{{ (scpub_release_preflight.stdout | from_json).runId }}",
                    ),
                    "--property=Type=exec",
                    "--property=RuntimeMaxSec=1800",
                    "--property=TimeoutStopSec=240",
                    "--wait",
                    "/usr/local/libexec/simplestchat-public/simplestchat-ops",
                    "release",
                    "deploy",
                    "--revision",
                    "{{ scpub_release_revision }}",
                    "--quiet-seconds",
                    "{{ scpub_release_quiet_seconds | default(600) }}",
                    "--timeout-seconds",
                    "1800",
                    "--finalization-seconds",
                    "60",
                ],
        "canonical-native-deploy",
    )?;
    let expanded = expand_signed_delivery(document, shared, "tasks", &selected())?;
    let (_, delivery) = named(sequence(&expanded[0]["tasks"])?, DELIVERY)?;
    signed_delivery_contract(delivery)?;
    for task in pre.iter().chain(tasks) {
        default_failure(task)?;
        require(
            task.get("block").is_none()
                && task.get("rescue").is_none()
                && task.get("always").is_none(),
            "no-routine-recovery-bypass",
        )?;
        let modules = task
            .as_object()
            .ok_or("task-object")?
            .keys()
            .filter(|key| key.starts_with("ansible.builtin."))
            .collect::<Vec<_>>();
        require(
            modules.len() == 1
                && [
                    "ansible.builtin.assert",
                    "ansible.builtin.command",
                    "ansible.builtin.copy",
                    "ansible.builtin.file",
                    "ansible.builtin.set_fact",
                    "ansible.builtin.stat",
                    "ansible.builtin.import_tasks",
                ]
                .contains(&modules[0].as_str()),
            "no-host-maintenance-modules",
        )?;
        if let Some(module) = task.get("ansible.builtin.command") {
            let argv = sequence(&module["argv"])?;
            require(
                argv.iter().all(|arg| {
                    ![
                        "apt", "reboot", "stop", "restart", "up", "down", "build", "pull",
                    ]
                    .iter()
                    .any(|forbidden| arg.as_str() == Some(*forbidden))
                }),
                "no-host-maintenance-commands",
            )?;
        }
        if task.get("ansible.builtin.import_tasks").is_some() {
            require(task["name"] == DELIVERY, "one-static-delivery-import")?;
        }
    }
    Ok(())
}

fn shared_task_mut<'a>(shared: &'a mut Value, name: &str) -> &'a mut Value {
    let (at, _) = named(sequence(&shared[0]["block"]).unwrap(), name).unwrap();
    &mut shared[0]["block"][at]
}

#[test]
fn explicit_routine_deploy_uses_the_actual_authenticated_shared_delivery() {
    routine_contract(&original_release(), &original_shared_delivery()).unwrap();
}

#[test]
fn every_early_typed_selector_and_quiet_bound_precedes_any_remote_preflight() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for at in 0..INPUT_ASSERTIONS.len() {
        for remove in [false, true] {
            let mut changed = caller.clone();
            let checks =
                task_mut(&mut changed, "pre_tasks", INPUTS)["ansible.builtin.assert"]["that"]
                    .as_array_mut()
                    .unwrap();
            if remove {
                checks.remove(at);
            } else {
                checks[at] = json!(true);
            }
            assert!(
                routine_contract(&changed, &shared).is_err(),
                "{at}: {remove}"
            );
        }
    }
    let mut changed = caller;
    let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
    let inputs = named(pre, INPUTS).unwrap().0;
    let preflight = named(
        pre,
        "Verify the host and exact prepared helpers without broad fact gathering",
    )
    .unwrap()
    .0;
    pre.swap(inputs, preflight);
    assert!(routine_contract(&changed, &shared).is_err());
}

#[test]
fn stage_only_default_check_mode_and_real_failure_status_cannot_be_bypassed() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for (section, name) in [
        ("pre_tasks", INPUTS),
        ("tasks", DELIVERY),
        ("tasks", DEPLOY),
    ] {
        for (field, value) in [
            ("when", json!(true)),
            ("ignore_errors", json!(true)),
            ("failed_when", json!(false)),
            ("async", json!(1800)),
        ] {
            let mut changed = caller.clone();
            task_mut(&mut changed, section, name)[field] = value;
            assert!(
                routine_contract(&changed, &shared).is_err(),
                "{name}: {field}"
            );
        }
    }
    for name in [DELIVERY, DEPLOY] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", name)["when"] = json!([SELECTED]);
        assert!(routine_contract(&changed, &shared).is_err(), "{name}");
    }
}

#[test]
fn native_revision_quiet_deadlines_tokens_and_delivery_order_are_fixed() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for (at, value) in [
        (3, "--property=RuntimeMaxSec=1801"),
        (4, "--property=TimeoutStopSec=0"),
        (6, "/usr/bin/python3"),
        (8, "maintain"),
        (10, "main"),
        (12, "{{ scpub_release_quiet_seconds | default(0) | int }}"),
        (14, "1801"),
        (16, "0"),
    ] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", DEPLOY)["ansible.builtin.command"]["argv"][at] =
            value.into();
        assert!(routine_contract(&changed, &shared).is_err(), "argv {at}");
    }
    for token in ["GH_TOKEN", "GITHUB_TOKEN"] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", DEPLOY)["environment"][token] = "inherited".into();
        assert!(routine_contract(&changed, &shared).is_err(), "{token}");
    }
    let mut changed = caller;
    let tasks = changed[0]["tasks"].as_array_mut().unwrap();
    let delivery = named(tasks, DELIVERY).unwrap().0;
    let retained = named(tasks, PREDICATE_CUSTODY_TASKS[6]).unwrap().0;
    tasks.swap(delivery, retained);
    assert!(routine_contract(&changed, &shared).is_err());
}

#[test]
fn both_callers_require_the_literal_shared_import_without_new_execution_authority() {
    let shared = original_shared_delivery();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let maintenance =
        parse(&std::fs::read(root.join("ops/ansible/maintenance.yml")).unwrap()).unwrap();
    let release = original_release();
    for (caller, section, condition) in [
        (maintenance, "pre_tasks", json!("not ansible_check_mode")),
        (release, "tasks", selected()),
    ] {
        expand_signed_delivery(&caller, &shared, section, &condition).unwrap();
        for (field, value) in [
            (
                "ansible.builtin.import_tasks",
                json!("{{ selected_tasks }}"),
            ),
            ("delegate_to", json!("localhost")),
            ("when", json!(true)),
            ("ignore_errors", json!(true)),
        ] {
            let mut changed = caller.clone();
            task_mut(&mut changed, section, DELIVERY)[field] = value;
            assert!(expand_signed_delivery(&changed, &shared, section, &condition).is_err());
        }
        for field in ["when", "rescue", "delegate_to"] {
            let mut changed = shared.clone();
            changed[0][field] = json!(true);
            assert!(expand_signed_delivery(&caller, &changed, section, &condition).is_err());
        }
    }
}

#[test]
fn routine_delivery_retains_all_authenticated_identity_and_transport_checks() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for name in [
        "Match the exact operation revision and trusted controller paths",
        "Require protected original application identity files",
        "Join staged application bytes and exact Ops selection before export",
        "Require the exact Rust-produced three-member transport",
        "Require the native authenticated candidate pin",
    ] {
        let count = shared_task_mut(&mut shared.clone(), name)["ansible.builtin.assert"]["that"]
            .as_array()
            .unwrap()
            .len();
        for at in 0..count {
            let mut changed = shared.clone();
            shared_task_mut(&mut changed, name)["ansible.builtin.assert"]["that"][at] = json!(true);
            assert!(routine_contract(&caller, &changed).is_err(), "{name}: {at}");
        }
    }
    for (name, field, value) in [
        (
            "Read the user's gh token only when GH_TOKEN was not supplied",
            "no_log",
            json!(false),
        ),
        (
            "Authenticate and export the exact signed pair through Rust",
            "failed_when",
            json!(false),
        ),
        (
            "Install or upgrade through the authenticated candidate and native checks",
            "ignore_errors",
            json!(true),
        ),
    ] {
        let mut changed = shared.clone();
        shared_task_mut(&mut changed, name)[field] = value;
        assert!(
            routine_contract(&caller, &changed).is_err(),
            "{name}: {field}"
        );
    }
    let mut changed = shared;
    changed[0]["always"][0]["ansible.builtin.set_fact"]["scpub_ops_credential"] = "retained".into();
    assert!(routine_contract(&caller, &changed).is_err());
}

#[test]
fn optional_retained_zip_never_becomes_an_implicit_fallback_or_receipt_authority() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for (before, after) in [
        (
            "if scpub_ops_artifact_zip is defined else {}",
            "if true else {}",
        ),
        (
            "'artifactZip': scpub_ops_artifact_zip",
            "'artifactZip': scpub_ops_artifact_zip | default('/tmp/cached-ops.zip')",
        ),
        (
            "'artifactZip': scpub_ops_artifact_zip",
            "'authenticated': true",
        ),
        (
            "'artifactZip': scpub_ops_artifact_zip",
            "'artifactZip': scpub_ops_receipt.transfer.path",
        ),
    ] {
        let mut changed = shared.clone();
        let request = &mut shared_task_mut(
            &mut changed,
            "Serialize one original request for the strict native decoder",
        )["ansible.builtin.set_fact"]["scpub_ops_request"];
        let original = request.as_str().unwrap();
        let replacement = original.replace(before, after);
        assert_ne!(replacement, original);
        *request = replacement.into();
        assert!(routine_contract(&caller, &changed).is_err(), "{after}");
    }
}
