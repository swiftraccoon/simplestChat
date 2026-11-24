//! The CI cache's workflow wiring: each test reads the workflow, selects its
//! steps and compares their text.

use super::{checkout_file, first, lacks, located, run, script, steps, uses, workflow};
use serde_json::{Value, json};

/// The setup action that selects the pinned Python.
const SETUP_PYTHON: &str = "actions/setup-python@";

/// The OpenSSL prefix every Rust and native consumer must see.
const CANONICAL_PKG_CONFIG: &str = "${{ github.workspace }}/target/openssl-4.1.0/lib/pkgconfig";

/// The position of the first step a predicate accepts.
fn position(steps: &[Value], accept: impl Fn(&Value) -> bool) -> usize {
    located(steps, accept).0
}

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

/// From `test_audits_helpers_and_browser_suites_are_not_replaced_by_cached_success`:
/// cached Rust work suppresses neither the advisory audit, the helper
/// checks nor any browser suite.
#[test]
fn audits_helpers_and_browser_suites_are_not_replaced_by_cached_success() {
    let ci = workflow("ci.yml");
    let lint = steps(&ci, "rust-lint");
    for name in [
        "Audit Rust dependencies",
        "Check helper syntax, safety and cleanup",
    ] {
        let step = first(lint, |step| step["name"] == name);
        assert!(lacks(step, &["if"]), "{name}");
        if name == "Check helper syntax, safety and cleanup" {
            assert_eq!(
                step["run"],
                "cargo run --locked -q -p xtask -- check --helpers"
            );
        }
    }
    assert_eq!(ci["jobs"]["rust-lint"]["timeout-minutes"], 30);
    let browser = steps(&ci, "browser");
    for step in browser {
        if script(step).contains("node web/e2e/") {
            let guard = step.get("if").map_or("", |guard| guard.as_str().unwrap());
            assert!(!guard.contains("verified"), "{guard}");
        }
    }
    let toolchain = first(browser, |step| {
        uses(step) == "./.github/actions/native-toolchain"
    });
    assert_eq!(
        toolchain["if"],
        "steps.verified.outputs.cache-hit != 'true'"
    );
}

/// From `test_selected_python_installs_the_reviewed_wheel_lock_before_fingerprinting`:
/// the Rust tests' fingerprint installs the reviewed wheel lock under the
/// selected Python and then keys the scope, unconditionally.
#[test]
fn selected_python_installs_the_reviewed_wheel_lock_before_fingerprinting() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "rust-test");
    let python = position(steps, |step| uses(step).starts_with(SETUP_PYTHON));
    let (fingerprinted, fingerprint) = located(steps, |step| step["id"] == "verified-inputs");
    assert!(python < fingerprinted);
    assert!(lacks(fingerprint, &["if", "continue-on-error"]));
    assert_eq!(
        run(fingerprint).lines().collect::<Vec<_>>(),
        [
            concat!(
                "python -m pip install --require-hashes --only-binary=:all: ",
                "-r ops/ansible/requirements.txt"
            ),
            concat!(
                "cargo run --locked -q -p simplestchat-ci -- key --scope \"$CI_CACHE_SCOPE\" ",
                "--github-output"
            ),
        ]
    );
    let requirements = checkout_file("ops/ansible/requirements.txt");
    let lines = requirements.lines().collect::<Vec<_>>();
    assert!(lines.contains(&"--require-hashes"));
    assert!(lines.contains(&"--only-binary=:all:"));
}

/// From `test_rollover_browser_acquisition_has_ten_minutes_without_weakening_runtime`:
/// slow font downloads get ten minutes for the same pinned install, before
/// the unchanged release check.
#[test]
fn rollover_browser_acquisition_has_ten_minutes_without_weakening_runtime() {
    let ci = workflow("ci.yml");
    let job = &ci["jobs"]["deployment"];
    let steps = steps(&ci, "deployment");
    let (installed, install) = located(steps, |step| {
        step["name"] == "Install pinned rollover browser tooling"
    });
    let (released, release) = located(steps, |step| {
        step["name"] == "App-only release and rollback on a disposable host"
    });
    assert_eq!(install["timeout-minutes"], 10);
    assert_eq!(release["timeout-minutes"], 20);
    assert_eq!(job["timeout-minutes"], 95);
    assert!(lacks(install, &["if", "continue-on-error"]));
    assert!(lacks(release, &["if", "continue-on-error"]));
    assert_eq!(
        install["env"],
        json!({"PLAYWRIGHT_BROWSERS_PATH": "${{ runner.temp }}/rollover-playwright"})
    );
    let commands = run(install)
        .lines()
        .filter(|line| !line.is_empty() && !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            "npm --prefix web/e2e ci --ignore-scripts",
            "npm --prefix web/e2e exec -- playwright install --with-deps chromium",
        ]
    );
    assert!(installed < released);
}

/// From `test_automation_checks_keep_their_ceiling_and_exact_gate`: the
/// checks job keeps its ten-minute ceiling and runs the whole gate once.
#[test]
fn automation_checks_keep_their_ceiling_and_exact_gate() {
    let ci = workflow("ci.yml");
    let job = &ci["jobs"]["automation"];
    assert_eq!(job["timeout-minutes"], 10);
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    let selected = steps(&ci, "automation")
        .iter()
        .filter(|step| script(step).contains("-- automation"))
        .collect::<Vec<_>>();
    let [gate] = selected[..] else {
        panic!("one gate: {selected:?}");
    };
    assert_eq!(
        gate["run"],
        "cargo run --locked -q -p simplestchat-ci -- automation --checks"
    );
}

