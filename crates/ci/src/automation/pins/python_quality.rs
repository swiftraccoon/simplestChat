//! The Python checker policy and its CI wiring, carried over from the Python
//! workflow tests: strict shared settings, pinned checker versions and the
//! gate's place in the hosted jobs. The gate's discovery, tool selection and
//! failure sequencing are exercised in `xtask/src/check/check_tests.rs`.

use super::{checkout_file, first, located, script, steps, workflow};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const TOOLCHAIN_ACTION: &str = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d";
const TOOLCHAIN_STEP: &str = "Install the pinned Rust toolchain for CI receipts";
const SUBJECT_STEP: &str = "Check the branch tip commit subject";
const NOT_VERIFIED: &str = "steps.verified.outputs.cache-hit != 'true'";

/// The parsed `pyproject.toml`.
fn pyproject() -> Value {
    toml::from_str(&checkout_file("pyproject.toml")).unwrap()
}

/// A list whose every item must be text.
fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap().iter();
    items.map(|item| item.as_str().unwrap()).collect()
}

/// From `test_guest_smokes_use_the_prebuilt_owner_after_the_guarded_workspace_build`:
/// both complete operations run the same prebuilt binary under the build's
/// cache guard. Exactly one job may own them.
#[test]
fn guest_smokes_use_the_prebuilt_owner_after_the_guarded_workspace_build() {
    let ci = workflow("ci.yml");
    let shutdown = "Verify readiness and graceful shutdown";
    let owners = ci["jobs"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, job)| job.get("steps").is_some())
        .filter(|(name, _)| steps(&ci, name).iter().any(|step| step["name"] == shutdown))
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    let [owner] = owners[..] else {
        panic!("exactly one job owns the guest smokes: {owners:?}");
    };
    let steps = steps(&ci, owner);
    let (built, build) = located(steps, |step| {
        step["name"] == "Build server, load generator and test harnesses"
    });
    assert_eq!(
        build["run"],
        "cargo build --locked --workspace --all-targets --all-features --profile test"
    );
    assert!(build.get("if").is_some());
    for (kind, name) in [
        ("shutdown", shutdown),
        ("diagnostics", "Verify bounded local operation diagnostics"),
    ] {
        let (position, step) = located(steps, |step| step["name"] == name);
        let command = format!(
            "./target/debug/load_test bench {kind}-smoke \
             --binary \"$PWD/target/debug/simplestChat\" \
             --output \"$RUNNER_TEMP/{kind}-smoke\""
        );
        assert_eq!(step["run"], command);
        assert!(built < position, "{kind}");
        assert_eq!(step["if"], build["if"], "{kind}");
        assert_eq!(
            step["env"],
            json!({
                "PKG_CONFIG_PATH": "${{ github.workspace }}/target/openssl-4.1.0/lib/pkgconfig"
            }),
            "{kind}"
        );
        assert!(step.get("continue-on-error").is_none(), "{kind}");
    }
    let upload = first(steps, |step| {
        step["name"] == "Preserve media smoke diagnostics"
    });
    assert_eq!(upload["if"], "${{ !cancelled() && !env.ACT }}");
    assert_eq!(upload["with"]["retention-days"], 7);
    assert_eq!(
        upload["with"]["path"]
            .as_str()
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "${{ runner.temp }}/media-smoke",
            "${{ runner.temp }}/shutdown-smoke",
            "${{ runner.temp }}/diagnostics-smoke"
        ]
    );
}

/// From `test_configuration_metadata_retention_never_uploads_command_logs`: in
/// each job the step whose `run` is exactly the CI binary's validation comes
/// before its metadata upload and never continues on error.
#[test]
fn configuration_validation_precedes_its_metadata_upload_in_each_job() {
    let ci = workflow("ci.yml");
    for (kind, job) in [("promtool", "automation"), ("caddy", "deployment")] {
        let steps = steps(&ci, job);
        let command = format!("cargo run --locked -q -p simplestchat-ci -- validate-config {kind}");
        let (validated, validate) = located(steps, |step| step["run"] == command.as_str());
        let name = format!("Preserve {kind} validation metadata");
        let (uploaded, _) = located(steps, |step| step["name"] == name.as_str());
        assert!(validated < uploaded, "{kind}");
        assert!(validate.get("continue-on-error").is_none(), "{kind}");
    }
}

