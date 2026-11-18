//! The CodeQL workflow: called security analysis and standalone advisory runs.

use super::super::{
    checkout_file, document, first, lacks, located, run, script, steps, uses, workflow,
};
use super::{TOOLCHAIN_ACTION, keys};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicU32, Ordering};

const JOB: &str = "source-analysis";
const HOSTED: &str = "${{ !env.ACT && matrix.language != 'rust' }}";
/// The security gate as each job's one build leaves it.
const SECURITY: &str = "\"$RUNNER_TEMP/codeql-tooling/debug/simplestchat-security\"";
/// The CI tool as the Rust job's one build leaves it.
const CI_TOOL: &str = "\"$RUNNER_TEMP/codeql-tooling/debug/simplestchat-ci\"";
/// That one build: the gate without the image checks it does not run, and
/// for Rust the CI tool beside it, into a directory no Cargo setting moves.
const BUILD: &str = concat!(
    "packages=(-p simplestchat-security)\n",
    "if [[ \"$CODEQL_LANGUAGE\" == rust ]]; then\n",
    "  packages+=(-p simplestchat-ci)\n",
    "fi\n",
    "cargo build --locked -q --no-default-features",
    " --target-dir \"$RUNNER_TEMP/codeql-tooling\" \"${packages[@]}\"\n",
);
/// The part of the native build that takes the driver's compilers.
const JOBS: &str = concat!(
    "case \"${CODEQL_BUILD_JOBS-}\" in\n",
    "  1 | 2 | 3 | 4) MEDIASOUP_BUILD_JOBS=\"$CODEQL_BUILD_JOBS\" ;;\n",
    "  *)\n",
    "    echo 'CODEQL_BUILD_JOBS must be the driver budget: 1 to 4 compilers.' >&2\n",
    "    exit 1\n",
    "    ;;\n",
    "esac\n",
    "export MEDIASOUP_BUILD_JOBS\n",
    "printf 'CodeQL traced compile workers: %s\\n' \"$MEDIASOUP_BUILD_JOBS\"\n",
);

/// The lines of a block scalar, as a set.
fn line_set(value: &Value) -> BTreeSet<&str> {
    value.as_str().unwrap().lines().collect()
}

/// From `test_codeql_advisory_runs_are_scheduled_or_manual_and_security_remains_reusable`:
/// direct-main development keeps every security scan without duplicate
/// advisory pull-request jobs.
#[test]
fn codeql_advisory_runs_are_scheduled_or_manual_and_security_remains_reusable() {
    let codeql = workflow("codeql.yml");
    assert_eq!(
        keys(&codeql["on"]),
        ["workflow_call", "schedule", "workflow_dispatch"].into()
    );
    let gate = &codeql["on"]["workflow_call"]["inputs"]["security_gate"];
    assert_eq!(gate["type"], "boolean");
    assert_eq!(gate["default"], true);
    assert!(lacks(&workflow("ci.yml")["jobs"]["codeql"], &["with"]));
    let dispatch = &codeql["on"]["workflow_dispatch"];
    assert!(dispatch.is_null() || lacks(dispatch, &["inputs"]));
    assert_eq!(keys(&codeql["jobs"]), [JOB].into());
    assert_eq!(
        codeql["jobs"][JOB]["strategy"]["matrix"]["language"],
        json!(["actions", "javascript-typescript", "python", "rust"])
    );
    assert_eq!(
        codeql["concurrency"]["group"],
        "codeql-${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}"
    );
}

/// From `test_codeql_routes_called_security_and_standalone_advisory_suites`:
/// called and standalone modes select matching query and category pairs.
#[test]
fn codeql_routes_called_security_and_standalone_advisory_suites() {
    let codeql = workflow("codeql.yml");
    let steps = steps(&codeql, JOB);
    let init = first(steps, |step| uses(step).contains("codeql-action/init@"));
    let analyze = first(steps, |step| uses(step).contains("codeql-action/analyze@"));
    assert_eq!(
        init["with"]["queries"],
        "${{ inputs.security_gate && 'security-extended' || 'security-and-quality' }}"
    );
    assert_eq!(
        analyze["with"]["category"],
        concat!(
            "/language:${{ matrix.language }}",
            "/${{ inputs.security_gate && 'security' || 'quality-advisory' }}"
        )
    );
    assert_eq!(init["if"], HOSTED);
    assert_eq!(analyze["if"], HOSTED);
}

