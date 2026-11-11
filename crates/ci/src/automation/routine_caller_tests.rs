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
    "scpub_ops_release_controller is defined",
    "scpub_ops_release_controller is string",
    "scpub_ops_release_controller is match('^/')",
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

const NATIVE: &str =
    "Verify the signed release through the native controller before any remote preflight";
const VERIFIED_IDENTITY_TASKS: [&str; 4] = [
    "Snapshot the controller-verified application predicate",
    "Snapshot the controller-verified application manifest",
    "Require the verified application originals before any Ops delivery",
    "Read the controller-verified application predicate",
];
const VERIFIED_MANIFEST: &str =
    "{{ (scpub_validated_release.stdout | from_json).directory }}/release.json";
const PREFLIGHT: &str =
    "Verify the host through the delivered Ops executable without broad fact gathering";
const IDENTITY: &str = "Require a fresh facts-free release unit identity";
const STORAGE: &str = "Create private immutable release storage";
const FETCH: &str = "Verify the exact GitHub release and fetch it directly onto the prepared host";
const STAGE: &str = "Import and verify the image while the public service remains running";
const OPS: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
const CURRENT: &str = "not ansible_check_mode";

/// The installed executable runs with a fixed search path and no GitHub token.
fn host_environment() -> Value {
    json!({"PATH":"/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN":"", "GITHUB_TOKEN":""})
}

/// The routine caller registers the originals its controller just verified,
/// first of all tasks, so the delivery joins them before the host holds any
/// part of this release.
fn verified_identity_contract(tasks: &[Value]) -> Check<()> {
    let (predicate_at, predicate) = named(tasks, VERIFIED_IDENTITY_TASKS[0])?;
    stat(predicate, VERIFIED_PREDICATE, "scpub_app_predicate_file")?;
    let (manifest_at, manifest) = named(tasks, VERIFIED_IDENTITY_TASKS[1])?;
    stat(manifest, VERIFIED_MANIFEST, "scpub_app_manifest_file")?;
    let (profile_at, profile) = named(tasks, VERIFIED_IDENTITY_TASKS[2])?;
    assertions(
        profile,
        &[
            "scpub_app_predicate_file.stat.isreg | default(false)",
            "not (scpub_app_predicate_file.stat.islnk | default(false))",
            "scpub_app_predicate_file.stat.nlink == 1",
            "scpub_app_predicate_file.stat.size > 0",
            "scpub_app_predicate_file.stat.size <= 67108864",
            "scpub_app_predicate_file.stat.checksum is match('^[a-f0-9]{64}$')",
            "scpub_app_manifest_file.stat.isreg | default(false)",
            "not (scpub_app_manifest_file.stat.islnk | default(false))",
            concat!(
                "scpub_app_manifest_file.stat.checksum == ",
                "(scpub_validated_release.stdout | from_json)['release.json']"
            ),
        ],
    )?;
    let (read_at, read) = named(tasks, VERIFIED_IDENTITY_TASKS[3])?;
    require(
        expression(&read["ansible.builtin.slurp"]["src"], VERIFIED_PREDICATE)
            && read["register"] == "scpub_app_predicate_bytes"
            && read["no_log"] == true,
        "predicate-read",
    )?;
    for task in [predicate, manifest, read] {
        local(task)?;
    }
    for task in [predicate, manifest, profile, read] {
        default_failure(task)?;
        require(task["when"] == CURRENT, "verified-identity-condition")?;
    }
    let (delivery_at, _) = named(tasks, DELIVERY)?;
    require(
        predicate_at == 0
            && manifest_at == 1
            && profile_at == 2
            && read_at == 3
            && delivery_at == 4,
        "verified-identity-order",
    )?;
    one_identity_writer(tasks)
}

