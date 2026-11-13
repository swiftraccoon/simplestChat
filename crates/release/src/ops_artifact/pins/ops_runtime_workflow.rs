//! The manual Ops runtime proof workflow, carried over from the Python
//! workflow tests: a main-only read-only job that must not become a signer or
//! a mandatory production gate. The compiled controller writes its request;
//! `runtime::request_writer` and `tests/runtime_request.rs` cover that program.

use super::{checkout_file, steps, workflow};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const WORKFLOW: &str = "ops-runtime.yml";
const JOB: &str = "ops-runtime";
const CHECKOUT: &str = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const UPLOAD: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const COMPILE: &str = "cargo build --locked -p simplestchat-release --bin simplestchat-release";
const TOOLS: &str = "cargo run --locked -q -p simplestchat-security -- tools";

/// The indices of the steps `accept` selects, in workflow order.
fn indices(steps: &[Value], accept: impl Fn(&Value) -> bool) -> Vec<usize> {
    steps
        .iter()
        .enumerate()
        .filter(|(_, step)| accept(step))
        .map(|(index, _)| index)
        .collect()
}

/// The keys of a mapping.
fn keys(value: &Value) -> BTreeSet<&str> {
    let mapping = value.as_object().unwrap();
    mapping.keys().map(String::as_str).collect()
}

/// A step's `run`, which must be text when present; empty when absent.
fn script(step: &Value) -> &str {
    step.get("run").map_or("", |run| run.as_str().unwrap())
}

/// Whether a step's environment, a mapping when present, names `variable`.
fn carries(step: &Value, variable: &str) -> bool {
    step.get("env")
        .is_some_and(|env| env.as_object().unwrap().contains_key(variable))
}

/// From `test_manual_main_only_readonly_scope_has_no_caller_selected_checkout`:
/// inputs select an artifact identity, while the event selects the checkout.
#[test]
fn manual_main_only_readonly_scope_has_no_caller_selected_checkout() {
    let document = workflow(WORKFLOW);
    assert_eq!(keys(&document["on"]), ["workflow_dispatch"].into());
    let inputs = &document["on"]["workflow_dispatch"]["inputs"];
    assert_eq!(
        keys(inputs),
        ["run_id", "run_attempt", "artifact_id"].into()
    );
    for name in keys(inputs) {
        assert_eq!(inputs[name]["type"], "string", "{name}");
        assert_eq!(inputs[name]["required"], true, "{name}");
    }
    assert_eq!(
        document["permissions"],
        json!({"contents": "read", "actions": "read"})
    );
    assert!(document.get("env").is_none());
    assert_eq!(keys(&document["jobs"]), [JOB].into());
    let job = &document["jobs"][JOB];
    assert_eq!(
        job["if"],
        concat!(
            "${{ github.event_name == 'workflow_dispatch' && ",
            "github.ref == 'refs/heads/main' ",
            "&& github.repository == 'swiftraccoon/simplestChat' }}"
        )
    );
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    for key in ["permissions", "env", "needs"] {
        assert!(job.get(key).is_none(), "{key}");
    }
    let steps = steps(&document, JOB);
    assert_eq!(steps[0]["uses"], CHECKOUT);
    assert_eq!(steps[0]["with"], json!({"persist-credentials": false}));
    for step in steps {
        assert!(step.get("continue-on-error").is_none());
        assert!(step.get("secrets").is_none());
        assert!(!script(step).contains("${{"));
        if step["id"] == "diagnostics" {
            assert_eq!(step["if"], "${{ always() }}");
            assert_eq!(
                script(step).lines().collect::<Vec<_>>(),
                [
                    "if test -x target/debug/simplestchat-release; then",
                    "  target/debug/simplestchat-release ops-artifact runtime-diagnostics",
                    "fi",
                ]
            );
            assert_eq!(step["timeout-minutes"], 1);
        } else if step["uses"] != UPLOAD {
            assert!(step.get("if").is_none());
        }
    }
    let canonical = workflow("ci.yml");
    let needs = canonical["jobs"]["required"]["needs"].as_array().unwrap();
    assert!(needs.iter().all(|need| need.as_str().unwrap() != JOB));
}

/// From `test_api_credential_is_only_in_the_post_bootstrap_runtime_leaf`: the
/// controller is compiled by exactly one step with this exact command, before
/// the only step whose environment carries `GH_TOKEN`.
#[test]
fn the_controller_compiles_once_before_the_only_credential_step() {
    let workflow = workflow(WORKFLOW);
    let steps = steps(&workflow, JOB);
    // Python reads a missing environment as empty and refuses any other kind.
    assert!(
        steps
            .iter()
            .all(|step| step.get("env").is_none_or(Value::is_object))
    );
    let token_steps = indices(steps, |step| carries(step, "GH_TOKEN"));
    assert_eq!(token_steps.len(), 1);
    let runtime = token_steps[0];
    let compile_steps = indices(steps, |step| step["run"] == COMPILE);
    assert_eq!(compile_steps.len(), 1);
    assert!(compile_steps[0] < runtime);
}

