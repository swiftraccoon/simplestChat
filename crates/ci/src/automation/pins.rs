//! Workflow and playbook pins, one file per subject. A file carried over from
//! a Python test module takes that module's name and holds only its
//! assertions: each pin reads the same workflow, selects the same step the
//! same way and compares the same text. `public_recorded_release` is not
//! carried over; it pins the public playbook's recorded-release guard.

mod ci_verified;
mod performance_compare_ci;
mod public_recorded_release;
mod python_quality;
mod release_container_ci;
mod security_ci;
mod security_codeql_cache;
mod session_soak_ci;

use serde_json::Value;
use std::path::Path;

/// The parsed YAML file at `path` relative to the repository root.
fn document(path: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    serde_saphyr::from_str(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
}

/// One workflow under `.github/workflows/`.
fn workflow(name: &str) -> Value {
    document(&format!(".github/workflows/{name}"))
}

/// The ordered steps of one job.
fn steps<'a>(workflow: &'a Value, job: &str) -> &'a [Value] {
    workflow["jobs"][job]["steps"].as_array().unwrap()
}

/// The first step a predicate accepts, as Python's `next(...)` selects it.
fn first(steps: &[Value], accept: impl Fn(&Value) -> bool) -> &Value {
    steps.iter().find(|step| accept(step)).unwrap()
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
