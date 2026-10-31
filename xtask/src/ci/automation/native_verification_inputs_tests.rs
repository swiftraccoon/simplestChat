//! Native keeper admission precedes verification and every remote preflight.
use super::*;

const INPUTS: &str = "Require trusted native verification inputs before any remote preflight";
const INSPECT: &str = "Inspect the declared native GitHub executable on the controller";
const EXECUTABLE: &str = "Require the declared native GitHub executable before verification";
const ASSERTIONS: [&str; 9] = [
    "scpub_ops_xtask is defined",
    "scpub_ops_xtask is string",
    "scpub_ops_xtask is match('^/')",
    "scpub_ops_gh_executable is defined",
    "scpub_ops_gh_executable is string",
    "scpub_ops_gh_executable is match('^/')",
    "scpub_ops_gh_executable | basename == 'gh'",
    "scpub_ops_gh_executable | realpath | basename == 'gh'",
    "':' not in scpub_ops_gh_executable",
];
const FILE_ASSERTIONS: [&str; 3] = [
    "scpub_native_verification_gh.stat.isreg | default(false)",
    "not (scpub_native_verification_gh.stat.islnk | default(true))",
    "scpub_native_verification_gh.stat.executable | default(false)",
];
const RESULT_ASSERTIONS: [&str; 18] = [
    "scpub_native_validated_release is succeeded",
    "not (scpub_native_validated_release.skipped | default(false))",
    "scpub_native_validated_release.rc is defined",
    "scpub_native_validated_release.rc is integer",
    "scpub_native_validated_release.rc == 0",
    "scpub_native_validated_release.stdout is string",
    "scpub_native_verified_release is mapping",
    concat!(
        "scpub_native_verified_release.keys() | list | sort == ",
        "['directory', 'image.tar', 'release.json', 'revision']",
    ),
    "scpub_native_verified_release.revision is string",
    "scpub_native_verified_release.revision == scpub_release_expected_revision",
    "scpub_native_verified_release.directory is string",
    "scpub_native_verified_release.directory is match('^/')",
    "scpub_native_verified_release['image.tar'] is string",
    "scpub_native_verified_release['image.tar'] | length == 64",
    "scpub_native_verified_release['image.tar'] is match('^[a-f0-9]{64}$')",
    "scpub_native_verified_release['release.json'] is string",
    "scpub_native_verified_release['release.json'] | length == 64",
    "scpub_native_verified_release['release.json'] is match('^[a-f0-9]{64}$')",
];

pub(super) fn result_contract(pre: &[Value]) -> Check<usize> {
    let (at, result) = named(pre, RESULT)?;
    default_failure(result)?;
    require(
        result["when"] == SELECTED
            && result["vars"]
                == json!({
                    "scpub_native_verified_release":
                        "{{ scpub_native_validated_release.stdout | from_json }}"
                }),
        "actual-native-output-only",
    )?;
    assertions(result, &RESULT_ASSERTIONS)?;
    Ok(at)
}

pub(super) fn contract(pre: &[Value], choice_at: usize, native_at: usize) -> Check<()> {
    let (input_at, inputs) = named(pre, INPUTS)?;
    assertions(inputs, &ASSERTIONS)?;
    let (inspect_at, inspect) = named(pre, INSPECT)?;
    local(inspect)?;
    require(
        inspect["ansible.builtin.stat"]
            == json!({
                "path":"{{ scpub_ops_gh_executable | realpath }}",
                "follow":false,"get_checksum":false,"get_mime":false,"get_attributes":false
            })
            && inspect["register"] == "scpub_native_verification_gh"
            && inspect["changed_when"] == false
            && inspect["check_mode"] == false
            && inspect["timeout"] == 15,
        "explicit-current-keeper",
    )?;
    let (exec_at, executable) = named(pre, EXECUTABLE)?;
    assertions(executable, &FILE_ASSERTIONS)?;
    for task in [inputs, inspect, executable] {
        default_failure(task)?;
        require(task["when"] == SELECTED, "native-keeper-branch")?;
    }
    for task in pre {
        if task["register"] == "scpub_native_verification_gh" {
            require(task["name"] == INSPECT, "one-current-keeper-writer")?;
        }
    }
    require(
        choice_at < input_at
            && input_at < inspect_at
            && inspect_at < exec_at
            && exec_at < native_at,
        "native-keeper-admission-order",
    )
}

