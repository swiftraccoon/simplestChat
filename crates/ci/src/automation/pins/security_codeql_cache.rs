//! Assertions moved from the Python tests of the CodeQL database cache, which
//! were deleted with the Python cache module.

use super::{first, run, steps, workflow};

/// From `test_hosted_rust_key_uses_compiled_tooling_after_exact_bootstrap`: the
/// Rust database key is computed by the CI binary the job built.
#[test]
fn the_rust_database_key_is_computed_by_the_ci_binary() {
    let codeql = workflow("codeql.yml");
    let key = first(steps(&codeql, "source-analysis"), |step| {
        step["id"] == "rust-inputs"
    });
    assert_eq!(
        run(key),
        concat!(
            "key=\"$(\"$RUNNER_TEMP/codeql-tooling/debug/simplestchat-ci\" codeql-cache key",
            " --language rust --suite \"$CODEQL_SUITE\" \\\n",
            "  --source-root \"$RUNNER_TEMP/codeql-local/source\")\"\n",
            "echo \"key=$key\" >> \"$GITHUB_OUTPUT\"\n",
        )
    );
}