/// From `test_configuration_metadata_retention_never_uploads_command_logs`: the
/// fixed passive JSON survives failures and the private directory is never
/// retained.
#[test]
fn configuration_metadata_retention_never_uploads_command_logs() {
    let ci = workflow("ci.yml");
    for (kind, job) in [("promtool", "automation"), ("caddy", "deployment")] {
        let name = format!("Preserve {kind} validation metadata");
        let upload = first(steps(&ci, job), |step| step["name"] == name.as_str());
        assert_eq!(upload["if"], "${{ always() && !env.ACT }}", "{kind}");
        assert_eq!(
            upload["uses"], "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            "{kind}"
        );
        let path = format!(
            "${{{{ runner.temp }}}}/simplestchat-config-validation-{kind}-*/config-validation.json"
        );
        assert_eq!(
            upload["with"],
            json!({
                "name": format!("config-validation-{kind}"),
                "path": path,
                "if-no-files-found": "ignore",
                "retention-days": 7,
            }),
            "{kind}"
        );
    }
}

/// From `test_offline_configuration_validators_keep_existing_jobs_guards_and_fixed_inputs`:
/// only the two container command bodies changed, after the pinned Rust
/// bootstrap.
#[test]
fn offline_configuration_validators_keep_existing_jobs_guards_and_fixed_inputs() {
    let ci = workflow("ci.yml");
    for (kind, name, step_name, timeout) in [
        (
            "promtool",
            "automation",
            "Verify production alert rule behavior with pinned Prometheus",
            10,
        ),
        ("caddy", "deployment", "Validate Caddyfile", 95),
    ] {
        let job = &ci["jobs"][name];
        assert_eq!(job["timeout-minutes"], timeout, "{kind}");
        let steps = steps(&ci, name);
        let (position, step) = located(steps, |step| step["name"] == step_name);
        let command = format!("cargo run --locked -q -p simplestchat-ci -- validate-config {kind}");
        assert_eq!(step["run"], command);
        let (compiled, compiler) = located(steps, |step| step["uses"] == TOOLCHAIN_ACTION);
        assert!(compiled < position, "{kind}");
        assert_eq!(compiler["with"], json!({"toolchain": "1.99.0"}), "{kind}");
        assert!(compiler.get("if").is_none(), "{kind}");
        assert!(step.get("continue-on-error").is_none(), "{kind}");
        assert!(step.get("timeout-minutes").is_none(), "{kind}");
        if kind == "promtool" {
            assert_eq!(step["if"], NOT_VERIFIED);
            assert!(step.get("env").is_none());
        } else {
            assert!(step.get("if").is_none());
            let secret = format!("ci-only-proxy-secret-{}", "0".repeat(32));
            assert_eq!(step["env"], json!({"TRUSTED_PROXY_SECRET": secret}));
            assert_eq!(
                job["env"]["CADDY_IMAGE"],
                concat!(
                    "ghcr.io/swiftraccoon/simplestchat-caddy:2.11.7-",
                    "73c81fdbfe625d3df3989ba807064c81fbe2c071@sha256:",
                    "23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
                )
            );
        }
    }
}

/// From `test_branch_subject_checks_the_head_before_any_cached_gate`: the typed
/// byte-policy check stays unconditional after the existing bootstrap.
#[test]
fn branch_subject_checks_the_head_before_any_cached_gate() {
    let ci = workflow("ci.yml");
    let job = &ci["jobs"]["automation"];
    let steps = steps(&ci, "automation");
    assert_eq!(job["timeout-minutes"], 10);
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(steps[0]["with"]["fetch-depth"], 2);
    assert_eq!(steps[1]["if"], "${{ env.ACT }}");
    assert_eq!(steps[1]["run"], "build/ci-local-docker.sh");
    let [rust, subject, fingerprint] = &steps[3..6] else {
        panic!("three steps follow the bootstrap");
    };
    assert_eq!(rust["name"], TOOLCHAIN_STEP);
    assert_eq!(rust["uses"], TOOLCHAIN_ACTION);
    assert_eq!(rust["with"], json!({"toolchain": "1.99.0"}));
    assert_eq!(subject["name"], SUBJECT_STEP);
    assert_eq!(
        subject["env"],
        json!({"COMMIT_REVISION": "${{ github.event.pull_request.head.sha || github.sha }}"})
    );
    assert_eq!(
        subject["run"],
        "cargo run --locked -q -p simplestchat-ci -- commit-subject"
    );
    assert_eq!(fingerprint["id"], "verified-inputs");
    for scope in [job, rust, subject] {
        assert!(scope.get("if").is_none());
        assert!(scope.get("continue-on-error").is_none());
    }
    let subjects = steps.iter().filter(|step| step["name"] == SUBJECT_STEP);
    assert_eq!(subjects.count(), 1);
}