/// From `test_optional_native_codeql_retains_real_worker_build`: the local
/// opt-in keeps native extraction available without a hosted job.
#[test]
fn optional_native_codeql_retains_real_worker_build() {
    let command = checkout_file("build/codeql-native-build.sh");
    let setup = "python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 setup";
    let generator = "-j \"$MEDIASOUP_BUILD_JOBS\" flatbuffers-generator";
    let worker = "python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 libmediasoup-worker";
    for text in [
        "libmediasoup-worker",
        "mktemp -d \"$project_root/target/codeql-worker.XXXXXXXX\"",
        JOBS,
        "NINJA=\"$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/ninja\"",
        "\"$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/meson\" compile -C \"$BUILD_DIR\"",
        "export MEDIASOUP_BUILDTYPE=Release",
        "CodeQL FlatBuffers generator:",
        "CodeQL worker library:",
    ] {
        assert!(command.contains(text), "{text}");
    }
    assert!(!command.contains("docker"));
    assert!(!command.contains("-Doptimization"));
    assert!(!command.contains("build/security_"));
    assert_eq!(command.matches("MEDIASOUP_BUILD_JOBS=").count(), 1);
    let at = |text: &str| command.find(text).unwrap();
    assert!(at("set -euo pipefail\n") < at(JOBS));
    assert!(at(JOBS) < at(setup));
    assert!(at(setup) < at(generator));
    assert!(at(generator) < at(worker));
}

/// The native build took its compilers from the reference's resource
/// budget, run as a program under the tracer, which printed the
/// `CODEQL_BUILD_JOBS` the driver handed it, re-checked against its own
/// budget, or its own budget when none was handed. Its recorded answers for
/// every count the driver can hand over, and for malformed ones, are the
/// native build's own; with no count at all the build now refuses, since
/// the driver always hands one over.
#[test]
fn the_native_build_takes_the_drivers_compilers_as_the_reference_printed_them() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_native_jobs.json")).unwrap();
    // The driver hands over no more than its own budget of this host. The
    // recording's host allowed four, so every count the driver can hand over
    // was within the budget the reference re-checked.
    assert_eq!(recorded["hostWorkers"], 4);
    let mut tally = [0; 3];
    for case in recorded["cases"].as_array().unwrap() {
        let name = format!("{:?}", case["value"]);
        let mut shell = std::process::Command::new("/bin/bash");
        shell.args(["-euo", "pipefail", "-c", JOBS]);
        shell.env_clear().env("PATH", "/usr/bin:/bin");
        if let Some(value) = case["value"].as_str() {
            shell.env("CODEQL_BUILD_JOBS", value);
        }
        let ran = shell.output().unwrap();
        let printed = String::from_utf8(ran.stdout).unwrap();
        let complained = String::from_utf8(ran.stderr).unwrap();
        let refusal = "CODEQL_BUILD_JOBS must be the driver budget: 1 to 4 compilers.\n";
        if case["value"].is_null() {
            assert_eq!(case["status"], 0, "{name}");
            assert_eq!(ran.status.code(), Some(1), "{name}");
            assert_eq!((printed.as_str(), complained.as_str()), ("", refusal));
            tally[2] += 1;
        } else if case["status"] == 0 {
            let workers = case["stdout"].as_str().unwrap().strip_suffix('\n').unwrap();
            assert_eq!(ran.status.code(), Some(0), "{name}");
            let expected = format!("CodeQL traced compile workers: {workers}\n");
            assert_eq!((printed, complained.as_str()), (expected, ""), "{name}");
            tally[0] += 1;
        } else {
            let raised = "security_tools.ToolError: codeql_build_jobs_range";
            assert_eq!(case["raised"], raised, "{name}");
            assert_eq!(case["status"], 1, "{name}");
            assert_eq!(ran.status.code(), Some(1), "{name}");
            assert_eq!((printed.as_str(), complained.as_str()), ("", refusal));
            tally[1] += 1;
        }
    }
    assert_eq!(tally, [4, 16, 1]);
}

