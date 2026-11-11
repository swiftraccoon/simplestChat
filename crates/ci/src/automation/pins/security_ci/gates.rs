//! The required aggregate, the signing boundary and the shared source gates.

use super::super::{
    checkout_file, document, first, lacks, located, run, script, steps, uses, workflow,
};
use super::{NOT_VERIFIED, TOOLCHAIN_ACTION, keys};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const MAIN_PUSH: &str = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}";
const GATES: [&str; 10] = [
    "automation",
    "browser",
    "codeql",
    "deployment",
    "image-database",
    "native-dtls",
    "rust-lint",
    "rust-test",
    "security",
    "web",
];

/// The lines of a block scalar.
fn lines(value: &Value) -> Vec<&str> {
    value.as_str().unwrap().lines().collect()
}

/// The steps whose `uses` contains `action`.
fn using<'a>(steps: &'a [Value], action: &str) -> Vec<&'a Value> {
    let selected = steps.iter().filter(|step| uses(step).contains(action));
    selected.collect()
}

/// From `test_native_dtls_cache_miss_bootstrap_controller_and_source_closure`:
/// only exact native and tooling inputs reuse the unchanged bounded native job.
#[test]
fn native_dtls_cache_miss_bootstrap_controller_and_source_closure() {
    let ci = workflow("ci.yml");
    assert_eq!(ci["jobs"]["native-dtls"]["timeout-minutes"], 25);
    let steps = steps(&ci, "native-dtls");
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    let lookup = &steps[1];
    assert_eq!(lookup["with"]["lookup-only"], true);
    let key = lookup["with"]["key"].as_str().unwrap();
    assert!(key.starts_with("native-dtls-v2-"));
    assert!(key.contains("'main' || 'untrusted'"));
    let bootstrap = &steps[2];
    assert_eq!(bootstrap["if"], NOT_VERIFIED);
    assert_eq!(bootstrap["uses"], TOOLCHAIN_ACTION);
    assert_eq!(bootstrap["with"], json!({"toolchain": "1.99.0"}));
    let checks = steps
        .iter()
        .filter(|step| script(step).contains("native-dtls"))
        .collect::<Vec<_>>();
    let [check] = checks[..] else {
        panic!("exactly one step runs the native DTLS check");
    };
    assert_eq!(check["if"], NOT_VERIFIED);
    assert_eq!(check["timeout-minutes"], 12);
    assert!(
        steps
            .iter()
            .all(|step| !script(step).contains("native-dtls-verified"))
    );
    // Every native or tooling input changes the key; a browser source does not.
    let hashed = key.split_once("hashFiles(").unwrap().1;
    let hashed = hashed.split_once(')').unwrap().0;
    let patterns = regex::Regex::new("'([^']+)'")
        .unwrap()
        .captures_iter(hashed)
        .map(|found| found[1].to_owned())
        .collect::<Vec<_>>();
    let selects = |path: &str| {
        patterns
            .iter()
            .any(|pattern| match pattern.strip_suffix("/**") {
                Some(directory) => {
                    assert!(!directory.contains('*'), "{pattern}");
                    path.strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
                }
                None => {
                    assert!(!pattern.contains('*'), "{pattern}");
                    pattern == path
                }
            })
    };
    for path in [
        "vendor/worker.cpp",
        "crates/ci/src/process.rs",
        "crates/ci/src/bounded_log.rs",
        "Cargo.toml",
        "Cargo.lock",
        "xtask/Cargo.toml",
        "load_tests/Cargo.toml",
        "ops/Cargo.toml",
        "ops/ops-build-inputs.lock.json",
        "ops/src/lib.rs",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        ".github/workflows/ci.yml",
        "build/install-openssl.sh",
        "build/pip-constraints.txt",
        "build/check-native-dtls.sh",
    ] {
        assert!(selects(path), "{path}");
    }
    assert!(!selects("web/src/main.ts"));
}

