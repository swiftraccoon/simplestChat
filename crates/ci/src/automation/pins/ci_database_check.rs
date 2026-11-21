//! The automation job's three disposable database checks, carried over from
//! the Python workflow tests.

use super::{checkout_file, steps, workflow};
use serde_json::{Value, json};

/// From `test_three_checks_keep_the_original_cache_guards_and_restore_opt_in`:
/// the automation job selects only the three maintained suites after the
/// pinned compiler setup, each behind the cache guard, and only the restore
/// check opts into its real containers of the pinned image.
#[test]
fn three_checks_keep_the_original_cache_guards_and_restore_opt_in() {
    let ci = workflow("ci.yml");
    assert_eq!(ci["jobs"]["automation"]["timeout-minutes"], 10);
    let steps = steps(&ci, "automation");
    let compiler = steps
        .iter()
        .position(|step| step["name"] == "Install the pinned Rust toolchain for CI receipts")
        .unwrap();
    let names = [
        (
            "Verify operational alert history in disposable PostgreSQL",
            "monitoring",
        ),
        (
            "Verify release backup restoration in disposable PostgreSQL and the pinned image",
            "restore",
        ),
        (
            "Verify server migration data preservation in disposable PostgreSQL",
            "migration",
        ),
    ];
    let mut selected = Vec::new();
    for (name, check) in names {
        let matches = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step["name"] == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name}");
        let (index, step) = matches[0];
        assert!(index > compiler, "{name}");
        assert_eq!(
            step["if"], "steps.verified.outputs.cache-hit != 'true'",
            "{name}"
        );
        assert_eq!(
            step["run"],
            format!("cargo run --locked -q -p simplestchat-ci -- database-check {check}"),
            "{name}"
        );
        let step = step.as_object().unwrap();
        assert!(!step.contains_key("continue-on-error"), "{name}");
        assert!(!step.contains_key("timeout-minutes"), "{name}");
        let environment = step.get("env").cloned().unwrap_or_else(|| json!({}));
        let expected = if check == "restore" {
            json!({"RESTORE_CONTAINER_CHECK": "1"})
        } else {
            json!({})
        };
        assert_eq!(environment, expected, "{name}");
        selected.push(index);
    }
    assert!(selected.is_sorted());
    // Every `run` is text, and exactly those three name the command.
    assert!(
        steps
            .iter()
            .all(|step| step.get("run").is_none_or(Value::is_string))
    );
    let commands = steps
        .iter()
        .filter(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("database-check"))
        })
        .count();
    assert_eq!(commands, 3);
}

/// A failed restore check uploads, for a week, only the evidence its Ops
/// tests copy to the runner's temporary directory as publishable; never in
/// the local runner.
#[test]
fn a_failed_restore_check_uploads_only_its_publishable_evidence() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "automation");
    let position = |name: &str| {
        let found = steps.iter().position(|step| step["name"] == name);
        found.unwrap_or_else(|| panic!("{name}"))
    };
    let restore =
        position("Verify release backup restoration in disposable PostgreSQL and the pinned image");
    let upload = position("Preserve restore failure evidence");
    assert!(upload > restore);
    let tests = checkout_file("ops/src/release/attestation/restore/database_tests.rs");
    let prefix = "pub(super) const EVIDENCE: &str = \"simplestchat-restore-evidence.\";";
    assert_eq!(tests.matches(prefix).count(), 1);
    assert_eq!(
        steps[upload],
        json!({
            "name": "Preserve restore failure evidence",
            "if": "${{ failure() && !env.ACT }}",
            "uses": "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            "with": {
                "name": "restore-evidence",
                "path": "${{ runner.temp }}/simplestchat-restore-evidence.*/",
                "if-no-files-found": "ignore",
                "retention-days": 7,
            },
        })
    );
}