/// Every job installs the pinned toolchain and builds the compiled tooling
/// once, before anything uses it, where no Cargo setting of the workflow
/// moves it; every later step calls it by that absolute path, and nothing
/// runs Python or builds the gate again.
#[test]
fn every_codeql_job_builds_its_compiled_tooling_once_before_using_it() {
    let codeql = workflow("codeql.yml");
    let job = &codeql["jobs"][JOB];
    let steps = steps(&codeql, JOB);
    let toolchain = &steps[1];
    assert_eq!(toolchain["uses"], TOOLCHAIN_ACTION);
    assert_eq!(toolchain["with"], json!({"toolchain": "1.99.0"}));
    assert!(lacks(toolchain, &["if", "env", "run"]));
    let build = &steps[2];
    assert!(lacks(build, &["if", "uses", "continue-on-error"]));
    assert_eq!(
        build["env"],
        json!({"CODEQL_LANGUAGE": "${{ matrix.language }}"})
    );
    assert_eq!(run(build), BUILD);
    let mut levels = vec![&codeql["env"], &job["env"]];
    levels.extend(steps.iter().map(|step| &step["env"]));
    for level in levels {
        assert!(level.get("CARGO_BUILD_TARGET").is_none());
        assert!(level.get("CARGO_TARGET_DIR").is_none());
    }
    let text = checkout_file(".github/workflows/codeql.yml");
    assert!(!text.contains("python3"));
    assert!(!text.contains("build/security_"));
    assert_eq!(text.matches("cargo build").count(), 1);
    let (mut gates, mut tools) = (0, 0);
    for step in &steps[3..] {
        let script = script(step);
        let gate = script.matches("simplestchat-security").count();
        assert_eq!(script.matches(SECURITY).count(), gate, "{}", step["name"]);
        let tool = script.matches(CI_TOOL).count();
        let recorder = "cargo run --locked -q -p simplestchat-ci -- evidence record";
        let recorded = script.matches(recorder).count();
        assert_eq!(script.matches("simplestchat-ci").count(), tool + recorded);
        assert!(tool == 0 || step["if"] == "matrix.language == 'rust'" || step["id"] == JOB);
        gates += gate;
        tools += tool;
    }
    assert_eq!((gates, tools), (6, 2));
}

/// The Rust dependency build is keyed, and its publication bounded, by the
/// compiled gate: the key line goes to the step's output, and the bound's
/// lines to the log and to the output, a failure failing the step.
#[test]
fn the_rust_dependency_build_is_keyed_and_bounded_by_the_compiled_gate() {
    let codeql = workflow("codeql.yml");
    let steps = steps(&codeql, JOB);
    let key = first(steps, |step| step["id"] == "rust-cargo-inputs");
    assert_eq!(
        run(key),
        format!(
            concat!(
                "key=\"$({} codeql cargo-key",
                " --source-root \"$RUNNER_TEMP/codeql-local/source\")\"\n",
                "echo \"key=$key\" >> \"$GITHUB_OUTPUT\"\n"
            ),
            SECURITY
        )
    );
    let size = first(steps, |step| step["id"] == "rust-cargo-size");
    assert_eq!(
        run(size),
        format!(
            concat!(
                "set -euo pipefail\n",
                "{} codeql cargo-key --source-root \"$RUNNER_TEMP/codeql-local/source\" \\\n",
                "  --publication-check \"$RUNNER_TEMP/codeql-rust-cargo\"",
                " | tee -a \"$GITHUB_OUTPUT\"\n"
            ),
            SECURITY
        )
    );
}

/// From `test_codeql_excludes_vendor_for_every_automated_entrypoint`: called,
/// scheduled and dispatched analysis share the same first-party scope.
#[test]
fn codeql_excludes_vendor_for_every_automated_entrypoint() {
    let codeql = workflow("codeql.yml");
    assert_eq!(keys(&codeql["jobs"]), [JOB].into());
    let text = checkout_file(".github/workflows/codeql.yml");
    assert!(!text.contains("--include-vendor"));
    assert!(!text.contains("c-cpp"));
    let initialize = first(steps(&codeql, JOB), |step| {
        uses(step).contains("codeql-action/init@")
    });
    assert_eq!(
        initialize["with"]["config-file"],
        "security/codeql/first-party.yml"
    );
    let config = document("security/codeql/first-party.yml");
    assert_eq!(config["paths-ignore"], json!(["vendor/**"]));
}