/// From `test_aggregate_rejects_missing_skipped_and_failed_jobs`: every
/// maintained correctness and security job is in the always-run aggregate.
#[test]
fn aggregate_rejects_missing_skipped_and_failed_jobs() {
    let ci = workflow("ci.yml");
    let gate = &ci["jobs"]["required"];
    assert_eq!(gate["if"], "${{ always() }}");
    let needs = gate["needs"].as_array().unwrap().iter();
    let needs = needs.map(|need| need.as_str().unwrap());
    assert_eq!(needs.collect::<BTreeSet<_>>(), GATES.into());
    let separate = [
        "required",
        "release-security",
        "release-prepare",
        "ops-build",
        "ops-release-security",
    ];
    let mut jobs = keys(&ci["jobs"]);
    jobs.retain(|job| !separate.contains(job));
    assert_eq!(jobs, GATES.into());
    let steps = steps(&ci, "required");
    let (position, aggregate) = located(steps, |step| {
        step["name"] == "Require every declared gate to finish successfully"
    });
    assert_eq!(
        aggregate["env"],
        json!({"GATE_RESULTS": "${{ toJSON(needs) }}"})
    );
    assert_eq!(
        aggregate["run"],
        "cargo run --locked -q -p simplestchat-ci -- required"
    );
    assert!(lacks(aggregate, &["if", "continue-on-error"]));
    assert_eq!(position, 2);
    assert!(lacks(gate, &["continue-on-error"]));
}

/// From `test_aggregate_checks_stored_codeql_health_before_signing`: green
/// jobs do not certify failed ingestion, and fork pull requests use the same
/// read-only gate.
#[test]
fn aggregate_checks_stored_codeql_health_before_signing() {
    let ci = workflow("ci.yml");
    let gate = &ci["jobs"]["required"];
    assert_eq!(
        gate["permissions"],
        json!({"contents": "read", "security-events": "read"})
    );
    assert_eq!(gate["timeout-minutes"], 7);
    let steps = steps(&ci, "required");
    assert!(uses(&steps[0]).starts_with("actions/checkout@"));
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    let bootstrap = &steps[1];
    assert_eq!(bootstrap["uses"], TOOLCHAIN_ACTION);
    assert_eq!(bootstrap["with"], json!({"toolchain": "1.99.0"}));
    assert!(lacks(bootstrap, &["if", "env"]));
    assert!(lacks(gate, &["env"]));
    let (position, health) = located(steps, |step| {
        script(step).contains("build/security_codeql_triage.py health")
    });
    assert_eq!(health["env"], json!({"GH_TOKEN": "${{ github.token }}"}));
    assert_eq!(health["if"], "${{ !env.ACT }}");
    assert!(lacks(health, &["continue-on-error"]));
    assert!(position > 2);
    for argument in [
        "python3 build/security_codeql_triage.py health",
        "--repository \"$GITHUB_REPOSITORY\"",
        "--revision \"$GITHUB_SHA\"",
        "--ref \"$GITHUB_REF\"",
        "--output \"$RUNNER_TEMP/codeql-analysis-health\"",
    ] {
        assert!(run(health).contains(argument), "{argument}");
    }
    let upload = first(steps, |step| {
        uses(step).starts_with("actions/upload-artifact@")
    });
    assert_eq!(upload["if"], "${{ always() && !env.ACT }}");
    assert_eq!(
        lines(&upload["with"]["path"]),
        [
            "${{ runner.temp }}/codeql-analysis-health/report.json",
            "${{ runner.temp }}/codeql-analysis-health/failure.json",
        ]
    );
}

/// From `test_browser_suite_commands_preserve_groups_opt_ins_timeouts_and_failure_guard`:
/// the two-browser plans keep the existing job orchestration and suite opt-ins.
#[test]
fn browser_suite_commands_preserve_groups_opt_ins_timeouts_and_failure_guard() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "browser");
    let presentation = concat!(
        "${{ !cancelled() && matrix.group == 'media' && ",
        "steps.browser-tooling.outcome == 'success' }}"
    );
    for (name, guard, minutes, environment) in [
        (
            "Authentication dialog and diagnostic privacy in Chromium and Firefox",
            "matrix.group == 'accounts'",
            4,
            json!({"AUTH_LIFECYCLE_E2E": "1"}),
        ),
        (
            "Shared account state and room navigation in Chromium and Firefox",
            "matrix.group == 'accounts'",
            4,
            json!({"AUTH_CROSS_TAB_E2E": "1"}),
        ),
        (
            "Confirm room controls and retain failed edits",
            "matrix.group == 'accounts'",
            4,
            json!({"ROOM_CONTROL_E2E": "1"}),
        ),
        (
            "Media continuity and device controls in Chromium and Firefox",
            "matrix.group == 'media'",
            4,
            json!({"MEDIA_CONTINUITY_E2E": "1"}),
        ),
        (
            "Read presentations through constrained and recovered browser media",
            presentation,
            6,
            json!({"PRESENTATION_E2E": "1", "PRESENTATION_IMPAIRMENT": "1"}),
        ),
    ] {
        let step = first(steps, |step| step["name"] == name);
        assert_eq!(step["if"], guard, "{name}");
        assert_eq!(step["env"], environment, "{name}");
        assert_eq!(step["timeout-minutes"], minutes, "{name}");
        assert!(lacks(step, &["continue-on-error"]), "{name}");
    }
}

