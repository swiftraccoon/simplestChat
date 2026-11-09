//! Assertions moved from `ops/ansible/tests/test_release_container_ci.py`.
//! Each step is the deployment job's only step with its name.

use super::{run, steps, unique, workflow};

fn deployment_run(name: &str) -> String {
    run(unique(steps(&workflow("ci.yml"), "deployment"), name)).to_owned()
}

/// From `test_existing_image_is_built_and_smoked_once_before_release_integration`
/// and `test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export`,
/// which both pinned the build command.
#[test]
fn the_production_image_is_built_through_the_ci_binary() {
    assert_eq!(
        deployment_run("Build production container"),
        "cargo run --locked -q -p simplestchat-ci -- production-image build"
    );
}

/// From `test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export`.
#[test]
fn the_production_image_is_verified_and_smoked_through_the_ci_binary() {
    assert_eq!(
        deployment_run("Verify production image contents and user"),
        "cargo run --locked -q -p simplestchat-ci -- production-image verify"
    );
    assert_eq!(
        deployment_run("Production startup, migration and database-backed API smoke"),
        "cargo run --locked -q -p simplestchat-ci -- container-smoke"
    );
}

/// From `test_optional_http_owner_is_built_once_with_explicit_system_tls_and_separate_target`.
#[test]
fn the_release_http_policy_binary_is_the_feature_enabled_ci_build() {
    assert!(
        deployment_run("Verify the guest checker against pinned Compose JSON")
            .contains("cargo build --locked -p simplestchat-ci --features release-http-policy")
    );
}

/// From `test_local_failed_fixture_uses_the_compiled_private_evidence_policy`.
#[test]
fn local_release_container_diagnostics_copy_through_the_ci_binary() {
    assert_eq!(
        deployment_run("Preserve private local release-container diagnostics"),
        "cargo run --locked -q -p simplestchat-ci -- copy-evidence release-container"
    );
}
