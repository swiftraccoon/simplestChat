//! Assertions moved from `ops/ansible/tests/test_session_soak_ci.py`.

use super::{first, run, steps, workflow};

/// From `test_explicit_service_context_and_captured_database_target_replace_port_discovery`:
/// the soak job's named step runs exactly the CI binary's soak.
#[test]
fn the_soak_step_runs_the_ci_binary_soak() {
    let performance = workflow("performance.yml");
    let step = first(steps(&performance, "soak"), |step| {
        step["name"] == "Soak twenty minutes and stop PostgreSQL for eight seconds at the midpoint"
    });
    assert_eq!(
        run(step),
        "cargo run --locked -q -p simplestchat-ci -- session-soak"
    );
}