/// From `test_configuration_has_full_rules_and_no_machine_specific_environment`:
/// strict rule sets, no blanket type suppression and no local venv binding.
#[test]
fn configuration_has_full_rules_and_no_machine_specific_environment() {
    let config = pyproject();
    let ruff = &config["tool"]["ruff"];
    assert_eq!(strings(&ruff["lint"]["select"]), ["ALL"]);
    let excluded = strings(&ruff["exclude"]);
    assert!(!excluded.contains(&"build") && !excluded.contains(&"build/"));
    let based = config["tool"]["basedpyright"].as_object().unwrap();
    assert_eq!(based["typeCheckingMode"], "all");
    for key in ["venvPath", "venv", "ignore"] {
        assert!(!based.contains_key(key), "{key}");
    }
    assert_eq!(based["stubPath"], "typings");
    let included = strings(&based["include"]);
    for path in [
        "build",
        "ops/ansible/files",
        "ops/ansible/callback_plugins",
        "ops/ansible/tests",
        "typings",
    ] {
        assert!(included.contains(&path), "{path}");
    }
    for (name, value) in based {
        if name.starts_with("report") && name != "reportImplicitRelativeImport" {
            assert!(*value != json!(false) && *value != "none", "{name}");
        }
    }
}

/// From `test_checker_requirements_pin_versions_and_share_controller_dependencies`:
/// one pinned checker and controller environment, with no VPS tool additions.
#[test]
fn checker_requirements_pin_versions_and_share_controller_dependencies() {
    let text = checkout_file("build/python-requirements.txt");
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.trim())
        .map(|line| line.strip_suffix('\\').unwrap_or(line).trim())
        .collect::<BTreeSet<_>>();
    for line in [
        "-r ../ops/ansible/requirements.txt",
        "--require-hashes",
        "--only-binary=:all:",
    ] {
        assert!(lines.contains(line), "{line}");
    }
    for name in ["ruff", "basedpyright", "types-pyyaml"] {
        let pin = format!("{name}==");
        let pins = lines.iter().filter(|line| line.starts_with(&pin));
        assert_eq!(pins.count(), 1, "{name}");
    }
    let config = pyproject();
    let required = config["tool"]["ruff"]["required-version"].as_str().unwrap();
    assert!(lines.contains(format!("ruff{required}").as_str()));
}

/// From `test_ci_installs_and_runs_the_same_gate_before_offline_tests`: the
/// temporary CI environment is explicit and no failure is ignored.
#[test]
fn ci_installs_and_runs_the_same_gate_before_offline_tests() {
    let ci = workflow("ci.yml");
    let job = &ci["jobs"]["automation"];
    let steps = steps(&ci, "automation");
    let (installed, install) = located(steps, |step| {
        script(step).contains("install -r build/python-requirements.txt")
    });
    let (gated, gate) = located(steps, |step| {
        script(step).contains("simplestchat-ci -- automation")
    });
    let (compiled, rust) = located(steps, |step| step["name"] == TOOLCHAIN_STEP);
    assert!(installed < gated);
    assert!(compiled < gated);
    assert!(compiled < installed);
    assert_eq!(install["if"], NOT_VERIFIED);
    assert_eq!(
        script(install).lines().collect::<Vec<_>>(),
        [
            "sudo apt-get update",
            "sudo apt-get install -y python3-venv shellcheck jq postgresql",
            "python3 -m venv \"${RUNNER_TEMP}/ansible-controller\"",
            concat!(
                "\"${RUNNER_TEMP}/ansible-controller/bin/pip\" install -r ",
                "build/python-requirements.txt"
            ),
            "cargo run --locked -q -p simplestchat-ci -- controller-path",
        ]
    );
    assert!(install.get("continue-on-error").is_none());
    assert_eq!(rust["uses"], TOOLCHAIN_ACTION);
    assert_eq!(rust["with"]["toolchain"], "1.99.0");
    assert!(rust.get("if").is_none());
    let (fingerprinted, _) = located(steps, |step| step["id"] == "verified-inputs");
    assert!(compiled < fingerprinted);
    assert_eq!(
        gate["run"],
        "cargo run --locked -q -p simplestchat-ci -- automation --checks"
    );
    assert_eq!(
        gate["env"]["PYTHON_CHECK_ENV"],
        "${{ runner.temp }}/ansible-controller"
    );
    assert!(job.get("continue-on-error").is_none());
    assert!(gate.get("continue-on-error").is_none());
    assert!(job.get("if").is_none());
    assert_eq!(gate["if"], NOT_VERIFIED);
    let (verified_at, verified) = located(steps, |step| {
        script(step).contains("simplestchat-ci -- verify")
    });
    assert_eq!(verified["if"], "steps.verified.outputs.cache-hit == 'true'");
    assert_eq!(verified["env"]["CI_CACHE_SCOPE"], "automation");
    assert!(verified_at < gated);
    assert!(verified.get("continue-on-error").is_none());
}