/// From `test_preparation_has_only_read_authority_after_all_required_checks`:
/// compilation precedes signing authority and keeps the late-expiry phase.
#[test]
fn preparation_has_only_read_authority_after_all_required_checks() {
    let ci = workflow("ci.yml");
    let prepare = &ci["jobs"]["release-prepare"];
    assert_eq!(prepare["if"], MAIN_PUSH);
    assert_eq!(prepare["needs"], json!(["required"]));
    assert_eq!(prepare["permissions"], json!({"contents": "read"}));
    assert_eq!(prepare["timeout-minutes"], 15);
    assert!(lacks(prepare, &["continue-on-error"]));
    let steps = steps(&ci, "release-prepare");
    assert!(steps.iter().all(|step| lacks(step, &["continue-on-error"])));
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    let commands = steps
        .iter()
        .filter(|step| step.get("run").is_some())
        .collect::<Vec<_>>();
    let [compilation, operation] = commands[..] else {
        panic!("exactly two steps run commands");
    };
    for text in [
        "cargo build --locked --release -p simplestchat-ops",
        "--target-dir \"$RUNNER_TEMP/release-prepare-build\"",
    ] {
        assert!(run(compilation).contains(text), "{text}");
    }
    assert!(lacks(compilation, &["env"]));
    for text in [
        "\"$RUNNER_TEMP/release-prepare-build/release/simplestchat-ops\"",
        "release attestation prepare",
        "--timeout-seconds 300 --finalization-seconds 30",
        "--run-id \"$GITHUB_RUN_ID\" --run-attempt \"$GITHUB_RUN_ATTEMPT\"",
    ] {
        assert!(run(operation).contains(text), "{text}");
    }
    assert!(operation["timeout-minutes"].as_u64().unwrap() * 60 >= 330);
    assert!(lacks(operation, &["env"]));
    let downloads = using(steps, "actions/download-artifact@");
    let names = |steps: &[&Value]| {
        let names = steps.iter().map(|step| step["with"]["name"].clone());
        names.collect::<Vec<_>>()
    };
    assert_eq!(
        names(&downloads),
        [
            "simplestchat-unsigned-${{ github.sha }}",
            "production-security-${{ github.sha }}"
        ]
    );
    for step in &downloads {
        assert!(lacks(
            &step["with"],
            &["run-id", "repository", "github-token"]
        ));
    }
    let upload = using(steps, "actions/upload-artifact@")[0];
    assert_eq!(
        upload["with"]["name"],
        "production-signing-metadata-${{ github.sha }}"
    );
    let metadata = [
        "sbom.spdx.json",
        "image-security.json",
        "runtime-proof.json",
        "vex.openvex.json",
        "release-predicate.json",
    ];
    let original = ["image.tar", "release.json", "outcome.json", "source.json"];
    assert!(original.iter().all(|name| !metadata.contains(name)));
    let prepared = |name: &&str| format!("${{{{ runner.temp }}}}/prepared-release/{name}");
    assert_eq!(
        lines(&upload["with"]["path"])
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
        metadata.iter().map(prepared).collect()
    );
    assert_eq!(upload["with"]["if-no-files-found"], "error");
    let signer = self::steps(&ci, "release-security");
    let signer_downloads = using(signer, "actions/download-artifact@");
    assert_eq!(
        names(&signer_downloads),
        [
            "simplestchat-unsigned-${{ github.sha }}",
            "production-signing-metadata-${{ github.sha }}",
        ]
    );
    for step in &signer_downloads {
        assert_eq!(step["with"]["path"], "${{ runner.temp }}/signed-release");
    }
    for step in signer {
        for text in [
            "cargo",
            "release_attestation.py prepare",
            "simplestchat-ops",
        ] {
            assert!(!script(step).contains(text), "{text}");
        }
    }
}

