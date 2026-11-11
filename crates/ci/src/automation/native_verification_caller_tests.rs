//! The native controller verifier finishes before any host is contacted.
//! These mutation contracts inspect real YAML; Root's offline Ansible checks own
//! Jinja branch semantics. They perform no authentication or deployment.

use super::*;

#[path = "native_verification_inputs_tests.rs"]
mod inputs;

const NATIVE: &str =
    "Verify the signed release through the native controller before any remote preflight";
const NORMALIZE: &str = "Use the completed native controller verification result";
const RESULT: &str = "Require the completed native application verification result";
const REVISION: &str = "Select the explicitly pinned GitHub revision";

fn verify_arguments() -> Vec<&'static str> {
    vec![
        "{{ scpub_ops_release_controller }}",
        "verify",
        "--repository",
        "{{ scpub_release_repository }}",
        "--artifact-id",
        "{{ scpub_release_artifact_id }}",
        "--revision",
        "{{ scpub_release_expected_revision }}",
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
}

/// A pre-task reaches no host: it asserts, records a fact or runs on the
/// controller without privilege.
fn controller_only(task: &Value) -> bool {
    task.get("ansible.builtin.assert").is_some()
        || task.get("ansible.builtin.set_fact").is_some()
        || local(task).is_ok()
}

pub(super) fn contract(document: &Value) -> Check<()> {
    let p = play(document)?;
    default_failure(p)?;
    require(p.get("when").is_none(), "unconditional-verification-play")?;
    let pre = sequence(&p["pre_tasks"])?;
    let (native_at, native) = named(pre, NATIVE)?;
    inputs::contract(pre, native_at)?;
    local(native)?;
    default_failure(native)?;
    require(
        native.get("when").is_none()
            && native["register"] == "scpub_native_validated_release"
            && native["changed_when"] == false
            && native["check_mode"] == false
            && native["timeout"] == 660
            && arguments(native)? == verify_arguments()
            && native["environment"]
                == json!({
                    "PATH":"{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin"
                }),
        "native-verifier",
    )?;
    let (normalize_at, normalize) = named(pre, NORMALIZE)?;
    let result_at = inputs::result_contract(pre)?;
    default_failure(normalize)?;
    require(
        normalize["when"]
            == json!([
                "scpub_native_validated_release is succeeded",
                "not (scpub_native_validated_release.skipped | default(false))"
            ])
            && normalize["ansible.builtin.set_fact"]
                == json!({
                    "scpub_validated_release":"{{ scpub_native_validated_release }}",
                    "cacheable":false
                }),
        "completed-native-result",
    )?;
    let (revision_at, _) = named(pre, REVISION)?;
    require(
        native_at + 1 == result_at
            && result_at + 1 == normalize_at
            && normalize_at + 1 == revision_at
            && revision_at + 1 == pre.len(),
        "verify-then-adopt-as-the-last-pre-tasks",
    )?;
    for task in pre {
        default_failure(task)?;
        require(controller_only(task), "no-host-before-verification")?;
        require(
            task.get("block").is_none()
                && task.get("rescue").is_none()
                && task.get("always").is_none()
                && [
                    "scpub_native_validated_release",
                    "scpub_native_verification_gh",
                ]
                .iter()
                .all(|key| task["ansible.builtin.set_fact"].get(*key).is_none()),
            "no-verifier-recovery-or-result-rewrite",
        )?;
        if task["register"] == "scpub_native_validated_release" {
            require(task["name"] == NATIVE, "one-native-result-writer")?;
        }
        require(
            task["register"] != "scpub_validated_release",
            "no-unchecked-result-writer",
        )?;
        if task["ansible.builtin.set_fact"]
            .get("scpub_validated_release")
            .is_some()
        {
            require(task["name"] == NORMALIZE, "one-completed-result-writer")?;
        }
    }
    for task in sequence(&p["tasks"])? {
        require(
            task["register"] != "scpub_validated_release"
                && task["register"] != "scpub_native_validated_release"
                && task["ansible.builtin.set_fact"]
                    .get("scpub_validated_release")
                    .is_none(),
            "no-later-result-writer",
        )?;
        if let Some(module) = task.get("ansible.builtin.command") {
            let interpreter = sequence(&module["argv"])?.iter().any(|argument| {
                argument.as_str().is_some_and(|text| {
                    text.contains("python") || text.ends_with(".py") || text.contains(".py ")
                })
            });
            require(!interpreter, "no-interpreted-release-step")?;
        }
    }
    Ok(())
}

fn mutate_task(document: &mut Value, name: &str, field: &str, value: Value) {
    task_mut(document, "pre_tasks", name)[field] = value;
}

#[test]
fn the_native_verifier_finishes_before_any_host_is_contacted() {
    contract(&original_release()).unwrap();
}

#[test]
fn required_verification_tasks_cannot_be_missing_duplicated_or_reordered() {
    let original = original_release();
    contract(&original).unwrap();
    for name in [NATIVE, RESULT, NORMALIZE] {
        for duplicate in [false, true] {
            let mut changed = original.clone();
            let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
            let (at, task) = named(pre, name).unwrap();
            if duplicate {
                let extra = task.clone();
                pre.insert(at, extra);
            } else {
                pre.remove(at);
            }
            assert!(contract(&changed).is_err(), "{name}: {duplicate}");
        }
    }
    for (first, second) in [(NATIVE, RESULT), (RESULT, NORMALIZE), (NORMALIZE, REVISION)] {
        let mut changed = original.clone();
        let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
        let first_at = named(pre, first).unwrap().0;
        let second_at = named(pre, second).unwrap().0;
        pre.swap(first_at, second_at);
        assert!(contract(&changed).is_err(), "{first}: {second}");
    }
}