/// From `test_api_credential_is_only_in_the_post_bootstrap_runtime_leaf`:
/// authenticated tool installation finishes before the token is exposed, and
/// the request step receives only the three dispatch inputs.
#[test]
fn api_credential_is_only_in_the_post_bootstrap_runtime_leaf() {
    let workflow = workflow(WORKFLOW);
    let steps = steps(&workflow, JOB);
    let [runtime] = indices(steps, |step| carries(step, "GH_TOKEN"))[..] else {
        panic!("exactly one step carries the credential");
    };
    assert_eq!(
        steps[runtime]["env"],
        json!({
            "GH_TOKEN": "${{ github.token }}",
            "OPS_REQUEST_SHA256": "${{ steps.request.outputs.sha256 }}",
        })
    );
    assert_eq!(
        script(&steps[runtime]).lines().collect::<Vec<_>>(),
        [
            "target/debug/simplestchat-release ops-artifact runtime \\",
            "  --request \"$RUNNER_TEMP/ops-runtime-request.json\" \\",
            "  --request-sha256 \"$OPS_REQUEST_SHA256\"",
        ]
    );
    let install = format!("{TOOLS} install --tools gh --directory \"$RUNNER_TEMP/ops-gh\"");
    let resolve = format!("\"$({TOOLS} path gh --directory \"$RUNNER_TEMP/ops-gh\")\"");
    let [bootstrap] = indices(steps, |step| script(step).contains(&install))[..] else {
        panic!("exactly one step installs the pinned gh");
    };
    assert!(bootstrap < runtime);
    let bootstrap = script(&steps[bootstrap]);
    assert!(bootstrap.find(&install).unwrap() < bootstrap.find(&resolve).unwrap());
    assert!(!bootstrap.contains("security_tools.py"));
    assert!(bootstrap.contains("test \"$ops_gh_executable\" = \"$RUNNER_TEMP/ops-gh/bin/gh\""));
    for (index, step) in steps.iter().enumerate() {
        assert!(!carries(step, "GITHUB_TOKEN"));
        assert!(index == runtime || !carries(step, "GH_TOKEN"));
    }
    let [request] = indices(steps, |step| step["id"] == "request")[..] else {
        panic!("exactly one step writes the request");
    };
    assert!(request < runtime);
    assert_eq!(
        steps[request]["env"],
        json!({
            "OPS_RUN_ID": "${{ inputs.run_id }}",
            "OPS_RUN_ATTEMPT": "${{ inputs.run_attempt }}",
            "OPS_ARTIFACT_ID": "${{ inputs.artifact_id }}",
        })
    );
}

/// The request is written by the controller the job compiled, never by an
/// inline program: the one request step runs exactly that command, after the
/// one compile step, and no step feeds an interpreter a here-document.
#[test]
fn the_compiled_controller_writes_the_request() {
    let workflow = workflow(WORKFLOW);
    let steps = steps(&workflow, JOB);
    let [request] = indices(steps, |step| step["id"] == "request")[..] else {
        panic!("exactly one step writes the request");
    };
    assert_eq!(
        steps[request]["run"],
        "target/debug/simplestchat-release ops-artifact runtime-request"
    );
    let [compile] = indices(steps, |step| step["run"] == COMPILE)[..] else {
        panic!("exactly one step compiles the controller");
    };
    assert!(compile < request);
    assert!(steps.iter().all(|step| !script(step).contains("<<")));
}

/// From `test_watchdogs_leave_retirement_time_and_only_success_uploads_the_summary`:
/// private intermediate artifacts and failed runs cannot become public evidence.
#[test]
fn watchdogs_leave_retirement_time_and_only_success_uploads_the_summary() {
    let document = workflow(WORKFLOW);
    let job = &document["jobs"][JOB];
    let steps = steps(&document, JOB);
    let runtime = &steps[indices(steps, |step| carries(step, "GH_TOKEN"))[0]];
    assert_eq!(job["timeout-minutes"], 90);
    assert_eq!(runtime["timeout-minutes"], 75);
    let plan = checkout_file("crates/release/src/ops_artifact/runtime/plan.rs");
    let entry = regex::Regex::new(r"const ENTRY: Duration = Duration::from_secs\(([0-9]+)\)")
        .unwrap()
        .captures_iter(&plan)
        .map(|found| found[1].to_owned())
        .collect::<Vec<_>>();
    assert_eq!(entry, ["4200"]);
    let seconds = |minutes: &Value| minutes.as_u64().unwrap() * 60;
    assert!(entry[0].parse::<u64>().unwrap() < seconds(&runtime["timeout-minutes"]));
    assert!(seconds(&runtime["timeout-minutes"]) < seconds(&job["timeout-minutes"]));
    let uploads = indices(steps, |step| step["uses"] == UPLOAD);
    let [summary, diagnostic] = uploads[..] else {
        panic!("exactly two uploads");
    };
    assert_eq!(diagnostic, steps.len() - 1);
    assert_eq!(steps[summary]["if"], "${{ success() }}");
    assert_eq!(
        steps[summary]["with"],
        json!({
            "name": "ops-runtime-summary",
            "path": "${{ runner.temp }}/ops-runtime/summary.json",
            "if-no-files-found": "error",
            "retention-days": 14,
        })
    );
    assert_eq!(
        steps[diagnostic]["if"],
        "${{ always() && steps.diagnostics.outputs.retained == 'true' }}"
    );
    assert_eq!(
        steps[diagnostic]["with"],
        json!({
            "name": "ops-runtime-diagnostics",
            "path": "${{ runner.temp }}/ops-runtime-diagnostics/diagnostic.json",
            "if-no-files-found": "error",
            "retention-days": 1,
        })
    );
    let [diagnostics] = indices(steps, |step| step["id"] == "diagnostics")[..] else {
        panic!("exactly one diagnostics step");
    };
    assert_eq!(
        steps[diagnostics]["env"],
        json!({
            "OPS_RUN_ID": "${{ inputs.run_id }}",
            "OPS_RUN_ATTEMPT": "${{ inputs.run_attempt }}",
            "OPS_ARTIFACT_ID": "${{ inputs.artifact_id }}",
            "OPS_RUNTIME_OUTCOME": "${{ steps.runtime.outcome }}",
        })
    );
}