/// From `test_signing_only_follows_all_main_push_gates`: pull requests and
/// partial CI runs cannot request a signing identity.
#[test]
fn signing_only_follows_all_main_push_gates() {
    let ci = workflow("ci.yml");
    let identity = json!({"contents": "read", "id-token": "write", "attestations": "write"});
    let signer = &ci["jobs"]["release-security"];
    assert_eq!(signer["if"], MAIN_PUSH);
    assert_eq!(signer["needs"], json!(["required", "release-prepare"]));
    assert_eq!(signer["runs-on"], "ubuntu-24.04");
    assert_eq!(signer["permissions"], identity);
    let ops_signer = &ci["jobs"]["ops-release-security"];
    assert_eq!(ops_signer["if"], MAIN_PUSH);
    assert_eq!(ops_signer["needs"], json!(["required", "ops-build"]));
    assert_eq!(ops_signer["permissions"], identity);
    for (name, job) in ci["jobs"].as_object().unwrap() {
        if name == "release-security" || name == "ops-release-security" {
            continue;
        }
        assert!(job.is_object(), "{name}");
        if let Some(permissions) = job.get("permissions") {
            assert!(permissions.is_object(), "{name}");
            assert!(lacks(permissions, &["id-token", "attestations"]), "{name}");
        }
    }
    let steps = steps(&ci, "release-security");
    let downloads = steps
        .iter()
        .filter(|step| uses(step).starts_with("actions/download-artifact@"))
        .collect::<Vec<_>>();
    assert_eq!(downloads.len(), 2);
    for step in downloads {
        assert!(lacks(
            &step["with"],
            &["run-id", "github-token", "repository"]
        ));
    }
    let attest = first(steps, |step| step["id"] == "attest");
    let pinned = regex::Regex::new("^actions/attest@[a-f0-9]{40}$").unwrap();
    assert!(pinned.is_match(uses(attest)));
    assert_eq!(
        lines(&attest["with"]["subject-path"]),
        [
            "${{ runner.temp }}/signed-release/image.tar",
            "${{ runner.temp }}/signed-release/sbom.spdx.json",
            "${{ runner.temp }}/signed-release/runtime-proof.json",
            "${{ runner.temp }}/signed-release/vex.openvex.json",
        ]
    );
    assert_eq!(attest["with"]["create-storage-record"], false);
    let uploads = steps
        .iter()
        .filter(|step| uses(step).starts_with("actions/upload-artifact@"))
        .collect::<Vec<_>>();
    let [upload] = uploads[..] else {
        panic!("exactly one upload");
    };
    let signed = |name: &&str| format!("${{{{ runner.temp }}}}/signed-release/{name}");
    let expected = [
        "image.tar",
        "release.json",
        "outcome.json",
        "source.json",
        "sbom.spdx.json",
        "image-security.json",
        "runtime-proof.json",
        "vex.openvex.json",
        "release-predicate.json",
        "release-attestation.jsonl",
    ];
    assert_eq!(
        lines(&upload["with"]["path"])
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
        expected.iter().map(signed).collect()
    );
}

/// From `test_reusable_checks_share_revision_and_do_not_inherit_secrets`:
/// called analysis uses the caller checkout and its own concurrency group.
#[test]
fn reusable_checks_share_revision_and_do_not_inherit_secrets() {
    let ci = workflow("ci.yml");
    for name in ["security", "codeql"] {
        let caller = &ci["jobs"][name];
        assert_eq!(caller["uses"], format!("./.github/workflows/{name}.yml"));
        assert!(lacks(caller, &["secrets"]), "{name}");
        let called = workflow(&format!("{name}.yml"));
        let triggers = keys(&called["on"]);
        assert!(triggers.contains("workflow_call"), "{name}");
        assert!(!triggers.contains("push"), "{name}");
        assert!(!triggers.contains("pull_request"), "{name}");
        let group = called["concurrency"]["group"].as_str().unwrap();
        assert!(group.starts_with(&format!("{name}-")), "{name}");
        assert!(!called["on"]["schedule"].as_array().unwrap().is_empty());
    }
}