/// From `test_codeql_cache_publication_preserves_policy_failure_and_rejects_stale_markers`:
/// the workflow's own wrapper runs with a failing analysis command and with
/// fresh or stale evidence. Only a fresh marker publishes the cache output,
/// and the analysis status is kept either way.
#[test]
fn codeql_cache_publication_preserves_policy_failure_and_rejects_stale_markers() {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let codeql = workflow("codeql.yml");
    let steps = steps(&codeql, JOB);
    let runner = first(steps, |step| step["id"] == JOB);
    let save = first(steps, |step| uses(step).starts_with("actions/cache/save@"));
    let condition = save["if"].as_str().unwrap();
    for text in [
        "always() && !cancelled()",
        "steps.source-analysis.outputs.database-cache-ready == 'true'",
        "outputs.cache-hit != 'true'",
    ] {
        assert!(condition.contains(text), "{text}");
    }
    for (language, fresh) in [("rust", false), ("rust", true), ("python", false)] {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "codeql-step-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("codeql-local")).unwrap();
        let marker = directory.join(format!("codeql-local/{language}-cache-ready.json"));
        std::fs::write(&marker, "stale").unwrap();
        // The built gate the step runs, faked: it logs its arguments, makes
        // the marker when the test says so, and fails as a failed policy does.
        let gate = directory.join("codeql-tooling/debug/simplestchat-security");
        std::fs::create_dir_all(gate.parent().unwrap()).unwrap();
        let fake = concat!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RUNNER_TEMP/arguments\"\n",
            "if [ \"$TEST_FRESH\" = true ]; then ",
            ": > \"$RUNNER_TEMP/codeql-local/$CODEQL_LANGUAGE-cache-ready.json\"; fi\n",
            "exit 7\n"
        );
        std::fs::write(&gate, fake).unwrap();
        std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = directory.join("outputs");
        let result = std::process::Command::new("/bin/bash")
            .args(["-e", "-c", run(runner)])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("RUNNER_TEMP", &directory)
            .env("GITHUB_OUTPUT", &output)
            .env("GITHUB_SHA", "a".repeat(40))
            .env("CODEQL_LANGUAGE", language)
            .env("CODEQL_SUITE", "security")
            .env("TEST_FRESH", fresh.to_string())
            .output()
            .unwrap();
        let published = std::fs::read_to_string(&output).unwrap_or_default();
        let kept = marker.exists();
        let place = directory.to_str().unwrap();
        let mut expected = vec![
            "codeql".to_owned(),
            "local".to_owned(),
            "--revision".to_owned(),
            "a".repeat(40),
            "--language".to_owned(),
            language.to_owned(),
            "--suite".to_owned(),
            "security".to_owned(),
            "--output".to_owned(),
            format!("{place}/codeql-local"),
        ];
        if language == "rust" {
            for (option, value) in [
                ("--database-cache", "codeql-rust-database"),
                ("--rust-cargo-cache", "codeql-rust-cargo"),
                ("--cache-tool", "codeql-tooling/debug/simplestchat-ci"),
            ] {
                expected.extend([option.to_owned(), format!("{place}/{value}")]);
            }
        }
        let arguments = std::fs::read_to_string(directory.join("arguments")).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(arguments.lines().collect::<Vec<_>>(), expected);
        assert_eq!(result.status.code(), Some(7), "fresh={fresh}");
        assert_eq!(kept, fresh);
        let expected = if fresh {
            "database-cache-ready=true\n"
        } else {
            ""
        };
        assert_eq!(published, expected, "fresh={fresh}");
    }
}

/// From `test_local_codeql_runs_full_pinned_analysis_before_recording_success`:
/// local success requires each real language policy, and hosted uploads stay
/// on GitHub.
#[test]
fn local_codeql_runs_full_pinned_analysis_before_recording_success() {
    let codeql = workflow("codeql.yml");
    assert_eq!(
        codeql["env"]["CODEQL_ACTION_DIFF_INFORMED_QUERIES"],
        "false"
    );
    assert_eq!(codeql["jobs"][JOB]["strategy"]["max-parallel"], 4);
    assert_eq!(keys(&codeql["jobs"]), [JOB].into());
    let steps = steps(&codeql, JOB);
    let (ran, local) = located(steps, |step| script(step).contains(" codeql local "));
    assert_eq!(local["if"], "${{ env.ACT || matrix.language == 'rust' }}");
    assert!(lacks(local, &["continue-on-error"]));
    let command = format!(
        concat!(
            "{} codeql local --revision \"$GITHUB_SHA\" \\\n",
            "  --language \"$CODEQL_LANGUAGE\" --suite \"$CODEQL_SUITE\"",
            " --output \"$RUNNER_TEMP/codeql-local\" \"${{cache[@]}}\" || status=$?\n"
        ),
        SECURITY
    );
    assert_eq!(run(local).matches(&command).count(), 1);
    let (recorded, recorder) = located(steps, |step| {
        script(step).contains("cargo run --locked -q -p simplestchat-ci -- evidence record")
    });
    assert_eq!(recorder["if"], "${{ env.ACT }}");
    assert!(ran < recorded);
    let hosted_pin = first(steps, |step| script(step).contains(" codeql tools "));
    assert_eq!(hosted_pin["if"], HOSTED);
    assert_eq!(
        run(hosted_pin),
        format!(
            concat!(
                "{} codeql tools --codeql \"$CODEQL_BINARY\" \\\n",
                "  --language \"$CODEQL_LANGUAGE\" --output \"$RUNNER_TEMP/codeql-tools\"\n"
            ),
            SECURITY
        )
    );
    for value in [&codeql, &workflow("security.yml")] {
        for (name, job) in value["jobs"].as_object().unwrap() {
            for step in job["steps"].as_array().unwrap() {
                if uses(step).contains("upload-artifact@") {
                    assert!(step["if"].as_str().unwrap().contains("!env.ACT"), "{name}");
                }
            }
        }
    }
}

