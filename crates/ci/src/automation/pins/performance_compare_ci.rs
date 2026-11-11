//! The hosted baseline-versus-candidate comparison job, carried over from the
//! Python workflow tests.

use super::{run, steps, workflow};
use serde_json::json;

/// The one owner of the benchmark builds and the six runs.
const NAME: &str = "Build and compare the baseline and candidate";

/// From `test_same_hosted_setup_cache_and_ninety_minute_ceiling_precede_owner`:
/// no new bootstrap or paid runner, and the pinned npm also builds the bundle.
#[test]
fn same_hosted_setup_cache_and_ninety_minute_ceiling_precede_owner() {
    let performance = workflow("performance.yml");
    let job = &performance["jobs"]["compare"];
    let steps = steps(&performance, "compare");
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 90);
    assert_eq!(
        job["if"],
        "${{ !inputs.only || inputs.only == 'all' || inputs.only == 'compare' }}"
    );
    assert_eq!(steps[0]["with"]["fetch-depth"], 0);
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(steps[1]["uses"], "./.github/actions/native-toolchain");
    assert_eq!(steps[1]["with"]["cache-key"], "rust-release-benchmark");
    assert_eq!(steps[2]["with"]["node-version"], "26.10.0");
    assert_eq!(steps[3]["name"], "Install checksum-pinned npm");
    assert_eq!(
        steps[4]["name"],
        "Build the browser bundle the server requires at startup"
    );
    assert_eq!(
        steps[4]["run"],
        "npm --prefix web ci --ignore-scripts\nnpm --prefix web run build\n"
    );
    assert_eq!(job["env"]["OPENSSL_STATIC"], "1");
    assert_eq!(
        job["env"]["PIP_CONSTRAINT"],
        "${{ github.workspace }}/build/pip-constraints.txt"
    );
}

/// From `test_one_complete_owner_receives_only_the_existing_closed_profile_inputs`:
/// the benchmark builds and six runs belong to one owner, not split shells.
/// Only the browser bundle is built beside the owner: the server refuses to
/// start without it, and the comparison never requests a page.
#[test]
fn one_complete_owner_receives_only_the_existing_closed_profile_inputs() {
    let performance = workflow("performance.yml");
    let steps = steps(&performance, "compare");
    let step = &steps[5];
    assert_eq!(step["name"], NAME);
    assert_eq!(
        step["env"],
        json!({
            "BASELINE_REF": "${{ inputs.baseline || 'HEAD~1' }}",
            "CLIENTS": "${{ inputs.clients || '10' }}",
            "WORKERS": "${{ inputs.workers || '1' }}",
        })
    );
    let keys = step.as_object().unwrap();
    for key in [
        "if",
        "continue-on-error",
        "timeout-minutes",
        "working-directory",
    ] {
        assert!(!keys.contains_key(key), "{key}");
    }
    assert_eq!(steps.len(), 8);
}

/// From `test_threshold_and_artifact_guards_paths_and_retention_stay_separate`:
/// failed completed comparisons still reach the unchanged result readers.
#[test]
fn threshold_and_artifact_guards_paths_and_retention_stay_separate() {
    let performance = workflow("performance.yml");
    let [threshold, upload] = &steps(&performance, "compare")[6..] else {
        panic!("two steps follow the owner");
    };
    assert_eq!(threshold["name"], "Evaluate the regression budget");
    assert_eq!(threshold["if"], "${{ !cancelled() }}");
    assert_eq!(
        threshold["run"],
        concat!(
            "cargo run --locked -q -p xtask -- performance-thresholds ",
            "\"${RUNNER_TEMP}/comparison/comparison.json\""
        )
    );
    assert_eq!(upload["name"], "Retain the comparison");
    assert_eq!(upload["if"], "${{ !cancelled() }}");
    assert_eq!(
        upload["uses"],
        "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
    );
    assert_eq!(upload["with"]["path"], "${{ runner.temp }}/comparison");
    assert_eq!(upload["with"]["if-no-files-found"], "warn");
    assert_eq!(upload["with"]["retention-days"], 30);
}

/// From `test_one_complete_owner_receives_only_the_existing_closed_profile_inputs`:
/// the comparison owner is the compare job's sixth step and runs exactly the
/// CI binary's comparison.
#[test]
fn the_sixth_compare_step_runs_the_ci_binary_comparison() {
    let performance = workflow("performance.yml");
    let step = &steps(&performance, "compare")[5];
    assert_eq!(step["name"], NAME);
    assert_eq!(
        run(step),
        "cargo run --locked -q -p simplestchat-ci -- performance-compare"
    );
}
