//! Current counterparts of three unchanged Python workflow-reference methods.
//! These source contracts run no browser, compiler, workflow or Python loader.
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

type Check<T> = Result<T, &'static str>;
const BUILD: &str = "cargo build --locked -p simplestchat -p simplestchat-load-test --bins";
const RESTART: &str = concat!(
    "./target/debug/load_test bench restart-browser-smoke --binary \"$PWD/target/debug/simplestChat\" \\\n",
    "  --browser chromium --output \"${RUNNER_TEMP}/restart-browser\"\n",
);
const GUARD: &str = "steps.verified.outputs.cache-hit != 'true'";
#[derive(Deserialize)]
struct Workflow {
    jobs: BTreeMap<String, Value>,
}
fn source() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".github/workflows/ci.yml"),
    )
    .unwrap()
}
fn parsed() -> Workflow {
    serde_saphyr::from_str(&source()).unwrap()
}
fn require(ok: bool, cause: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(cause) }
}
fn named<'a>(steps: &'a [Value], name: &str) -> Check<(usize, &'a Value)> {
    let found = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step["name"] == name)
        .collect::<Vec<_>>();
    require(found.len() == 1, "unique-step")?;
    Ok(found[0])
}
fn toolchain(steps: &[Value]) -> Check<&Value> {
    let found = steps
        .iter()
        .filter(|step| step["uses"] == "./.github/actions/native-toolchain")
        .collect::<Vec<_>>();
    require(found.len() == 1, "unique-toolchain")?;
    Ok(found[0])
}
fn restart(steps: &[Value]) -> Check<()> {
    let (at, step) = named(
        steps,
        "Automatic room recovery after an owned server restart",
    )?;
    require(step["if"] == "matrix.group == 'stress'", "stress-only")?;
    require(step["timeout-minutes"] == 3, "three-minute-owner")?;
    require(step["run"] == RESTART, "restart-argv")?;
    require(
        step.get("env").is_none() && step.get("continue-on-error").is_none(),
        "restart-authority",
    )?;
    let (before, build) = named(steps, "Build server and load generator")?;
    require(build["run"] == BUILD && before < at, "prebuilt-app-load")?;
    let (_, upload) = named(steps, "Preserve browser restart diagnostics")?;
    require(
        upload["if"] == "${{ !cancelled() && matrix.group == 'stress' && !env.ACT }}",
        "upload-guard",
    )?;
    require(
        upload["with"]["path"] == "${{ runner.temp }}/restart-browser",
        "upload-path",
    )?;
    require(upload["with"]["retention-days"] == 7, "upload-retention")
}
const SKIPS: [&str; 14] = [
    "capacity_vps::process::tests::inert_child_signal_mask",
    "capacity_vps::host_tests::whole_host_policy_matches_original_python_oracle",
    concat!(
        "ci::ops_artifact::capacity_vps::controller_tests::",
        "all_eight_whole_reference_scenarios_preserve_local_policy_and_explicit_transport_deltas"
    ),
    "release::build::tests::whole_reference_scenarios_match_unchanged_python",
    "release::build::process::tests::signal_worker",
    "release::attestation::prepare::oracle_tests::whole_prepare_scenarios_match_unchanged_python",
    "release::attestation::prepare::signal_tests::retained_hash_signal_child",
    "whole_verify_scenarios_match_unchanged_python",
    "whole_fresh_release_matches_unchanged_python",
    "whole_fetch_controller_matches_unchanged_python",
    "whole_host_receiver_matches_unchanged_python",
    "whole_stage_matches_unchanged_python",
    "whole_deploy_matches_unchanged_python",
    "whole_restore_matches_unchanged_python",
];
fn complete_graph(jobs: &BTreeMap<String, Value>) -> Check<()> {
    for (name, role) in [
        ("rust-test", "rust-test"),
        ("browser", "browser-backend-v1"),
    ] {
        let steps = jobs
            .get(name)
            .and_then(|job| job["steps"].as_array())
            .ok_or("steps")?;
        require(
            toolchain(steps)?["with"]["cache-key"] == role,
            "dependency-role",
        )?;
        let (before, build) = steps
            .iter()
            .enumerate()
            .find(|(_, step)| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("cargo build "))
            })
            .ok_or("build")?;
        if name == "browser" {
            require(build["run"] == BUILD, "app-full-load-defaults")?;
            continue;
        }
        require(
            build["run"]
                == "cargo build --locked --workspace --all-targets --all-features --profile test",
            "complete-test-graph",
        )?;
        let (at, execute) = steps
            .iter()
            .enumerate()
            .find(|(_, step)| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("cargo test "))
            })
            .ok_or("execute")?;
        let mut expected = vec![
            "cargo",
            "test",
            "--locked",
            "--workspace",
            "--all-features",
            "--",
            "--include-ignored",
            "--test-threads=1",
        ];
        for skip in SKIPS {
            expected.extend(["--skip", skip]);
        }
        require(
            execute["run"]
                .as_str()
                .ok_or("test-argv")?
                .split_whitespace()
                .collect::<Vec<_>>()
                == expected,
            "every-test-and-reference-skip",
        )?;
        require(
            before < at && build["if"] == execute["if"] && build["if"] == GUARD,
            "ordered-equal-build-test-guards",
        )?;
    }
    Ok(())
}
#[test]
fn current_restart_and_complete_graph_preserve_all_frozen_method_obligations() {
    let workflow = parsed();
    restart(workflow.jobs["browser"]["steps"].as_array().unwrap()).unwrap();
    complete_graph(&workflow.jobs).unwrap();
}
#[test]
fn restart_contract_refuses_authority_argv_order_and_retention_neighbors() {
    let steps = parsed().jobs["browser"]["steps"]
        .as_array()
        .unwrap()
        .clone();
    for (name, field, value) in [
        (
            "Automatic room recovery after an owned server restart",
            "if",
            json!(true),
        ),
        (
            "Automatic room recovery after an owned server restart",
            "timeout-minutes",
            json!(4),
        ),
        (
            "Automatic room recovery after an owned server restart",
            "run",
            json!("another owner"),
        ),
        (
            "Automatic room recovery after an owned server restart",
            "env",
            json!({}),
        ),
        (
            "Automatic room recovery after an owned server restart",
            "continue-on-error",
            json!(false),
        ),
        (
            "Build server and load generator",
            "run",
            json!("cargo build --locked --workspace --bins"),
        ),
        (
            "Preserve browser restart diagnostics",
            "if",
            json!("always()"),
        ),
    ] {
        let mut changed = steps.clone();
        let at = named(&changed, name).unwrap().0;
        changed[at][field] = value;
        assert!(restart(&changed).is_err(), "{name}: {field}");
    }
    for (field, value) in [
        ("path", json!("unbounded-output")),
        ("retention-days", json!(8)),
    ] {
        let mut changed = steps.clone();
        let at = named(&changed, "Preserve browser restart diagnostics")
            .unwrap()
            .0;
        changed[at]["with"][field] = value;
        assert!(restart(&changed).is_err());
    }
    let mut changed = steps;
    let build = named(&changed, "Build server and load generator")
        .unwrap()
        .0;
    let run = named(
        &changed,
        "Automatic room recovery after an owned server restart",
    )
    .unwrap()
    .0;
    changed.swap(build, run);
    assert!(restart(&changed).is_err());
}
#[test]
fn graph_contract_refuses_old_browser_role_incomplete_tests_and_guard_changes() {
    for (job, field, value) in [
        ("browser", "cache-key", "rust-build"),
        ("rust-test", "cache-key", "browser-backend-v1"),
    ] {
        let mut jobs = parsed().jobs;
        let steps = jobs[job]["steps"].as_array().unwrap();
        let at = steps
            .iter()
            .position(|step| step["uses"] == "./.github/actions/native-toolchain")
            .unwrap();
        jobs.get_mut(job).unwrap()["steps"][at]["with"][field] = value.into();
        assert!(complete_graph(&jobs).is_err());
    }
    for job in ["browser", "rust-test"] {
        let mut jobs = parsed().jobs;
        let steps = jobs[job]["steps"].as_array().unwrap();
        let at = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("cargo build "))
            })
            .unwrap();
        jobs.get_mut(job).unwrap()["steps"][at]["run"] = "cargo build --locked --bins".into();
        assert!(complete_graph(&jobs).is_err());
    }
    for suffix in [" --skip unknown-test", " --test-threads=2"] {
        let mut jobs = parsed().jobs;
        let at = jobs["rust-test"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("cargo test "))
            })
            .unwrap();
        let run = format!(
            "{}{suffix}",
            jobs["rust-test"]["steps"][at]["run"].as_str().unwrap()
        );
        jobs.get_mut("rust-test").unwrap()["steps"][at]["run"] = run.into();
        assert!(complete_graph(&jobs).is_err());
    }
    let mut jobs = parsed().jobs;
    let at = jobs["rust-test"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .position(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.starts_with("cargo test "))
        })
        .unwrap();
    jobs.get_mut("rust-test").unwrap()["steps"][at]["if"] = true.into();
    assert!(complete_graph(&jobs).is_err());
    let mut jobs = parsed().jobs;
    let steps = jobs.get_mut("rust-test").unwrap()["steps"]
        .as_array_mut()
        .unwrap();
    let build = steps
        .iter()
        .position(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.starts_with("cargo build "))
        })
        .unwrap();
    let execute = steps
        .iter()
        .position(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.starts_with("cargo test "))
        })
        .unwrap();
    steps.swap(build, execute);
    assert!(complete_graph(&jobs).is_err());
    let mut jobs = parsed().jobs;
    let run = jobs["rust-test"]["steps"][at]["run"]
        .as_str()
        .unwrap()
        .replace("--skip whole_restore_matches_unchanged_python", "");
    jobs.get_mut("rust-test").unwrap()["steps"][at]["run"] = run.into();
    assert!(complete_graph(&jobs).is_err());
}

#[path = "workflow_projection_tests.rs"]
mod projection;