#[test]
fn verification_is_unconditional_and_has_no_interpreted_alternative() {
    let original = original_release();
    for (name, field, value) in [
        (
            NATIVE,
            "when",
            json!("scpub_release_native_verification | default(false)"),
        ),
        (NATIVE, "when", json!(false)),
        (RESULT, "when", json!(false)),
        (
            NORMALIZE,
            "when",
            json!("scpub_native_validated_release is succeeded"),
        ),
        (
            NORMALIZE,
            "when",
            json!("scpub_native_validated_release is defined"),
        ),
    ] {
        let mut changed = original.clone();
        mutate_task(&mut changed, name, field, value);
        assert!(contract(&changed).is_err(), "{name}: {field}");
    }
    // The retired interpreted verifier wrote the adopted fact directly.
    let mut changed = original.clone();
    let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
    let (at, native) = named(pre, NATIVE).unwrap();
    let mut reference = native.clone();
    reference["name"] = json!("Verify the signed release on the controller");
    reference["register"] = json!("scpub_validated_release");
    reference["ansible.builtin.command"]["argv"] = json!([
        "{{ ansible_playbook_python }}",
        "-B",
        "{{ playbook_dir }}/../../build/verify-release.py"
    ]);
    pre.insert(at, reference);
    assert!(contract(&changed).is_err());
    // No later task runs an interpreter or a script on either side.
    for argv in [
        json!(["/usr/bin/python3", "-B", "/usr/local/libexec/x"]),
        json!([
            "{{ ansible_playbook_python }}",
            "{{ playbook_dir }}/../../build/fetch-release.py"
        ]),
    ] {
        let mut changed = original.clone();
        changed[0]["tasks"].as_array_mut().unwrap().push(json!({
            "name": "Run an interpreted release step",
            "ansible.builtin.command": {"argv": argv},
        }));
        assert_eq!(contract(&changed), Err("no-interpreted-release-step"));
    }
    // A host is not contacted before verification completes.
    let mut changed = original;
    changed[0]["pre_tasks"].as_array_mut().unwrap().insert(
        0,
        json!({
            "name": "Probe the host first",
            "ansible.builtin.command": {"argv": ["/usr/bin/true"]},
            "changed_when": false,
        }),
    );
    assert_eq!(contract(&changed), Err("no-host-before-verification"));
}

#[test]
fn exact_native_arguments_and_unchanged_outer_guard_cannot_be_weakened() {
    let original = original_release();
    for at in 0..verify_arguments().len() {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", NATIVE)["ansible.builtin.command"]["argv"][at] =
            json!("untrusted-or-stale");
        assert!(contract(&changed).is_err(), "native argv {at}");
    }
    for (field, value) in [
        ("register", json!("scpub_assumed_result")),
        ("delegate_to", json!("public_vps")),
        ("become", json!(true)),
        ("changed_when", json!(true)),
        ("check_mode", json!(true)),
        ("timeout", json!(661)),
    ] {
        let mut changed = original.clone();
        mutate_task(&mut changed, NATIVE, field, value);
        assert!(contract(&changed).is_err(), "{field}");
    }
}

#[test]
fn actual_failure_and_one_completed_result_writer_are_required() {
    let original = original_release();
    for name in [NATIVE, RESULT, NORMALIZE] {
        for (field, value) in [
            ("ignore_errors", json!(true)),
            ("failed_when", json!(false)),
            ("ignore_unreachable", json!(true)),
            ("async", json!(600)),
            ("rescue", json!([])),
        ] {
            let mut changed = original.clone();
            mutate_task(&mut changed, name, field, value);
            assert!(contract(&changed).is_err(), "{name}: {field}");
        }
    }
    for source in [NATIVE, NORMALIZE] {
        let mut changed = original.clone();
        let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
        let mut extra = named(pre, source).unwrap().1.clone();
        extra["name"] = json!("Adopt a saved or duplicate verification result");
        pre.push(extra);
        assert!(contract(&changed).is_err(), "{source}");
    }
    for key in [
        "scpub_native_validated_release",
        "scpub_native_verification_gh",
    ] {
        let mut changed = original.clone();
        let mut fake = json!({"name":"Overwrite a current verification observation",
            "ansible.builtin.set_fact":{}});
        fake["ansible.builtin.set_fact"][key] = json!(true);
        changed[0]["pre_tasks"].as_array_mut().unwrap().push(fake);
        assert!(contract(&changed).is_err(), "{key}");
    }
    for replacement in [
        json!("{{ scpub_validated_release }}"),
        json!({"stdout":"saved positive JSON","rc":0}),
    ] {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", NORMALIZE)["ansible.builtin.set_fact"]["scpub_validated_release"] =
            replacement;
        assert!(contract(&changed).is_err());
    }
    let mut changed = original;
    changed[0]["tasks"].as_array_mut().unwrap().push(json!({
        "name": "Adopt a later verification result",
        "ansible.builtin.set_fact": {"scpub_validated_release": {"stdout": "{}"}},
    }));
    assert_eq!(contract(&changed), Err("no-later-result-writer"));
}
