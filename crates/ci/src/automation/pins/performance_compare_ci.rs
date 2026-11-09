//! Assertions moved from `ops/ansible/tests/test_performance_compare_ci.py`.

use super::{run, steps, workflow};

/// From `test_one_complete_owner_receives_only_the_existing_closed_profile_inputs`:
/// the comparison owner is the compare job's sixth step and runs exactly the
/// CI binary's comparison.
#[test]
fn the_sixth_compare_step_runs_the_ci_binary_comparison() {
    let performance = workflow("performance.yml");
    let step = &steps(&performance, "compare")[5];
    assert_eq!(step["name"], "Build and compare the baseline and candidate");
    assert_eq!(
        run(step),
        "cargo run --locked -q -p simplestchat-ci -- performance-compare"
    );
}
