//! Fixed class and exact method routing within the existing regression owner.
//! No custom Python loader or independent child owner is introduced.
use super::super::Role;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, ffi::OsString, path::Path};
#[path = "workflow_routing.rs"]
mod workflow_routing;
pub(super) use workflow_routing::{
    CONTAINER_BYTES, CONTAINER_SHA256, QUALITY_BYTES, QUALITY_SHA256, SUPPORT_BYTES,
    SUPPORT_SHA256, VERIFIED_BYTES, VERIFIED_SHA256, WORKFLOW_BYTES, WORKFLOW_SHA256,
};

const MODULE: &str = "test_release_playbook";
const CURRENT_CLASSES: [&str; 2] = ["ReleasePlaybookTests", "PublicPlaybookSelectionTests"];
pub(super) const FROZEN_MODULE_BYTES: usize = 30653;
pub(super) const FROZEN_MODULE_SHA256: &str =
    "da212cc2a423f6b5c8026f76ce52dc5057434c1bc0af989fdb1a458166799272";
pub(super) const OLD_PLAYBOOK_BYTES: usize = 5329;
pub(super) const OLD_PLAYBOOK_SHA256: &str =
    "a4d4b97b086c90960436d23ff4c9fa167ada31dee4a41672508d8d27e7e15b34";
pub(super) const OLD_SOURCE_BYTES: usize = 585;
pub(super) const OLD_SOURCE_SHA256: &str =
    "0f09ca96c071cce2d0ae04fcd845888fdf4181f39ba95968499ab70ee7e89c76";

pub(super) fn source_matches(bytes: &[u8], length: usize, expected: &str) -> bool {
    bytes.len() == length
        && Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == expected
}
fn select(module: &str, arguments: &mut Vec<OsString>) {
    if module == MODULE {
        for class in CURRENT_CLASSES {
            arguments.extend(["-k".into(), format!("{MODULE}.{class}.*").into()]);
        }
    } else if workflow_routing::MODULES.contains(&module) {
        workflow_routing::select(module, arguments);
    } else {
        arguments.extend(["-k".into(), format!("{module}.*").into()]);
    }
}

