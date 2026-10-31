//! Current caller selection and actual native verifier completion remain separate.
//! These mutation contracts inspect real YAML; Root's offline Ansible checks own
//! Jinja branch semantics. They perform no authentication or deployment.

use super::*;

#[path = "native_verification_inputs_tests.rs"]
mod inputs;

const CHOICE: &str = "Require a typed controller verification choice";
const NATIVE: &str =
    "Verify the signed release through the native controller before any remote preflight";
const REFERENCE: &str = "Verify the signed release on the controller before any remote preflight";
const NORMALIZE: &str = "Use the completed native controller verification result";
const RESULT: &str = "Require the completed native application verification result";
const PREFLIGHT: &str = "Verify the host and exact prepared helpers without broad fact gathering";
const SELECTED: &str = "scpub_release_native_verification | default(false)";
const REFERENCE_SELECTED: &str = "not (scpub_release_native_verification | default(false))";

fn verify_arguments(native: bool) -> Vec<&'static str> {
    let mut argv = if native {
        vec!["{{ scpub_ops_xtask }}", "release", "verify"]
    } else {
        vec![
            "{{ ansible_playbook_python }}",
            "-B",
            "{{ playbook_dir }}/../../build/verify-release.py",
        ]
    };
    argv.extend([
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
    ]);
    if native {
        argv.extend(["--timeout-seconds", "600", "--finalization-seconds", "60"]);
    }
    argv
}

fn completed_verifier(task: &Value, register: &str) -> Check<()> {
    local(task)?;
    default_failure(task)?;
    require(
        task["register"] == register
            && task["changed_when"] == false
            && task["check_mode"] == false
            && task["timeout"] == 660,
        "complete-controller-verifier",
    )
}

