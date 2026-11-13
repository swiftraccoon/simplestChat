//! Workflow pins, one file per subject. A file carried over from a Python test
//! module takes that module's name and holds only its assertions: each pin
//! reads the same workflow, selects the same steps the same way and compares
//! the same text. `release_runtime_controllers` is not carried over; it pins
//! each release runtime workflow's controller build to the path its Python
//! helper opens.

mod ops_artifact_workflow;
mod ops_runtime_workflow;
mod pinned_gh_installation;
mod release_fetch_runtime_workflow;
mod release_runtime_controllers;

use serde_json::Value;
use std::path::Path;

/// The text of one file in the checkout, by its path from the repository root.
fn checkout_file(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    std::fs::read_to_string(root.join(path)).unwrap()
}

/// One workflow under `.github/workflows/`, parsed from the checkout.
fn workflow(name: &str) -> Value {
    serde_saphyr::from_str(&checkout_file(&format!(".github/workflows/{name}"))).unwrap()
}

/// The ordered steps of one job, each of which must be a mapping.
fn steps<'a>(workflow: &'a Value, job: &str) -> &'a [Value] {
    let steps = workflow["jobs"][job]["steps"].as_array().unwrap();
    assert!(
        steps.iter().all(Value::is_object),
        "{job}: every step is a mapping"
    );
    steps
}
