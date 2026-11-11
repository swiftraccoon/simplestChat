//! Workflow and playbook pins, one file per subject. A file carried over from
//! a Python test module takes that module's name and holds only its
//! assertions: each pin reads the same workflow, selects the same step the
//! same way and compares the same text. `public_recorded_release` is not
//! carried over; it pins the public playbook's recorded-release guard.

mod ansible;
mod benchmark_automation;
mod cache_retention_workflow;
mod ci_database_check;
mod ci_verified;
mod host_playbooks;
mod migration_prepare;
mod performance_compare_ci;
mod public_deployment;
mod public_recorded_release;
mod python_quality;
mod release_container_ci;
mod release_playbook;
mod release_workflow;
mod security_ci;
mod security_codeql_cache;
mod security_vm_boot;
mod session_soak_ci;

use serde_json::Value;
use std::path::Path;

/// The text of the file at `path` relative to the repository root.
fn checkout_file(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    std::fs::read_to_string(root.join(path)).unwrap()
}

/// The parsed YAML file at `path` relative to the repository root.
fn document(path: &str) -> Value {
    serde_saphyr::from_str(&checkout_file(path)).unwrap()
}

/// One workflow under `.github/workflows/`.
fn workflow(name: &str) -> Value {
    document(&format!(".github/workflows/{name}"))
}

/// The ordered steps of one job, each of which must be a mapping.
fn steps<'a>(workflow: &'a Value, job: &str) -> &'a [Value] {
    let steps = workflow["jobs"][job]["steps"].as_array().unwrap();
    assert!(steps.iter().all(Value::is_object), "{job}");
    steps
}

/// The first step a predicate accepts, as Python's `next(...)` selects it.
fn first(steps: &[Value], accept: impl Fn(&Value) -> bool) -> &Value {
    located(steps, accept).1
}

/// The first step a predicate accepts, and its position among the steps.
fn located(steps: &[Value], accept: impl Fn(&Value) -> bool) -> (usize, &Value) {
    steps
        .iter()
        .enumerate()
        .find(|(_, step)| accept(step))
        .unwrap()
}

/// Exactly one step with `name`, failing on missing or ambiguous wiring.
fn unique<'a>(steps: &'a [Value], name: &str) -> &'a Value {
    let matches = steps
        .iter()
        .filter(|step| step["name"] == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "Expected exactly one {name:?} step");
    matches[0]
}

/// A step's `run` text.
fn run(step: &Value) -> &str {
    step["run"].as_str().unwrap()
}

/// A step's `run`, which must be text when present; empty for a step without
/// one, as Python's `step.get("run", "")` reads it.
fn script(step: &Value) -> &str {
    step.get("run").map_or("", |run| run.as_str().unwrap())
}

/// A step's `uses`, which must be text when present; empty when absent.
fn uses(step: &Value) -> &str {
    step.get("uses").map_or("", |uses| uses.as_str().unwrap())
}

/// Whether a mapping lacks every one of `keys`.
fn lacks(mapping: &Value, keys: &[&str]) -> bool {
    assert!(mapping.is_object());
    keys.iter().all(|key| mapping.get(key).is_none())
}
