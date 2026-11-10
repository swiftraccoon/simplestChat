//! Assertions moved from `ops/ansible/tests/test_ops_runtime_workflow.py`.

use super::{steps, workflow};
use serde_json::Value;

/// The indices of the steps `accept` selects, in workflow order.
fn indices(steps: &[Value], accept: impl Fn(&Value) -> bool) -> Vec<usize> {
    steps
        .iter()
        .enumerate()
        .filter(|(_, step)| accept(step))
        .map(|(index, _)| index)
        .collect()
}

/// From `test_api_credential_is_only_in_the_post_bootstrap_runtime_leaf`: the
/// controller is compiled by exactly one step with this exact command, before
/// the only step whose environment carries `GH_TOKEN`.
#[test]
fn the_controller_compiles_once_before_the_only_credential_step() {
    let workflow = workflow("ops-runtime.yml");
    let steps = steps(&workflow, "ops-runtime");
    // Python reads a missing environment as empty and refuses any other kind.
    assert!(
        steps
            .iter()
            .all(|step| step.get("env").is_none_or(Value::is_object))
    );
    let token_steps = indices(steps, |step| {
        step["env"]
            .as_object()
            .is_some_and(|env| env.contains_key("GH_TOKEN"))
    });
    assert_eq!(token_steps.len(), 1);
    let runtime = token_steps[0];
    let compile_steps = indices(steps, |step| {
        step["run"] == "cargo build --locked -p simplestchat-release --bin simplestchat-release"
    });
    assert_eq!(compile_steps.len(), 1);
    assert!(compile_steps[0] < runtime);
}
