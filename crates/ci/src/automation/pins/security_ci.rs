//! Assertions moved from `ops/ansible/tests/test_security_ci.py`.

use super::{document, first, run, steps, workflow};
use serde_json::Value;

/// From `test_native_dtls_cache_miss_bootstrap_controller_and_source_closure`:
/// the one native-dtls step that runs the CI binary records success exactly.
#[test]
fn the_native_dtls_job_runs_one_recording_ci_binary_check() {
    let ci = workflow("ci.yml");
    let checks = steps(&ci, "native-dtls")
        .iter()
        .filter(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("native-dtls"))
        })
        .collect::<Vec<_>>();
    assert_eq!(checks.len(), 1);
    assert_eq!(
        run(checks[0]),
        "cargo run --locked -q -p simplestchat-ci -- native-dtls --record-success"
    );
}

/// From `test_browser_suite_commands_preserve_groups_opt_ins_timeouts_and_failure_guard`:
/// each named browser step runs exactly its suite through the CI binary.
#[test]
fn each_browser_suite_step_runs_its_suite_through_the_ci_binary() {
    let ci = workflow("ci.yml");
    let browser = steps(&ci, "browser");
    for (name, suite) in [
        (
            "Authentication dialog and diagnostic privacy in Chromium and Firefox",
            "auth-lifecycle",
        ),
        (
            "Shared account state and room navigation in Chromium and Firefox",
            "auth-cross-tab",
        ),
        (
            "Confirm room controls and retain failed edits",
            "room-control-confirmation",
        ),
        (
            "Media continuity and device controls in Chromium and Firefox",
            "media-continuity",
        ),
        (
            "Read presentations through constrained and recovered browser media",
            "presentation",
        ),
    ] {
        let step = first(browser, |step| step["name"] == name);
        assert_eq!(
            run(step),
            format!("cargo run --locked -q -p simplestchat-ci -- browser-suite {suite}"),
            "{suite}"
        );
    }
}

/// From `test_database_cache_output_policy_preserves_guards_budgets_and_compiler_scope`:
/// both database-cache jobs publish their selector hint through the CI binary.
#[test]
fn both_database_cache_jobs_publish_their_hint_through_the_ci_binary() {
    let ci = workflow("ci.yml");
    for job in ["image-database", "deployment"] {
        let steps = steps(&ci, job);
        let hint = first(steps, |step| step["id"] == "image-database-inputs");
        assert_eq!(
            run(hint),
            "cargo run --locked -q -p simplestchat-ci -- image-cache hint",
            "{job}"
        );
    }
}

/// From `test_build_caches_cannot_fall_back_across_trust_or_architecture`: every
/// cache save that publishes a validated database generation names a producer
/// whose last command reports that generation's status through the CI binary.
#[test]
fn validated_database_cache_saves_name_a_ci_binary_status_producer() {
    let ci = workflow("ci.yml");
    let load = workflow("load-generator-image.yml");
    let composite = document(".github/actions/native-toolchain/action.yml");
    let sources = [
        composite["runs"]["steps"].as_array().unwrap().as_slice(),
        steps(&ci, "image-database"),
        steps(&ci, "deployment"),
        steps(&load, "image"),
    ];
    let saved = regex::Regex::new(
        r"^\$\{\{ steps\.((?:prepared|scanned)-image-database)\.outputs\.key \}\}$",
    )
    .unwrap();
    let mut producers = 0;
    for steps in sources {
        for step in steps {
            let uses = step["uses"].as_str().unwrap_or("");
            if !(uses.starts_with("actions/cache") || uses.starts_with("Swatinem/rust-cache"))
                || [
                    "Restore pinned rollover Chromium download bytes",
                    "Save pinned rollover Chromium download bytes",
                ]
                .contains(&step["name"].as_str().unwrap_or(""))
            {
                continue;
            }
            if !uses.starts_with("actions/cache/save@") {
                continue;
            }
            let key = step["with"]["key"].as_str().unwrap();
            let Some(found) = saved.captures(key) else {
                continue;
            };
            let producer = first(steps, |step: &Value| step["id"] == found[1]);
            let kind = if producer["id"] == "prepared-image-database" {
                "preparation"
            } else {
                "production"
            };
            assert_eq!(
                run(producer).lines().last(),
                Some(
                    format!(
                        "cargo run --locked -q -p simplestchat-ci -- image-cache status {kind}"
                    )
                    .as_str()
                ),
                "{key}"
            );
            producers += 1;
        }
    }
    assert!(producers > 0, "at least one validated database cache save");
}