/// Prohibitions on every routine task, checked before the ordered contract so
/// a foreign task is refused for what it does rather than for where it sits.
fn closed_routine_tasks(pre: &[Value], tasks: &[Value]) -> Check<()> {
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
                    "ansible.builtin.slurp",
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
        // The host downloads the release itself and checks it against the digests
        // the controller re-verified; no task copies the controller's release.json
        // and image.tar to the host, and no helper file is installed.
        require(
            task["loop"] != json!(["release.json", "image.tar"]),
            "no-controller-release-copy",
        )?;
        let copied = &task["ansible.builtin.copy"]["dest"];
        require(
            !copied
                .as_str()
                .is_some_and(|path| path.starts_with("/usr/local/libexec/")),
            "no-helper-installation",
        )?;
        if task.get("ansible.builtin.slurp").is_some() {
            require(
                task["name"] == VERIFIED_IDENTITY_TASKS[3],
                "one-controller-read",
            )?;
        }
    }
    Ok(())
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
    closed_routine_tasks(pre, sequence(&p["tasks"])?)?;
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
    // Staging runs the signed executable too, so its selection is always typed.
    require(inputs.get("when").is_none(), "explicit-inputs")?;
    assertions(inputs, &INPUT_ASSERTIONS)?;
    let (verify_at, _) = named(pre, NATIVE)?;
    require(input_at < verify_at, "inputs-before-verification")?;
    let tasks = sequence(&p["tasks"])?;
    verified_identity_contract(tasks)?;
    let (delivery_at, _) = named(tasks, DELIVERY)?;
    let (preflight_at, preflight) = named(tasks, PREFLIGHT)?;
    default_failure(preflight)?;
    require(
        preflight["when"] == CURRENT
            && preflight["register"] == "scpub_release_preflight"
            && preflight["changed_when"] == false
            && preflight["timeout"] == 60
            && preflight["environment"] == host_environment()
            && preflight["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && arguments(preflight)? == [OPS, "release", "preflight"],
        "native-preflight",
    )?;
    let (identity_at, identity) = named(tasks, IDENTITY)?;
    default_failure(identity)?;
    require(identity["when"] == CURRENT, "unit-identity-condition")?;
    assertions(
        identity,
        &[
            "(scpub_release_preflight.stdout | from_json).schemaVersion == 1",
            "(scpub_release_preflight.stdout | from_json).runId is match('^[a-f0-9]{32}$')",
        ],
    )?;
    let (storage_at, storage) = named(tasks, STORAGE)?;
    default_failure(storage)?;
    require(
        storage.get("when").is_none()
            && storage["ansible.builtin.file"]
                == json!({
                    "path":"{{ item }}", "state":"directory", "owner":"root",
                    "group":"root", "mode":"0700"
                })
            && storage["loop"]
                == json!([
                    "{{ scpub_root }}/releases",
                    "{{ scpub_root }}/releases/{{ scpub_release_revision }}"
                ]),
        "private-release-storage",
    )?;
    let (fetch_at, fetch) = named(tasks, FETCH)?;
    require(
        fetch["when"] == CURRENT
            && fetch["changed_when"] == true
            && fetch["timeout"] == 1080
            && fetch["environment"]
                == json!({
                    "PATH":"{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin"
                })
            && arguments(fetch)?
                == [
                    "{{ scpub_ops_release_controller }}",
                    "fetch",
                    "--repository",
                    "{{ scpub_release_repository }}",
                    "--revision",
                    "{{ scpub_release_revision }}",
                    "--artifact-id",
                    "{{ scpub_release_artifact_id }}",
                    "--ci-run",
                    "{{ scpub_release_ci_run }}",
                    "--host",
                    "{{ ansible_host }}",
                    "--user",
                    "{{ ansible_user }}",
                    "--identity",
                    "{{ ansible_ssh_private_key_file }}",
                    "--port",
                    "{{ ansible_port | default(22) }}",
                    "--output-parent",
                    "{{ (playbook_dir ~ '/../../results') | realpath }}",
                    "--verified-directory",
                    "{{ (scpub_validated_release.stdout | from_json).directory }}",
                    "--timeout-seconds",
                    "1020",
                    "--finalization-seconds",
                    "60",
                ],
        "native-fetch",
    )?;
    let (stage_at, stage) = named(tasks, STAGE)?;
    require(
        stage["when"] == CURRENT
            && stage["changed_when"] == true
            && stage["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && stage["environment"] == host_environment()
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
                    OPS,
                    "release",
                    "stage",
                    "--revision",
                    "{{ scpub_release_revision }}",
                    "--timeout-seconds",
                    "600",
                    "--finalization-seconds",
                    "60",
                ],
        "native-stage",
    )?;
    let (retained_at, _) = named(tasks, PREDICATE_CUSTODY_TASKS[6])?;
    let (deploy_at, deploy) = named(tasks, DEPLOY)?;
    require(
        delivery_at + 1 == preflight_at
            && preflight_at + 1 == identity_at
            && identity_at + 1 == storage_at
            && storage_at + 1 == fetch_at
            && fetch_at + 1 == stage_at
            && retained_at + 1 == deploy_at
            && deploy_at + 1 == tasks.len(),
        "delivery-preflight-fetch-stage-deploy-order",
    )?;
    default_failure(deploy)?;
    require(
        deploy["when"] == selected()
            && deploy["changed_when"] == true
            && deploy["vars"]["ansible_python_interpreter"] == "/usr/bin/python3"
            && deploy["environment"] == host_environment()
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
                    OPS,
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
    let expanded = expand_signed_delivery(document, shared, "tasks", &json!(CURRENT))?;
    let (_, delivery) = named(sequence(&expanded[0]["tasks"])?, DELIVERY)?;
    signed_delivery_contract(delivery)?;
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
fn routine_release_never_copies_the_controller_release_pair() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    // The host downloads the release itself and checks it against the digests
    // the controller re-verified; no task copies the controller's release.json
    // and image.tar to the host.
    let mut changed = caller;
    changed[0]["tasks"].as_array_mut().unwrap().insert(
        0,
        json!({
            "name": "Copy the controller release to the host",
            "ansible.builtin.copy": {
                "src": "{{ item }}",
                "dest": "{{ scpub_root }}/releases/{{ scpub_release_revision }}/{{ item }}",
                "mode": "0600",
            },
            "loop": ["release.json", "image.tar"],
        }),
    );
    assert_eq!(
        routine_contract(&changed, &shared),
        Err("no-controller-release-copy")
    );
}

#[test]
fn every_early_typed_selector_and_quiet_bound_precedes_verification() {
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
    let verify = named(pre, NATIVE).unwrap().0;
    pre.swap(inputs, verify);
    assert!(routine_contract(&changed, &shared).is_err());
}

#[test]
fn check_mode_deploy_selection_and_real_failure_status_cannot_be_bypassed() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for (section, name) in [
        ("pre_tasks", INPUTS),
        ("tasks", DELIVERY),
        ("tasks", PREFLIGHT),
        ("tasks", IDENTITY),
        ("tasks", FETCH),
        ("tasks", STAGE),
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
    // The inputs are typed for staging as well: deployment is not their condition.
    let mut changed = caller.clone();
    task_mut(&mut changed, "pre_tasks", INPUTS)["when"] = json!(SELECTED);
    assert_eq!(routine_contract(&changed, &shared), Err("explicit-inputs"));
    // The delivery is not a deployment option; only the replacement is.
    let mut changed = caller.clone();
    task_mut(&mut changed, "tasks", DELIVERY)["when"] = selected();
    assert!(routine_contract(&changed, &shared).is_err());
    let mut changed = caller;
    task_mut(&mut changed, "tasks", DEPLOY)["when"] = json!([SELECTED]);
    assert!(routine_contract(&changed, &shared).is_err());
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
    for name in [PREFLIGHT, STAGE, DEPLOY] {
        for token in ["GH_TOKEN", "GITHUB_TOKEN"] {
            let mut changed = caller.clone();
            task_mut(&mut changed, "tasks", name)["environment"][token] = "inherited".into();
            assert!(
                routine_contract(&changed, &shared).is_err(),
                "{name}: {token}"
            );
        }
    }
    // The delivery cannot move behind the steps that run what it installs.
    for later in [PREFLIGHT, FETCH, STAGE, PREDICATE_CUSTODY_TASKS[6]] {
        let mut changed = caller.clone();
        let tasks = changed[0]["tasks"].as_array_mut().unwrap();
        let delivery = named(tasks, DELIVERY).unwrap().0;
        let other = named(tasks, later).unwrap().0;
        tasks.swap(delivery, other);
        assert!(routine_contract(&changed, &shared).is_err(), "{later}");
    }
}

#[test]
fn host_steps_run_only_the_delivered_executable_with_fixed_budgets() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    let argv = |document: &Value, name: &str| -> usize {
        let tasks = sequence(&document[0]["tasks"]).unwrap();
        arguments(named(tasks, name).unwrap().1).unwrap().len()
    };
    for name in [PREFLIGHT, FETCH, STAGE] {
        for at in 0..argv(&caller, name) {
            let mut changed = caller.clone();
            task_mut(&mut changed, "tasks", name)["ansible.builtin.command"]["argv"][at] =
                json!("substituted");
            assert!(
                routine_contract(&changed, &shared).is_err(),
                "{name}: argv {at}"
            );
        }
    }
    for (name, field, value) in [
        (PREFLIGHT, "register", json!("saved_preflight")),
        (PREFLIGHT, "timeout", json!(600)),
        (PREFLIGHT, "changed_when", json!(true)),
        (FETCH, "delegate_to", json!("public_vps")),
        (FETCH, "become", json!(true)),
        (FETCH, "timeout", json!(10800)),
        (
            FETCH,
            "environment",
            json!({"PATH": "{{ lookup('env', 'PATH') }}"}),
        ),
        (STAGE, "changed_when", json!(false)),
        (STORAGE, "when", json!("scpub_release_prepared")),
    ] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", name)[field] = value;
        assert!(
            routine_contract(&changed, &shared).is_err(),
            "{name}: {field}"
        );
    }
    for at in 0..2 {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", IDENTITY)["ansible.builtin.assert"]["that"][at] =
            json!(true);
        assert!(
            routine_contract(&changed, &shared).is_err(),
            "identity {at}"
        );
    }
    // No helper file is copied into the executable directory any more.
    let mut changed = caller.clone();
    changed[0]["tasks"].as_array_mut().unwrap().insert(
        5,
        json!({
            "name": "Install a release helper",
            "ansible.builtin.copy": {
                "src": "release-public.py",
                "dest": "/usr/local/libexec/simplestchat-public/release-public.py",
            },
        }),
    );
    assert!(routine_contract(&changed, &shared).is_err());
    let mut changed = caller;
    changed[0]["tasks"].as_array_mut().unwrap().push(json!({
        "name": "Read another controller file",
        "ansible.builtin.slurp": {"src": "/tmp/other"},
        "delegate_to": "localhost",
        "become": false,
    }));
    assert!(routine_contract(&changed, &shared).is_err());
}

