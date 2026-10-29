//! Exact method routing for three unchanged, byte-pinned Python modules.
//! Only four historical workflow assertions move to the fixed YAML reference.

use std::ffi::OsString;

pub(in super::super) const QUALITY_BYTES: usize = 16299;
pub(in super::super) const QUALITY_SHA256: &str =
    "71d11f0964026a63f32de99f96bf642e4b3f6b14d566c91e0e249db64474332b";
pub(in super::super) const VERIFIED_BYTES: usize = 62348;
pub(in super::super) const VERIFIED_SHA256: &str =
    "b70f5f88b3a059d8571d6bdfe9dc5ffd59df8e57eb692ec52e2d4e7c1d80ec33";
pub(in super::super) const SUPPORT_BYTES: usize = 2574;
pub(in super::super) const SUPPORT_SHA256: &str =
    "ae56c5e0254594f61c0ff242c5bb55d642f36d3c9b9bb4ddf366a00d56e10ce7";
pub(in super::super) const CONTAINER_BYTES: usize = 26087;
pub(in super::super) const CONTAINER_SHA256: &str =
    "0e3d83ce7814a33c9bf2fc8ac9c41b821aca488ffdf7c726929fd685a87b221a";
pub(in super::super) const WORKFLOW_BYTES: usize = 73398;
pub(in super::super) const WORKFLOW_SHA256: &str =
    "fb9b55ab8772bcbb1d23553f66dc4767ac4b1e7a65eb0595c1bb961a132b3aed";

pub(super) const MODULES: [&str; 3] = [
    "test_python_quality",
    "test_ci_verified",
    "test_release_container_ci",
];
pub(super) const LEGACY: [&str; 4] = [
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_restart_browser_keeps_the_three_minute_owner_and_guarded_evidence",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_workflow_projection_preserves_the_actual_build_contract",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_rust_tests_prime_their_own_complete_graph_before_executing_every_test",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_only_sanitized_report_is_retained_even_on_failure",
    ),
];
const METHODS: [&str; 62] = [
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_restart_browser_keeps_the_three_minute_owner_and_guarded_evidence",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_guest_smokes_use_the_prebuilt_owner_after_the_guarded_workspace_build",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_configuration_metadata_retention_never_uploads_command_logs",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_offline_configuration_validators_keep_existing_jobs_guards_and_fixed_inputs",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_branch_subject_checks_the_head_before_any_cached_gate",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_configuration_has_full_rules_and_no_machine_specific_environment",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_checker_requirements_pin_versions_and_share_controller_dependencies",
    ),
    concat!(
        "test_python_quality.PythonPolicyTests.",
        "test_ci_installs_and_runs_the_same_gate_before_offline_tests",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_frontend_code_reuses_backend_but_shared_json_fixture_invalidates_it",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_frontend_helper_tests_and_guides_reuse_existing_rust_receipts",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_rust_receipts_keep_sources_shared_data_and_runtime_inputs_bound",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_changed_source_policy_workflow_mode_and_deleted_inputs_invalidate",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_security_binds_each_helper_and_ignores_unrelated_review_changes",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_security_closure_covers_all_local_python_imports",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_codeql_review_data_changes_do_not_invalidate_evaluated_databases",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_codeql_binds_its_actual_build_analysis_and_policy_inputs",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_codeql_closure_covers_all_local_python_imports",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_rust_analysis_keeps_all_targets_embedded_data_and_tool_inputs",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_rust_analysis_ignores_unrelated_docs_deployment_and_frontend_tooling",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_native_codeql_ignores_unrelated_ci_tools_but_binds_native_data_and_modes",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_runner_architecture_trust_and_hosted_image_generation_are_bound",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_receipt_reuse_names_original_revision_and_refuses_stale_inputs",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_rust_reference_requirements_invalidate_original_success",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_cache_checks_every_binary_before_installing_any",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_embedded_ops_catalog_changes_every_compiled_tool_key_and_original_receipt",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_closure_preserves_build_inputs_without_operational_invalidations",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_workflow_projection_preserves_the_actual_build_contract",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_setup_defaults_stay_stable_and_compiler_overrides_invalidate",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_dependency_guard_rejects_unbound_sources_before_publication",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_dependency_guard_binds_generated_inputs_and_depinfo_bytes",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_backend_dependency_guard_rejects_wrong_targets_and_unsupported_rules",
    ),
    concat!(
        "test_ci_verified.VerifiedCacheTests.",
        "test_codeql_cannot_use_a_success_stamp_instead_of_a_traced_database",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_selected_python_installs_the_reviewed_wheel_lock_before_fingerprinting",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_rollover_browser_acquisition_has_ten_minutes_without_weakening_runtime",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_reference_rust_bootstrap_installs_every_declared_component_before_cargo",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_parallel_automation_keeps_every_reference_required_without_shared_receipts",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_rust_unicode_oracle_is_pinned_before_fingerprints_cache_and_builds",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_rust_native_consumers_override_the_python_pkg_config_export",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_native_success_receipts_do_not_enable_automated_vendor_scans",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_rust_tests_prime_their_own_complete_graph_before_executing_every_test",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_audits_helpers_and_browser_suites_are_not_replaced_by_cached_success",
    ),
    concat!(
        "test_ci_verified.VerifiedWorkflowTests.",
        "test_cache_receipts_are_verified_and_saved_only_on_success_without_prefix_fallback",
    ),
    concat!(
        "test_ci_verified.RestoreCiIntegrationTests.",
        "test_restore_inputs_invalidate_existing_checks_and_rust_receipts_without_git",
    ),
    concat!(
        "test_ci_verified.RestoreCiIntegrationTests.",
        "test_restore_has_one_exact_host_owner_and_an_explicit_broad_skip",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_fresh_host_and_job_deadline_include_the_existing_build",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_existing_image_is_built_and_smoked_once_before_release_integration",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_image_policy_has_pinned_rust_on_hosted_and_disposable_ci",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_push_export_never_rebuilds_and_publication_waits_for_required_checks",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_compose_is_verified_before_install_and_matches_both_execution_users",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_guest_compose_contract_runs_with_required_pinned_renderer",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_optional_http_owner_is_built_once_with_explicit_system_tls_and_separate_target",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_dependencies_use_existing_pinned_controller_requirements_in_an_isolated_venv",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_controller_setup_follows_validation_and_still_overlaps_image_work",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_root_disposable_opt_in_is_explicit_bounded_and_preserves_failure",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_pinned_rollover_browser_runs_after_image_build_without_an_extra_app_build",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_only_sanitized_report_is_retained_even_on_failure",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_completed_layers_are_saved_before_later_checks_can_fail",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_local_failed_fixture_uses_the_compiled_private_evidence_policy",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_pinned_compiler_precedes_image_policy_and_all_guarded_local_evidence",
    ),
    concat!(
        "test_release_container_ci.ReleaseContainerCiTests.",
        "test_read_only_permissions_pinned_actions_and_no_remote_deployment",
    ),
];