pub(super) fn contract(document: &Value) -> Check<()> {
    let p = play(document)?;
    default_failure(p)?;
    require(p.get("when").is_none(), "unconditional-verification-play")?;
    let pre = sequence(&p["pre_tasks"])?;
    let (choice_at, choice) = named(pre, CHOICE)?;
    default_failure(choice)?;
    require(choice.get("when").is_none(), "typed-choice-always")?;
    assertions(
        choice,
        &["scpub_release_native_verification | default(false) is boolean"],
    )?;
    let (native_at, native) = named(pre, NATIVE)?;
    inputs::contract(pre, choice_at, native_at)?;
    completed_verifier(native, "scpub_native_validated_release")?;
    require(
        native["when"] == SELECTED
            && arguments(native)? == verify_arguments(true)
            && native["environment"]
                == json!({
                    "PATH":"{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin"
                }),
        "native-verifier-branch",
    )?;
    let (reference_at, reference) = named(pre, REFERENCE)?;
    completed_verifier(reference, "scpub_validated_release")?;
    require(
        reference["when"] == REFERENCE_SELECTED && arguments(reference)? == verify_arguments(false),
        "reference-only-explicitly-selected",
    )?;
    let (normalize_at, normalize) = named(pre, NORMALIZE)?;
    let result_at = inputs::result_contract(pre)?;
    default_failure(normalize)?;
    require(
        normalize["when"]
            == json!([
                SELECTED,
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
    let (preflight_at, _) = named(pre, PREFLIGHT)?;
    require(
        choice_at < native_at
            && native_at < reference_at
            && reference_at + 1 == result_at
            && result_at + 1 == normalize_at
            && normalize_at < preflight_at,
        "verify-before-host-and-normalize-after-skipped-register",
    )?;
    for task in pre {
        default_failure(task)?;
        require(
            task.get("block").is_none()
                && task.get("rescue").is_none()
                && task.get("always").is_none()
                && [
                    "scpub_release_native_verification",
                    "scpub_native_validated_release",
                    "scpub_native_verification_gh",
                ]
                .iter()
                .all(|key| task["ansible.builtin.set_fact"].get(*key).is_none()),
            "no-verifier-recovery-or-selection-rewrite",
        )?;
        if task["register"] == "scpub_native_validated_release" {
            require(task["name"] == NATIVE, "one-native-result-writer")?;
        }
        if task["register"] == "scpub_validated_release" {
            require(task["name"] == REFERENCE, "one-reference-result-writer")?;
        }
        if task["ansible.builtin.set_fact"]
            .get("scpub_validated_release")
            .is_some()
        {
            require(task["name"] == NORMALIZE, "one-completed-result-writer")?;
        }
    }
    Ok(())
}

fn mutate_task(document: &mut Value, name: &str, field: &str, value: Value) {
    task_mut(document, "pre_tasks", name)[field] = value;
}

#[test]
fn current_native_or_reference_verifier_finishes_before_every_remote_preflight() {
    contract(&original_release()).unwrap();
}

#[test]
fn required_verification_tasks_cannot_be_missing_duplicated_or_reordered() {
    let original = original_release();
    contract(&original).unwrap();
    for name in [CHOICE, NATIVE, REFERENCE, RESULT, NORMALIZE] {
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
    for (first, second) in [
        (CHOICE, NATIVE),
        (NATIVE, REFERENCE),
        (REFERENCE, RESULT),
        (RESULT, NORMALIZE),
        (NORMALIZE, PREFLIGHT),
    ] {
        let mut changed = original.clone();
        let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
        let first_at = named(pre, first).unwrap().0;
        let second_at = named(pre, second).unwrap().0;
        pre.swap(first_at, second_at);
        assert!(contract(&changed).is_err(), "{first}: {second}");
    }
}

#[test]
fn native_selection_is_a_strict_boolean_and_never_a_failure_fallback() {
    let original = original_release();
    for (name, field, value) in [
        (CHOICE, "when", json!(SELECTED)),
        (NATIVE, "when", json!(true)),
        (
            REFERENCE,
            "when",
            json!("scpub_native_validated_release is failed"),
        ),
        (RESULT, "when", json!(true)),
        (NORMALIZE, "when", json!(SELECTED)),
        (
            NORMALIZE,
            "when",
            json!([SELECTED, "scpub_native_validated_release is defined"]),
        ),
    ] {
        let mut changed = original.clone();
        mutate_task(&mut changed, name, field, value);
        assert!(contract(&changed).is_err(), "{name}: {field}");
    }
    let mut changed = original;
    task_mut(&mut changed, "pre_tasks", CHOICE)["ansible.builtin.assert"]["that"] =
        json!(["scpub_release_native_verification | default(false) | bool"]);
    assert!(contract(&changed).is_err());
}

#[test]
fn exact_native_arguments_and_unchanged_outer_guard_cannot_be_weakened() {
    let original = original_release();
    for at in 0..verify_arguments(true).len() {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", NATIVE)["ansible.builtin.command"]["argv"][at] =
            json!("untrusted-or-stale");
        assert!(contract(&changed).is_err(), "native argv {at}");
    }
    for name in [NATIVE, REFERENCE] {
        for (field, value) in [
            ("register", json!("scpub_assumed_result")),
            ("delegate_to", json!("public_vps")),
            ("become", json!(true)),
            ("changed_when", json!(true)),
            ("check_mode", json!(true)),
            ("timeout", json!(661)),
        ] {
            let mut changed = original.clone();
            mutate_task(&mut changed, name, field, value);
            assert!(contract(&changed).is_err(), "{name}: {field}");
        }
    }
}

#[test]
fn actual_failure_and_one_completed_result_writer_are_required() {
    let original = original_release();
    for name in [CHOICE, NATIVE, REFERENCE, RESULT, NORMALIZE] {
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
    for source in [NATIVE, REFERENCE, NORMALIZE] {
        let mut changed = original.clone();
        let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
        let mut extra = named(pre, source).unwrap().1.clone();
        extra["name"] = json!("Adopt a saved or duplicate verification result");
        pre.push(extra);
        assert!(contract(&changed).is_err(), "{source}");
    }
    for key in [
        "scpub_release_native_verification",
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
}
