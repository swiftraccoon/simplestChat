//! The cache retention workflow: maintenance separate from the CI checks,
//! applied only after a successful main push, never from a pull request's
//! checkout. Carried over from the Python retention tests.

use super::{lacks, run, steps, workflow};
use serde_json::json;

/// From `test_local_default_is_read_only_and_workflow_requires_successful_main_ci`:
/// the workflow half. The command's read-only default is covered by the
/// retention owner's tests.
#[test]
fn workflow_requires_successful_main_ci_before_applying_retention() {
    let retention = workflow("cache-retention.yml");
    assert_eq!(retention["on"]["workflow_run"]["workflows"], json!(["CI"]));
    let job = &retention["jobs"]["retain"];
    let guard = job["if"].as_str().unwrap();
    for condition in [
        "conclusion == 'success'",
        "event == 'push'",
        "head_branch == 'main'",
        "head_repository.full_name == github.repository",
    ] {
        assert!(guard.contains(condition), "{condition}");
    }
    assert_eq!(
        job["permissions"],
        json!({"contents": "read", "actions": "write"})
    );
    let steps = steps(&retention, "retain");
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(
        steps[0]["with"]["ref"],
        "${{ github.event.workflow_run.head_sha }}"
    );
    assert_eq!(
        steps[1]["uses"],
        "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d"
    );
    assert_eq!(steps[1]["with"]["toolchain"], "1.99.0");
    assert_eq!(
        steps[2]["run"],
        "cargo build --locked -q -p simplestchat-ci"
    );
    assert!(lacks(&steps[2], &["env"]));
    assert!(run(&steps[3]).contains("target/debug/simplestchat-ci retention --apply"));
}