#[test]
fn every_native_path_and_executable_check_is_required() {
    let original = original_release();
    for (name, checks) in [
        (INPUTS, ASSERTIONS.as_slice()),
        (EXECUTABLE, FILE_ASSERTIONS.as_slice()),
    ] {
        for at in 0..checks.len() {
            for remove in [false, true] {
                let mut changed = original.clone();
                let values =
                    task_mut(&mut changed, "pre_tasks", name)["ansible.builtin.assert"]["that"]
                        .as_array_mut()
                        .unwrap();
                if remove {
                    values.remove(at);
                } else {
                    values[at] = json!(true);
                }
                assert!(super::contract(&changed).is_err(), "{name}: {at}: {remove}");
            }
        }
    }
}

#[test]
fn native_keeper_cannot_be_missing_duplicated_delayed_or_selected_by_search_fallback() {
    let original = original_release();
    for name in [INPUTS, INSPECT, EXECUTABLE] {
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
            assert!(super::contract(&changed).is_err(), "{name}: {duplicate}");
        }
        let mut changed = original.clone();
        let pre = changed[0]["pre_tasks"].as_array_mut().unwrap();
        let keeper = named(pre, name).unwrap().0;
        let native = named(pre, NATIVE).unwrap().0;
        pre.swap(keeper, native);
        assert!(super::contract(&changed).is_err(), "{name}: late");
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", name)["when"] = json!(false);
        assert!(super::contract(&changed).is_err(), "{name}: skipped");
    }
    for replacement in [
        json!({}),
        json!({"PATH":"/usr/bin:/bin"}),
        json!({"PATH":"{{ lookup('env', 'PATH') }}"}),
        json!({"PATH":"{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin",
            "GH_TOKEN":"saved-or-default-token"}),
    ] {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", NATIVE)["environment"] = replacement;
        assert!(super::contract(&changed).is_err());
    }
}

#[test]
fn keeper_stat_and_local_observation_cannot_be_substituted_or_suppressed() {
    let original = original_release();
    for (field, replacement) in [
        (
            "path",
            json!("{{ scpub_ops_gh_executable | dirname }}/other-gh"),
        ),
        ("follow", json!(true)),
        ("get_checksum", json!(true)),
        ("get_mime", json!(true)),
        ("get_attributes", json!(true)),
    ] {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", INSPECT)["ansible.builtin.stat"][field] = replacement;
        assert!(super::contract(&changed).is_err(), "{field}");
    }
    for (field, replacement) in [
        ("delegate_to", json!("public_vps")),
        ("become", json!(true)),
        ("register", json!("saved_keeper_result")),
        ("check_mode", json!(true)),
        ("timeout", json!(0)),
    ] {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", INSPECT)[field] = replacement;
        assert!(super::contract(&changed).is_err(), "{field}");
    }
}

#[test]
fn every_completed_status_and_exact_result_projection_check_is_required() {
    let original = original_release();
    for at in 0..RESULT_ASSERTIONS.len() {
        for remove in [false, true] {
            let mut changed = original.clone();
            let values =
                task_mut(&mut changed, "pre_tasks", RESULT)["ansible.builtin.assert"]["that"]
                    .as_array_mut()
                    .unwrap();
            if remove {
                values.remove(at);
            } else {
                values[at] = json!(true);
            }
            assert!(super::contract(&changed).is_err(), "{at}: {remove}");
        }
    }
    for binding in [
        "{{ scpub_validated_release.stdout | from_json }}",
        "{{ scpub_native_validated_release.stdout | default('{}') | from_json }}",
        "{{ {'revision': scpub_release_expected_revision} }}",
    ] {
        let mut changed = original.clone();
        task_mut(&mut changed, "pre_tasks", RESULT)["vars"]["scpub_native_verified_release"] =
            json!(binding);
        assert!(super::contract(&changed).is_err(), "{binding}");
    }
}
