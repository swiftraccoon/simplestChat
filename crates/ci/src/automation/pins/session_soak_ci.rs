//! The scheduled session soak job, carried over from the Python workflow
//! tests.

use super::{first, run, steps, workflow};
use serde_json::Value;

/// The one supervised full-profile workload step.
const NAME: &str = "Soak twenty minutes and stop PostgreSQL for eight seconds at the midpoint";

/// The scheduled workload job.
fn job(performance: &Value) -> &Value {
    let job = &performance["jobs"]["soak"];
    assert!(job.is_object());
    job
}

/// The first step with `name`, and its position.
fn named<'a>(steps: &'a [Value], name: &str) -> (usize, &'a Value) {
    steps
        .iter()
        .enumerate()
        .find(|(_, step)| step["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// From `test_same_hosted_job_deadlines_and_pinned_database_are_retained`:
/// the port changes orchestration without reducing the public workload.
#[test]
fn same_hosted_job_deadlines_and_pinned_database_are_retained() {
    let performance = workflow("performance.yml");
    let job = job(&performance);
    let (_, step) = named(steps(&performance, "soak"), NAME);
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 60);
    assert_eq!(step["timeout-minutes"], 40);
    assert_eq!(
        job["services"]["postgres"]["image"],
        concat!(
            "docker.io/library/postgres:18.6-bookworm@sha256:",
            "3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650"
        )
    );
    let step = step.as_object().unwrap();
    assert!(!step.contains_key("continue-on-error"));
    assert!(!step.contains_key("if"));
}

/// From `test_explicit_service_context_and_captured_database_target_replace_port_discovery`:
/// only the runner-selected PostgreSQL service can receive stop and start.
#[test]
fn explicit_service_context_and_captured_database_target_replace_port_discovery() {
    let performance = workflow("performance.yml");
    let job = job(&performance);
    let (_, step) = named(steps(&performance, "soak"), NAME);
    for (key, value) in [
        ("SOAK_POSTGRES_ID", "${{ job.services.postgres.id }}"),
        (
            "SOAK_POSTGRES_NETWORK",
            "${{ job.services.postgres.network }}",
        ),
        (
            "SOAK_POSTGRES_PORT",
            "${{ job.services.postgres.ports['5432'] }}",
        ),
        ("SESSION_SOAK_E2E", "1"),
        ("E2E_ARTIFACTS", "${{ runner.temp }}/session-soak"),
        ("SERVER_REVISION", "${{ github.sha }}"),
        ("FRONTEND_REVISION", "${{ github.sha }}"),
    ] {
        assert_eq!(step["env"][key], value, "{key}");
    }
    let database = concat!(
        "postgres://test_owner:disposable-ci-password@127.0.0.1:5432/",
        "simplestchat_test?sslmode=disable"
    );
    let environment = step["env"].as_object().unwrap();
    for key in ["DATABASE_URL", "TEST_DATABASE_URL"] {
        assert_eq!(job["env"][key], database, "{key}");
        assert!(!environment.contains_key(key), "{key}");
    }
}

/// From `test_the_existing_migration_build_browser_and_artifact_boundaries_stay_ordered`:
/// the supervisor consumes built assets and does not create a second workload.
#[test]
fn the_existing_migration_build_browser_and_artifact_boundaries_stay_ordered() {
    let performance = workflow("performance.yml");
    let steps = steps(&performance, "soak");
    let (position, _) = named(steps, NAME);
    for name in [
        "Build server and load generator",
        "Build UI and install pinned browser tooling",
        "Migrate disposable test database with the real server",
    ] {
        assert!(named(steps, name).0 < position, "{name}");
    }
    let (retained, retention) = named(steps, "Preserve soak diagnostics");
    assert!(retained > position);
    assert_eq!(retention["with"]["path"], "${{ runner.temp }}/session-soak");
}

/// From `test_explicit_service_context_and_captured_database_target_replace_port_discovery`:
/// the soak job's named step runs exactly the CI binary's soak.
#[test]
fn the_soak_step_runs_the_ci_binary_soak() {
    let performance = workflow("performance.yml");
    let step = first(steps(&performance, "soak"), |step| step["name"] == NAME);
    assert_eq!(
        run(step),
        "cargo run --locked -q -p simplestchat-ci -- session-soak"
    );
}
