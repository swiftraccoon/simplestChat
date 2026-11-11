//! The manual free-KVM release-fetch proof keeps its exact credential and
//! upload boundary, carried over from the Python workflow tests. Compilation
//! and installation stay credentialless; only a closed summary is uploaded.

use super::{steps, workflow};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const WORKFLOW: &str = "release-fetch-runtime.yml";
const JOB: &str = "release-fetch-runtime";

/// A step's `run`, which must be text when present; empty when absent.
fn run(step: &Value) -> &str {
    step.get("run").map_or("", |run| run.as_str().unwrap())
}

/// From `test_manual_readonly_standard_runner_has_no_target_or_checkout_selectors`:
/// this optional disposable proof carries no release or signer authority.
#[test]
fn manual_readonly_standard_runner_has_no_target_or_checkout_selectors() {
    let fetch = workflow(WORKFLOW);
    let triggers = fetch["on"].as_object().unwrap();
    assert_eq!(
        triggers.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        ["workflow_dispatch"].into()
    );
    assert!(triggers["workflow_dispatch"].is_null());
    assert_eq!(
        fetch["permissions"],
        json!({"contents": "read", "actions": "read"})
    );
    let job = fetch["jobs"][JOB].as_object().unwrap();
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 90);
    assert_eq!(
        job["if"],
        concat!(
            "${{ github.event_name == 'workflow_dispatch' && ",
            "github.ref == 'refs/heads/main' && ",
            "github.repository == 'swiftraccoon/simplestChat' }}"
        )
    );
    assert!(!job.contains_key("needs"));
    assert!(!job.contains_key("env"));
    assert_eq!(
        steps(&fetch, JOB)[0]["with"],
        json!({"persist-credentials": false})
    );
}

/// From `test_sole_credential_leaf_runs_only_after_pinned_install_and_gnu_source_build`:
/// acquisition is a cache input, and both native branches stay in the keeper.
#[test]
fn sole_credential_leaf_runs_only_after_pinned_install_and_gnu_source_build() {
    let fetch = workflow(WORKFLOW);
    let steps = steps(&fetch, JOB);
    let token = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            step.get("env")
                .is_some_and(|env| env.as_object().unwrap().contains_key("GH_TOKEN"))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [runtime] = token[..] else {
        panic!("exactly one step carries the credential");
    };
    assert_eq!(steps[runtime]["timeout-minutes"], 65);
    assert_eq!(
        steps[runtime]["env"],
        json!({"GH_TOKEN": "${{ github.token }}"})
    );
    let before = steps[..runtime]
        .iter()
        .map(run)
        .collect::<Vec<_>>()
        .join("\n");
    for text in [
        "--require-hashes --only-binary=:all:",
        "build/security_tools.py install --tools gh",
        "cargo build --locked --release --target x86_64-unknown-linux-gnu",
        "-p simplestchat-ops --bin simplestchat-ops",
    ] {
        assert!(before.contains(text), "{text}");
    }
    let script = steps[runtime]["run"].as_str().unwrap();
    assert!(
        script.find("security_vm_release.py").unwrap()
            < script.find("security_vm_fetch.py").unwrap()
    );
    for text in ["timeout ", "${{", "ssh ", "curl "] {
        assert!(!script.contains(text), "{text}");
    }
}

/// From `test_upload_is_only_the_closed_success_summary`: raw captures, keys,
/// grants and failed partial reports cannot become artifacts.
#[test]
fn upload_is_only_the_closed_success_summary() {
    let fetch = workflow(WORKFLOW);
    let uploads = steps(&fetch, JOB)
        .iter()
        .filter(|step| {
            step.get("uses")
                .is_some_and(|uses| uses.as_str().unwrap().contains("upload-artifact@"))
        })
        .collect::<Vec<_>>();
    let [upload] = uploads[..] else {
        panic!("exactly one upload");
    };
    assert_eq!(upload["if"], "${{ success() }}");
    assert!(upload["with"].is_object());
    assert_eq!(
        upload["with"]["path"],
        "${{ runner.temp }}/release-fetch-runtime/summary.json"
    );
    assert_eq!(upload["with"]["if-no-files-found"], "error");
}