pub(super) fn select(module: &str, arguments: &mut Vec<OsString>) {
    for method in METHODS {
        if method.split('.').next() == Some(module) && !LEGACY.contains(&method) {
            arguments.extend(["-k".into(), method.into()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_method_roster_routes_every_original_test_once() {
        let sources = [
            (
                MODULES[0],
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_python_quality.py"
                )),
                QUALITY_BYTES,
                QUALITY_SHA256,
            ),
            (
                MODULES[1],
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_ci_verified.py"
                )),
                VERIFIED_BYTES,
                VERIFIED_SHA256,
            ),
            (
                MODULES[2],
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_release_container_ci.py"
                )),
                CONTAINER_BYTES,
                CONTAINER_SHA256,
            ),
        ];
        let mut observed = Vec::new();
        for (module, source, bytes, sha) in sources {
            assert!(super::super::source_matches(source.as_bytes(), bytes, sha));
            let mut class = None;
            for line in source.lines() {
                if let Some(name) = line.strip_prefix("class ") {
                    class = name.split_once('(').map(|(name, _)| name);
                }
                if let Some(method) = line.strip_prefix("    def test_") {
                    let method = method.split_once('(').unwrap().0;
                    observed.push(format!("{module}.{}.test_{method}", class.unwrap()));
                }
            }
            let mut selectors = Vec::new();
            select(module, &mut selectors);
            for method in METHODS
                .iter()
                .filter(|name| name.starts_with(&format!("{module}.")))
            {
                let current = selectors
                    .windows(2)
                    .filter(|pair| pair[0] == "-k" && pair[1] == *method)
                    .count();
                assert_eq!(
                    current + usize::from(LEGACY.contains(method)),
                    1,
                    "{method}"
                );
            }
        }
        assert_eq!(observed, METHODS);
        assert_eq!(
            METHODS.len(),
            METHODS
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        assert!(LEGACY.iter().all(|method| METHODS.contains(method)));
        let groups = [
            vec!["test_release_playbook".into(), MODULES[0].into()],
            vec![MODULES[1].into(), MODULES[2].into(), "test_other".into()],
        ];
        for database in [false, true] {
            let jobs = super::super::current_arguments(&groups, database).unwrap();
            let selectors = jobs
                .iter()
                .flat_map(|(_, arguments)| arguments.windows(2))
                .filter(|pair| pair[0] == "-k")
                .map(|pair| pair[1].to_str().unwrap())
                .collect::<Vec<_>>();
            let legacy = super::super::legacy_arguments();
            for method in METHODS {
                let current = selectors
                    .iter()
                    .filter(|selector| {
                        selector
                            .strip_suffix('*')
                            .map_or(method == **selector, |prefix| method.starts_with(prefix))
                    })
                    .count();
                let historical = legacy
                    .iter()
                    .filter(|argument| argument.to_str() == Some(method))
                    .count();
                assert_eq!(current + historical, 1, "{database}: {method}");
            }
        }
    }
}