/// The pinned Python is selected once, unconditionally, and named for what
/// runs on it, before every step that installs into it or runs it: in
/// rust-test the Ansible the playbook pins call and the vendored worker
/// build's pip; in automation the Ansible controller environment. It also
/// precedes the fingerprint and the cache restore.
#[test]
fn the_pinned_python_precedes_fingerprints_cache_and_its_consumers() {
    let ci = workflow("ci.yml");
    for (name, purpose) in [
        (
            "rust-test",
            "Select the pinned Python of Ansible and the native build",
        ),
        (
            "automation",
            "Select the pinned Python of the Ansible controller",
        ),
    ] {
        let steps = steps(&ci, name);
        let selected = steps
            .iter()
            .filter(|step| uses(step).starts_with(SETUP_PYTHON))
            .collect::<Vec<_>>();
        let [python] = selected[..] else {
            panic!("{name}: one Python: {selected:?}");
        };
        assert_eq!(python["name"], purpose, "{name}");
        assert_eq!(
            python["uses"],
            "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1"
        );
        assert_eq!(python["with"]["python-version"], "3.14.8", "{name}");
        assert!(lacks(python, &["if", "continue-on-error"]), "{name}");
        let selected = position(steps, |step| uses(step).starts_with(SETUP_PYTHON));
        let mut protected = vec![
            position(steps, |step| step["id"] == "verified-inputs"),
            position(steps, |step| step["id"] == "verified"),
        ];
        if name == "rust-test" {
            protected.extend([
                position(steps, |step| {
                    uses(step) == "./.github/actions/native-toolchain"
                }),
                position(steps, |step| script(step).starts_with("cargo build ")),
                position(steps, |step| script(step).starts_with("cargo test ")),
            ]);
        } else {
            protected.push(position(steps, |step| {
                script(step).contains("python3 -m venv \"${RUNNER_TEMP}/ansible-controller\"")
            }));
        }
        for step in protected {
            assert!(selected < step, "{name}: step {step}");
        }
    }
}

/// From `test_rust_native_consumers_override_the_python_pkg_config_export`:
/// the selected Python exports its own `PKG_CONFIG_PATH` after the job's
/// environment is set, so each of the twelve Rust and native consumers
/// sets the pinned OpenSSL prefix itself.
#[test]
fn rust_native_consumers_override_the_python_pkg_config_export() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "rust-test");
    let (python, setup) = located(steps, |step| uses(step).starts_with(SETUP_PYTHON));
    let exports = setup["with"].get("update-environment");
    assert!(exports.is_none_or(|exports| *exports == json!(true) || *exports == "true"));
    assert_eq!(ci["env"]["PKG_CONFIG_PATH"], CANONICAL_PKG_CONFIG);
    let consumers = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            uses(step) == "./.github/actions/native-toolchain"
                || script(step).contains("cargo ")
                || script(step).contains("./target/debug/load_test ")
        })
        .collect::<Vec<_>>();
    assert_eq!(consumers.len(), 12);
    for (index, step) in consumers {
        assert!(python < index, "{step}");
        assert_eq!(
            step["env"]["PKG_CONFIG_PATH"], CANONICAL_PKG_CONFIG,
            "{step}"
        );
    }
}

/// From `test_native_success_receipts_do_not_enable_automated_vendor_scans`:
/// no hosted job runs the native suites or the deep tier.
#[test]
fn native_success_receipts_do_not_enable_automated_vendor_scans() {
    let jobs = &workflow("security.yml")["jobs"];
    assert!(jobs.get("native-security").is_none());
    assert!(jobs.get("security-deep").is_none());
}

/// From `test_cache_receipts_are_verified_and_saved_only_on_success_without_prefix_fallback`:
/// in every job that keys a reusable success, the receipt is verified only
/// on an exact hit, saved only on a miss and never after a failure, no
/// cache falls back to a prefix, and no CI binary step interpolates an
/// expression into its script.
#[test]
fn cache_receipts_are_verified_and_saved_only_on_success_without_prefix_fallback() {
    let mut keyed = 0;
    for name in ["ci.yml", "security.yml"] {
        let parsed = workflow(name);
        for (job, value) in parsed["jobs"].as_object().unwrap() {
            let steps = value.get("steps").map_or(&[][..], |steps| {
                let steps = steps.as_array().unwrap();
                assert!(steps.iter().all(Value::is_object), "{job}");
                steps
            });
            if !steps.iter().any(|step| step["id"] == "verified-inputs") {
                continue;
            }
            keyed += 1;
            let verify = first(steps, |step| {
                script(step).contains("simplestchat-ci -- verify")
            });
            let save = first(steps, |step| {
                script(step).contains("simplestchat-ci -- save")
            });
            assert_eq!(
                verify["if"], "steps.verified.outputs.cache-hit == 'true'",
                "{job}"
            );
            assert_eq!(
                save["if"], "steps.verified.outputs.cache-hit != 'true'",
                "{job}"
            );
            assert!(!save["if"].as_str().unwrap().contains("always()"), "{job}");
            for step in steps {
                if step["id"] == "verified" {
                    assert!(step["with"].is_object(), "{job}");
                    assert!(lacks(&step["with"], &["restore-keys"]), "{job}");
                }
                if script(step).contains("simplestchat-ci -- ") {
                    assert!(!run(step).contains("${{"), "{job}");
                }
            }
        }
    }
    assert!(keyed > 0);
}
