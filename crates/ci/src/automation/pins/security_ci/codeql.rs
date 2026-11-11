//! The CodeQL workflow: called security analysis and standalone advisory runs.

use super::super::{
    checkout_file, document, first, lacks, located, run, script, steps, uses, workflow,
};
use super::keys;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

const JOB: &str = "source-analysis";
const HOSTED: &str = "${{ !env.ACT && matrix.language != 'rust' }}";

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
        "build/security_codeql_resources.py",
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
    let at = |text: &str| command.find(text).unwrap();
    assert!(at(setup) < at(generator));
    assert!(at(generator) < at(worker));
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
    for fresh in [false, true] {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "codeql-step-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("codeql-local")).unwrap();
        let marker = directory.join("codeql-local/rust-cache-ready.json");
        std::fs::write(&marker, "stale").unwrap();
        let output = directory.join("outputs");
        let script = format!(
            concat!(
                "python3() {{ if [[ \"$TEST_FRESH\" == true ]]; then ",
                ": > \"$RUNNER_TEMP/codeql-local/$CODEQL_LANGUAGE-cache-ready.json\"; ",
                "fi; return 7; }}\n{}"
            ),
            run(runner)
        );
        let result = std::process::Command::new("/bin/bash")
            .args(["-e", "-c", &script])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("RUNNER_TEMP", &directory)
            .env("GITHUB_OUTPUT", &output)
            .env("GITHUB_SHA", "a".repeat(40))
            .env("CODEQL_LANGUAGE", "rust")
            .env("CODEQL_SUITE", "security")
            .env("OPENSSL_DIR", directory.join("openssl"))
            .env("TEST_FRESH", fresh.to_string())
            .output()
            .unwrap();
        let published = std::fs::read_to_string(&output).unwrap_or_default();
        let kept = marker.exists();
        std::fs::remove_dir_all(&directory).unwrap();
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
    let (ran, local) = located(steps, |step| {
        script(step).contains("build/security_codeql_local.py")
    });
    assert_eq!(local["if"], "${{ env.ACT || matrix.language == 'rust' }}");
    assert!(lacks(local, &["continue-on-error"]));
    for text in [
        "--revision \"$GITHUB_SHA\"",
        "--suite \"$CODEQL_SUITE\"",
        "--language \"$CODEQL_LANGUAGE\"",
    ] {
        assert!(run(local).contains(text), "{text}");
    }
    let (recorded, recorder) = located(steps, |step| {
        script(step).contains("cargo run --locked -q -p simplestchat-ci -- evidence record")
    });
    assert_eq!(recorder["if"], "${{ env.ACT }}");
    assert!(ran < recorded);
    let hosted_pin = first(steps, |step| {
        script(step).contains("build/security_codeql_tools.py")
    });
    assert_eq!(hosted_pin["if"], HOSTED);
    assert!(run(hosted_pin).contains("--codeql \"$CODEQL_BINARY\""));
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
    let (gated, gate) = located(steps, |step| {
        script(step).contains("build/security_codeql_triage.py sarif")
    });
    assert!(analyzed < gated);
    assert_eq!(analyze["with"]["output"], "${{ runner.temp }}/codeql-sarif");
    assert_eq!(
        gate["if"],
        "${{ !env.ACT && matrix.language != 'rust' && inputs.security_gate }}"
    );
    assert!(lacks(gate, &["continue-on-error"]));
    assert!(run(gate).contains("${#reports[@]} != 1"));
    assert!(run(gate).contains("--revision \"$GITHUB_SHA\""));
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
    assert!(
        run(validator).contains("upload_ready(output, os.environ['GITHUB_SHA'], category, 'rust')")
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