/// From `test_codeql_success_requires_original_report_policy_enforcement`: an
/// analyzer success or remote dismissal cannot stand in for the local verdict.
#[test]
fn codeql_success_requires_original_report_policy_enforcement() {
    let codeql = workflow("codeql.yml");
    let steps = steps(&codeql, JOB);
    let (analyzed, analyze) = located(steps, |step| uses(step).contains("codeql-action/analyze@"));
    let (gated, gate) = located(steps, |step| script(step).contains(" codeql triage sarif "));
    assert!(analyzed < gated);
    assert_eq!(analyze["with"]["output"], "${{ runner.temp }}/codeql-sarif");
    assert_eq!(
        gate["if"],
        "${{ !env.ACT && matrix.language != 'rust' && inputs.security_gate }}"
    );
    assert!(lacks(gate, &["continue-on-error"]));
    assert!(run(gate).contains("${#reports[@]} != 1"));
    let command = format!(
        concat!(
            "{} codeql triage sarif --revision \"$GITHUB_SHA\" \\\n",
            "  --input \"${{reports[0]}}\" --output \"$RUNNER_TEMP/codeql-policy-evidence\"\n"
        ),
        SECURITY
    );
    assert!(run(gate).ends_with(&command));
    let uploads = steps
        .iter()
        .filter(|step| uses(step).contains("upload-artifact@"))
        .collect::<Vec<_>>();
    let [upload] = uploads[..] else {
        panic!("exactly one upload");
    };
    assert_eq!(
        upload["if"],
        "${{ always() && !env.ACT && inputs.security_gate }}"
    );
    assert_eq!(
        line_set(&upload["with"]["path"]),
        [
            "${{ runner.temp }}/codeql-policy-evidence/report.json",
            "${{ runner.temp }}/codeql-policy-evidence/failure.json",
            "${{ runner.temp }}/codeql-local/rust-security-policy/report.json",
            "${{ runner.temp }}/codeql-local/summary.json",
            "${{ runner.temp }}/codeql-local/rust-security.sarif",
            "${{ runner.temp }}/codeql-local/*-codeql-analyze-rust-security.stderr",
        ]
        .into()
    );
}

/// From `test_rust_uses_exact_database_cache_and_current_upload_categories`:
/// Rust alone leaves the source matrix's action analyzer for the shared
/// pinned runner.
#[test]
fn rust_uses_exact_database_cache_and_current_upload_categories() {
    let codeql = workflow("codeql.yml");
    let steps = steps(&codeql, JOB);
    let restore = first(steps, |step| step["id"] == "rust-database");
    assert_eq!(restore["if"], "matrix.language == 'rust'");
    assert!(restore["with"].is_object());
    assert!(lacks(&restore["with"], &["restore-keys"]));
    let validator = first(steps, |step| step["id"] == "rust-reports");
    assert_eq!(
        validator["if"],
        "${{ always() && !env.ACT && matrix.language == 'rust' }}"
    );
    assert_eq!(
        run(validator),
        format!(
            concat!(
                "{} codeql upload-ready --output \"$RUNNER_TEMP/codeql-local\" \\\n",
                "  --revision \"$GITHUB_SHA\" --language rust >> \"$GITHUB_OUTPUT\"\n"
            ),
            SECURITY
        )
    );
    let uploads = steps
        .iter()
        .filter(|step| uses(step).contains("codeql-action/upload-sarif@"))
        .collect::<Vec<_>>();
    let categories = uploads.iter().map(|step| &step["with"]["category"]);
    assert_eq!(
        categories
            .map(|category| category.as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        ["/language:rust/security", "/language:rust/quality-advisory"].into()
    );
    for upload in uploads {
        let guard = upload["if"].as_str().unwrap();
        assert!(guard.contains("always() && !env.ACT && matrix.language == 'rust'"));
        assert_eq!(upload["with"]["wait-for-processing"], true);
        assert!(lacks(upload, &["continue-on-error"]));
    }
}