#[test]
fn verified_originals_are_registered_once_on_the_controller_before_the_delivery() {
    let caller = original_release();
    let shared = original_shared_delivery();
    routine_contract(&caller, &shared).unwrap();
    for name in VERIFIED_IDENTITY_TASKS {
        let mut changed = caller.clone();
        let tasks = changed[0]["tasks"].as_array_mut().unwrap();
        let at = named(tasks, name).unwrap().0;
        tasks.remove(at);
        assert!(routine_contract(&changed, &shared).is_err(), "{name}: gone");
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", name)["when"] = json!(false);
        assert!(
            routine_contract(&changed, &shared).is_err(),
            "{name}: skipped"
        );
    }
    for name in [
        VERIFIED_IDENTITY_TASKS[0],
        VERIFIED_IDENTITY_TASKS[1],
        VERIFIED_IDENTITY_TASKS[3],
    ] {
        for (field, value) in [
            ("delegate_to", json!("public_vps")),
            ("become", json!(true)),
        ] {
            let mut changed = caller.clone();
            task_mut(&mut changed, "tasks", name)[field] = value;
            assert!(
                routine_contract(&changed, &shared).is_err(),
                "{name}: {field}"
            );
        }
    }
    let count = 9;
    for at in 0..count {
        for remove in [false, true] {
            let mut changed = caller.clone();
            let checks = task_mut(&mut changed, "tasks", VERIFIED_IDENTITY_TASKS[2])
                ["ansible.builtin.assert"]["that"]
                .as_array_mut()
                .unwrap();
            assert_eq!(checks.len(), count);
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
    for (name, module, field, value) in [
        (
            VERIFIED_IDENTITY_TASKS[0],
            "ansible.builtin.stat",
            "path",
            json!("{{ scpub_root }}/releases/{{ scpub_release_revision }}/release-predicate.json"),
        ),
        (
            VERIFIED_IDENTITY_TASKS[1],
            "ansible.builtin.stat",
            "follow",
            json!(true),
        ),
        (
            VERIFIED_IDENTITY_TASKS[3],
            "ansible.builtin.slurp",
            "src",
            json!("/tmp/assumed-predicate.json"),
        ),
    ] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", name)[module][field] = value;
        assert!(routine_contract(&changed, &shared).is_err(), "{name}");
    }
    let mut changed = caller;
    let tasks = changed[0]["tasks"].as_array_mut().unwrap();
    let delivery = named(tasks, DELIVERY).unwrap().0;
    let read = named(tasks, VERIFIED_IDENTITY_TASKS[3]).unwrap().0;
    tasks.swap(delivery, read);
    assert!(routine_contract(&changed, &shared).is_err());
}

#[test]
fn both_callers_require_the_literal_shared_import_without_new_execution_authority() {
    let shared = original_shared_delivery();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();
    let maintenance =
        parse(&std::fs::read(root.join("ops/ansible/maintenance.yml")).unwrap()).unwrap();
    let release = original_release();
    let condition = json!(CURRENT);
    for (caller, section) in [(maintenance, "pre_tasks"), (release, "tasks")] {
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
        JOIN,
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