/// From `test_all_workflows_use_unprivileged_pr_events_and_pinned_actions`:
/// external action code is pinned and no job runs on a privileged
/// pull-request event.
#[test]
fn all_workflows_use_unprivileged_pr_events_and_pinned_actions() {
    let pinned = regex::Regex::new("^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[a-f0-9]{40}$").unwrap();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.github/workflows");
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".yml"))
        .collect::<Vec<_>>();
    names.sort();
    assert!(names.iter().any(|name| name == "ci.yml"));
    for name in names {
        let value = workflow(&name);
        assert!(
            !keys(&value["on"]).contains("pull_request_target"),
            "{name}"
        );
        for job in value["jobs"].as_object().unwrap().values() {
            assert!(job.is_object(), "{name}");
            let Some(steps) = job.get("steps") else {
                continue;
            };
            for step in steps.as_array().unwrap() {
                assert!(step.is_object(), "{name}");
                let action = uses(step);
                let local = action.starts_with("./") || action.starts_with("$/");
                if !action.is_empty() && !local {
                    assert!(pinned.is_match(action), "{name}: {action}");
                }
            }
        }
    }
}

/// From `test_vendor_security_suites_are_explicit_local_only`: no hosted,
/// scheduled or complete-local workflow executes vendor source suites.
#[test]
fn vendor_security_suites_are_explicit_local_only() {
    let security = workflow("security.yml");
    assert_eq!(
        keys(&security["jobs"]),
        ["security-fast", "security-mutation"].into()
    );
    for job in ["security-fast", "security-mutation"] {
        for step in steps(&security, job) {
            for forbidden in [
                "native_security.py",
                "--include-vendor",
                "--deep-check native",
            ] {
                assert!(!script(step).contains(forbidden), "{job}: {forbidden}");
            }
        }
    }
}

/// From `test_dependency_reviews_are_shared_by_direct_main_and_local_source_checks`:
/// changed npm, Python and Actions dependencies keep their policy checks
/// without a pull-request job.
#[test]
fn dependency_reviews_are_shared_by_direct_main_and_local_source_checks() {
    let security = workflow("security.yml");
    assert!(lacks(&security["jobs"], &["dependency-review"]));
    assert!(lacks(&security["jobs"]["security-fast"], &["if"]));
    let gate = first(steps(&security, "security-fast"), |step| {
        script(step).contains("build/check-security.sh fast")
    });
    assert!(lacks(gate, &["if"]));
    assert_eq!(
        gate["env"]["SECURITY_BASE"],
        "${{ github.event.pull_request.base.sha || github.event.before }}"
    );
    let source = checkout_file("build/security_check.py");
    for text in [
        concat!(
            "security_dependency_licenses.check(context, snapshot, base, ",
            "include_vendor=include_vendor)"
        ),
        "security_actions.check(context, snapshot, base, include_vendor=include_vendor)",
        "vulnerabilities[\"total\"] == 0",
        "\"pip_audit\"",
    ] {
        assert!(source.contains(text), "{text}");
    }
}

/// From `test_source_security_jobs_bootstrap_rust_before_running_shared_checks`:
/// fresh local images need the pinned installer the native CI jobs use.
#[test]
fn source_security_jobs_bootstrap_rust_before_running_shared_checks() {
    let native = document(".github/actions/native-toolchain/action.yml");
    let composite = native["runs"]["steps"].as_array().unwrap();
    let existing = first(composite, |step| {
        uses(step).starts_with("dtolnay/rust-toolchain@")
    });
    assert!(existing["with"]["toolchain"].is_string());
    let security = workflow("security.yml");
    let steps = steps(&security, "security-fast");
    let installers = steps
        .iter()
        .filter(|step| uses(step).starts_with("dtolnay/rust-toolchain@"))
        .collect::<Vec<_>>();
    let [installer] = installers[..] else {
        panic!("exactly one toolchain installer");
    };
    assert_eq!(installer["uses"], existing["uses"]);
    assert_eq!(
        installer["with"],
        json!({"toolchain": existing["with"]["toolchain"]})
    );
    assert!(lacks(installer, &["if", "continue-on-error"]));
    let (installed, _) = located(steps, |step| std::ptr::eq(step, installer));
    let (gated, _) = located(steps, |step| {
        script(step).contains("build/check-security.sh")
    });
    assert!(installed < gated);
    assert!(
        steps
            .iter()
            .all(|step| !script(step).contains("rustup toolchain install"))
    );
}