/// The existing admitted two groups remain the authoritative module roster.
/// Pin all four specially routed modules BEFORE calling this function.
pub(in super::super) fn current_arguments(
    groups: &[Vec<String>; 2],
    database: bool,
) -> Result<Vec<(Role, Vec<OsString>)>, ()> {
    let modules = groups.iter().flatten().collect::<Vec<_>>();
    let unique = modules.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != modules.len()
        || [MODULE]
            .into_iter()
            .chain(workflow_routing::MODULES)
            .any(|expected| {
                modules
                    .iter()
                    .filter(|module| module.as_str() == expected)
                    .count()
                    != 1
            })
    {
        return Err(());
    }
    let base = [
        "-m",
        "unittest",
        "discover",
        "-s",
        "ops/ansible/tests",
        "-v",
    ];
    let mut jobs = Vec::new();
    for (ordinal, group) in groups.iter().enumerate().take(if database { 1 } else { 2 }) {
        let mut arguments = base.iter().map(OsString::from).collect::<Vec<_>>();
        if database {
            // Do not retain the old unfiltered DB discovery: it would execute
            // MaintenancePlaybookTests against the new caller as well.
            for module in &unique {
                select(module, &mut arguments);
            }
        } else {
            for module in group {
                select(module, &mut arguments);
            }
        }
        jobs.push((
            if ordinal == 0 {
                Role::CurrentFirst
            } else {
                Role::CurrentSecond
            },
            arguments,
        ));
    }
    Ok(jobs)
}
pub(in super::super) fn legacy_arguments() -> Vec<OsString> {
    // Import through stdlib unittest. Executing the file directly would reach
    // its __main__ block BEFORE MaintenancePlaybookTests is defined.
    let mut arguments: Vec<OsString> = [
        "-B",
        "-m",
        "unittest",
        "-v",
        "test_release_playbook.MaintenancePlaybookTests",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    arguments.extend(workflow_routing::LEGACY.into_iter().map(OsString::from));
    arguments
}
pub(in super::super) fn legacy_pythonpath(
    private_root: &Path,
    original_root: &Path,
    inherited: Option<&std::ffi::OsStr>,
) -> Result<OsString, std::env::JoinPathsError> {
    let mut paths = vec![
        private_root.join("ops/ansible/tests"),
        original_root.join("ops/ansible/tests"),
    ];
    paths.extend(
        [
            "build",
            "ops/ansible/files",
            "ops/ansible/callback_plugins",
            "ops/ansible/filter_plugins",
        ]
        .map(|name| original_root.join(name)),
    );
    if let Some(inherited) = inherited {
        paths.extend(std::env::split_paths(inherited));
    }
    std::env::join_paths(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDS: [&str; 12] = [
        "ReleasePlaybookTests.test_only_current_signed_github_artifacts_can_select_a_release",
        concat!(
            "ReleasePlaybookTests.",
            "test_github_source_requires_exact_identities_and_",
            "explicit_supported_ssh",
        ),
        concat!(
            "ReleasePlaybookTests.",
            "test_github_fetch_is_bounded_controller_only_and_",
            "precedes_common_stage",
        ),
        "ReleasePlaybookTests.test_stage_is_default_and_deployment_is_an_explicit_separate_job",
        concat!(
            "ReleasePlaybookTests.",
            "test_routine_release_has_no_host_maintenance_or_",
            "configuration_rendering",
        ),
        concat!(
            "ReleasePlaybookTests.",
            "test_attestation_precedes_every_remote_preflight_and_",
            "cached_bytes_are_reverified",
        ),
        "ReleasePlaybookTests.test_python_helper_and_shared_module_are_installed_together",
        concat!(
            "ReleasePlaybookTests.",
            "test_prepared_mode_skips_helper_writes_but_keeps_local_",
            "release_storage",
        ),
        concat!(
            "ReleasePlaybookTests.",
            "test_real_ansible_lookup_preserves_source_bytes_and_",
            "selects_complete_helper_sets",
        ),
        concat!(
            "ReleasePlaybookTests.",
            "test_real_compose_preview_and_installed_selection_",
            "change_only_images_and_image_metadata",
        ),
        concat!(
            "MaintenancePlaybookTests.",
            "test_source_candidate_env_grants_and_launcher_run_as_",
            "one_transient_unit",
        ),
        concat!(
            "PublicPlaybookSelectionTests.",
            "test_a_staged_release_replaces_the_on_host_build_as_the_",
            "image_source",
        ),
    ];
    fn patterns(arguments: &[OsString]) -> Vec<String> {
        arguments
            .windows(2)
            .filter(|pair| pair[0] == "-k")
            .map(|pair| pair[1].to_str().unwrap().to_owned())
            .collect()
    }
    #[test]
    fn both_profiles_route_all_twelve_known_tests_exactly_once() {
        let groups = [
            vec![
                "test_alpha".into(),
                MODULE.into(),
                "test_ci_verified".into(),
                "test_release_container_ci".into(),
            ],
            vec!["test_beta".into(), "test_python_quality".into()],
        ];
        for database in [false, true] {
            let jobs = current_arguments(&groups, database).unwrap();
            assert_eq!(jobs.len(), if database { 1 } else { 2 });
            let selectors = jobs
                .iter()
                .flat_map(|(_, args)| patterns(args))
                .collect::<Vec<_>>();
            assert!(
                !selectors
                    .iter()
                    .any(|value| value == &format!("{MODULE}.*"))
            );
            for id in IDS {
                let full = format!("{MODULE}.{id}");
                let current = selectors
                    .iter()
                    .filter(|pattern| {
                        pattern
                            .strip_suffix('*')
                            .map_or(full.as_str() == pattern.as_str(), |prefix| {
                                full.starts_with(prefix)
                            })
                    })
                    .count();
                let legacy = usize::from(id.starts_with("MaintenancePlaybookTests."));
                assert_eq!(current + legacy, 1, "{database}: {id}");
            }
        }
    }
    #[test]
    fn source_change_cannot_silently_expand_the_reviewed_class_inventory() {
        let original = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ops/ansible/tests/test_release_playbook.py"
        ));
        assert!(source_matches(
            original,
            FROZEN_MODULE_BYTES,
            FROZEN_MODULE_SHA256
        ));
        let mut changed = original.to_vec();
        changed.push(b'\n');
        assert!(!source_matches(
            &changed,
            FROZEN_MODULE_BYTES,
            FROZEN_MODULE_SHA256
        ));
        assert_eq!(
            legacy_arguments()[4],
            "test_release_playbook.MaintenancePlaybookTests"
        );
        assert_eq!(Role::MaintenanceLegacy.maximum_log(), 1024 * 1024);
        assert_eq!(Role::MaintenanceLegacy.index(), 2);
    }
}
