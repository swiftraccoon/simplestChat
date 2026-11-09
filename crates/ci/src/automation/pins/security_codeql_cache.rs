//! Assertions moved from `ops/ansible/tests/test_security_codeql_cache.py`.

use super::{first, run, steps, workflow};

/// From `test_hosted_rust_key_uses_compiled_tooling_after_exact_bootstrap`: the
/// Rust database key is computed by the CI binary.
#[test]
fn the_rust_database_key_is_computed_by_the_ci_binary() {
    let codeql = workflow("codeql.yml");
    let key = first(steps(&codeql, "source-analysis"), |step| {
        step["id"] == "rust-inputs"
    });
    assert!(run(key).contains("cargo run --locked -q -p simplestchat-ci -- codeql-cache key"));
}