/// From `test_npm_path_publication_follows_installer_and_pinned_rust_bootstrap`:
/// every npm path publication is reachable with Rust and keeps installer
/// failure fatal.
#[test]
fn npm_path_publication_follows_installer_and_pinned_rust_bootstrap() {
    let minimal = "rustup toolchain install --profile minimal";
    for (filename, jobs) in [
        (
            "ci.yml",
            &["rust-lint", "rust-test", "browser", "web", "deployment"][..],
        ),
        ("performance.yml", &["compare", "impaired", "soak"][..]),
        ("browser-compatibility.yml", &["browser"][..]),
        ("security.yml", &["security-fast", "security-mutation"][..]),
        ("ice-isolation.yml", &["same-host"][..]),
    ] {
        let document = workflow(filename);
        for job in jobs {
            let site = format!("{filename}: {job}");
            let steps = steps(&document, job);
            let (installed, install) =
                located(steps, |step| step["name"] == "Install checksum-pinned npm");
            let commands = run(install).lines().filter(|line| !line.starts_with('#'));
            let mut expected = vec![
                "node build/install-npm.mjs \"$RUNNER_TEMP/simplestchat-npm-12.2.0\"",
                "cargo run --locked -q -p simplestchat-ci -- npm-path",
            ];
            if filename == "ice-isolation.yml" {
                expected.insert(1, minimal);
            }
            assert_eq!(commands.collect::<Vec<_>>(), expected, "{site}");
            assert!(lacks(install, &["continue-on-error"]), "{site}");
            if filename == "ci.yml" && *job == "rust-test" {
                assert_eq!(install["if"], NOT_VERIFIED, "{site}");
            } else {
                assert!(lacks(install, &["if"]), "{site}");
            }
            assert_eq!(
                install["working-directory"], "${{ github.workspace }}",
                "{site}"
            );
            let bootstraps = |step: &Value| {
                lacks(step, &["if"])
                    && (uses(step).starts_with("dtolnay/rust-toolchain@")
                        || step["uses"] == "./.github/actions/native-toolchain"
                        || script(step).lines().any(|line| line == minimal))
            };
            let (bootstrapped, bootstrap) = located(steps, bootstraps);
            assert!(bootstrapped <= installed, "{site}");
            if bootstrapped == installed {
                assert_eq!(filename, "ice-isolation.yml", "{site}");
            } else if uses(bootstrap).starts_with("dtolnay/rust-toolchain@") {
                assert_eq!(bootstrap["uses"], TOOLCHAIN_ACTION, "{site}");
                assert_eq!(bootstrap["with"]["toolchain"], "1.99.0", "{site}");
            }
        }
    }
}

/// From `test_scheduled_mutations_use_shared_gate_and_never_publish_build_outputs`:
/// test-quality jobs have bounded scope and cannot promote mutated binaries.
#[test]
fn scheduled_mutations_use_shared_gate_and_never_publish_build_outputs() {
    let security = workflow("security.yml");
    let mutation = &security["jobs"]["security-mutation"];
    assert_eq!(mutation["timeout-minutes"], 75);
    assert_eq!(
        mutation["if"],
        "${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}"
    );
    assert!(lacks(mutation, &["permissions", "services"]));
    let steps = steps(&security, "security-mutation");
    let command = steps.iter().map(script).collect::<Vec<_>>().join("\n");
    assert!(command.contains("cargo fetch --locked"));
    assert!(command.contains("build/check-security.sh deep --deep-check mutation"));
    let [upload] = using(steps, "upload-artifact@")[..] else {
        panic!("exactly one upload");
    };
    assert_eq!(
        lines(&upload["with"]["path"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        [
            "${{ runner.temp }}/security-mutation/summary.json",
            "${{ runner.temp }}/security-mutation/mutation/outcome.json",
            "${{ runner.temp }}/security-mutation/mutation/summary.json",
            "${{ runner.temp }}/security-mutation/mutation/inventory.json",
        ]
        .into()
    );
    assert!(lacks(&security["jobs"], &["security-deep"]));
    assert!(!command.contains("--include-vendor"));
}
