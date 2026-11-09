//! Assertions moved from `ops/ansible/tests/test_ci_verified.py`.

use super::{first, run, steps, workflow};

/// From `test_audits_helpers_and_browser_suites_are_not_replaced_by_cached_success`:
/// the rust-lint job's dependency audit runs the CI binary's step exactly.
#[test]
fn rust_lint_audits_dependencies_through_the_ci_binary() {
    let ci = workflow("ci.yml");
    let lint = steps(&ci, "rust-lint");
    let step = first(lint, |step| step["name"] == "Audit Rust dependencies");
    assert_eq!(
        run(step),
        "cargo run --locked -q -p simplestchat-ci -- audit-rust"
    );
}
