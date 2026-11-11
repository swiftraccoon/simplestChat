//! The automation job's three disposable database checks, carried over from
//! the Python workflow tests.

use super::{steps, workflow};
use serde_json::{Value, json};

/// From `test_three_checks_keep_the_original_cache_guards_and_restore_opt_in`:
/// the automation job selects only the three maintained suites after the
/// pinned compiler setup, each behind the cache guard, and only the restore
/// check opts into its container.
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
            "Verify isolated backup restoration with pinned PostgreSQL",
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
            json!({"RESTORE_CONTAINER_E2E": "1"})
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
