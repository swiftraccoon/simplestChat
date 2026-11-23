//! Exact source and frozen guest-role catalog. No step of these flows runs, ships or
//! imports a Python source of the repository, so none is listed.

use super::options::Purpose;
use std::collections::BTreeSet;

/// The guest playbook that delivers the signed Ops executable, and the production
/// delivery tasks it imports unchanged.
pub(super) const DELIVERY_PLAYBOOK: &str = "ops/ansible/security-vm-ops-install.yml";
pub(super) const DELIVERY_TASKS: &str = "ops/ansible/tasks/signed-ops.yml";

pub(super) fn sources_for(purpose: Purpose) -> BTreeSet<String> {
    if matches!(purpose, Purpose::Infrastructure | Purpose::Boot) {
        return infrastructure_sources();
    }
    let mut names = sources();
    if purpose == Purpose::Deploy {
        for name in [
            ".github/workflows/release-deploy-runtime.yml",
            "ops/ansible/security-vm-native-deploy.yml",
        ] {
            names.insert(name.into());
        }
    }
    names
}
/// What the infrastructure job reads or runs by name: its workflow, the
/// image pin, the four playbooks, the delivery and the controller's own
/// Ansible configuration.
pub(super) fn infrastructure_sources() -> BTreeSet<String> {
    [
        ".github/workflows/infrastructure-security.yml",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "security/vm/debian-cloud.json",
        "security/vm/stage.yml",
        "ops/ansible/site.yml",
        "ops/ansible/public.yml",
        "ops/ansible/backup.yml",
        DELIVERY_PLAYBOOK,
        DELIVERY_TASKS,
        "ops/ansible/ansible.cfg",
        "ops/ansible/requirements.txt",
        "ops/ansible/group_vars/benchmark_hosts.yml",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
pub(super) const MIGRATIONS: [&str; 38] = [
    "001_create_users.sql",
    "002_create_sessions.sql",
    "003_create_rooms.sql",
    "004_create_room_roles.sql",
    "005_create_room_states.sql",
    "006_add_session_token_index.sql",
    "007_add_webauthn_credential_id.sql",
    "008_canonicalize_user_emails.sql",
    "009_add_room_query_indexes.sql",
    "010_add_refresh_token_families.sql",
    "011_add_room_search_trigram_indexes.sql",
    "012_canonicalize_guest_ipv6_moderation_ips.sql",
    "013_community_accounts.sql",
    "014_create_room_reports.sql",
    "015_add_user_chat_style.sql",
    "016_constraints_and_indexes.sql",
    "017_moderation_history.sql",
    "018_user_chat_preferences.sql",
    "019_invites.sql",
    "020_invite_receipts_and_retention.sql",
    "021_invitation_secrets.sql",
    "022_require_current_sessions.sql",
    "023_independent_appearance.sql",
    "024_validate_independent_appearance.sql",
    "025_chat_history.sql",
    "026_validate_chat_moderation.sql",
    "027_chat_updates.sql",
    "028_chat_lookup_indexes.sql",
    "029_chat_reply_index.sql",
    "030_push_subscriptions.sql",
    "031_discovery.sql",
    "032_notification_preferences.sql",
    "033_attachments.sql",
    "034_room_image_version.sql",
    "035_role_numbering.sql",
    "036_validate_role_numbering.sql",
    "037_users_avatar_digest.sql",
    "038_session_access_tokens.sql",
];
pub(super) fn guest_sources() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for name in [
        "Caddyfile",
        "docker-compose.yml",
        "ops/ansible/files/deploy-public.sh",
        "ops/ansible/files/public-compose.sh",
    ] {
        names.insert(name.into());
    }
    for name in [
        "app.env",
        "migration.env",
        "proxy.env",
        "compose.yml",
        "pg_hba.conf",
        "init-database.sql",
        "runtime-grants.sql",
        "postgres-admin-password",
    ] {
        names.insert(format!("ops/ansible/templates/public-{name}.j2"));
    }
    for name in MIGRATIONS {
        names.insert(format!("migrations/{name}"));
    }
    names
}
pub(super) fn sources() -> BTreeSet<String> {
    let mut names = guest_sources();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "security/vm/debian-cloud.json",
        DELIVERY_PLAYBOOK,
        DELIVERY_TASKS,
        "ops/ansible/ansible.cfg",
        "ops/ansible/requirements.txt",
        ".github/workflows/maintenance-launcher-runtime.yml",
        "ops/ansible/security-vm-maintain-prerequisite.yml",
        "ops/ansible/group_vars/benchmark_hosts.yml",
        "ops/ansible/tasks/docker.yml",
        "ops/ansible/tasks/runtime-binaries.yml",
        "ops/ansible/tasks/runtime-verify.yml",
        "ops/ansible/templates/docker.sources.j2",
        "ops/ansible/templates/docker.preferences.j2",
        "ops/ansible/templates/containerd-runtime.conf.j2",
    ] {
        names.insert(name.into());
    }
    for name in [
        "ansible",
        "auth",
        "catalog",
        "control",
        "deploy_capture",
        "deploy_execute",
        "deploy_options",
        "deploy_options_tests",
        "deploy_projection",
        "deploy_projection_tests",
        "deploy_workflow_tests",
        "evidence",
        "execute",
        "files",
        "host_baseline",
        "host_diagnostic",
        "host_projection",
        "host_upgrade",
        "infrastructure",
        "infrastructure_oracle_tests",
        "infrastructure_tests",
        "install_tests",
        "launcher_diagnostic",
        "mod",
        "options",
        "prepared",
        "process",
        "projection",
        "qmp",
        "session",
        "source",
        "tests",
        "vm",
        "work",
        "work_tests",
    ] {
        names.insert(format!(
            "crates/release/src/ops_artifact/maintain_runtime/{name}.rs"
        ));
    }
    for name in [
        "ops/Cargo.toml",
        "ops/ops-build-inputs.lock.json",
        "ops/src/lib.rs",
        "ops/src/main.rs",
        "ops/src/distribution_build_plan.rs",
        "ops/src/distribution_build_plan_tests.rs",
        "ops/src/release/mod.rs",
        "ops/src/release/maintain_prerequisite.rs",
        "ops/src/release/attestation/maintain_prerequisite.rs",
        "crates/release/Cargo.toml",
        "crates/release/src/main.rs",
        "crates/toolkit/Cargo.toml",
        "crates/toolkit/src/lib.rs",
        "crates/toolkit/src/executable.rs",
        "crates/toolkit/src/owned_process.rs",
        "crates/toolkit/src/replay.rs",
        "crates/toolkit/src/tooling.rs",
        "crates/ci/Cargo.toml",
        "crates/ci/src/lib.rs",
        "crates/ci/src/main.rs",
        "crates/ci/src/runner.rs",
        "crates/ci/src/apt_mirror.rs",
        "crates/ci/src/automation/workflow_rpm_diagnostic_tests.rs",
        "crates/ci/src/release_controller_files.rs",
        "crates/ci/src/release_controller_plan.rs",
        "crates/ci/src/rpm_diagnostic.rs",
        "crates/ci/src/rpm_diagnostic_tests.rs",
        "crates/ci/src/tests.rs",
        "crates/release/src/ops_artifact.rs",
        "crates/release/src/ops_artifact/fixture.rs",
        "crates/release/src/ops_artifact/fixture_driver.rs",
        "crates/release/src/ops_artifact/fixture_driver_tests.rs",
        "crates/release/src/ops_artifact/native_tests.rs",
        "crates/release/src/ops_artifact/recipe.rs",
        "crates/release/src/ops_artifact/resolution.rs",
        "ops/src/owned_process.rs",
        "ops/src/owned_process_tests.rs",
        "ops/src/release/attestation.rs",
        "ops/src/release/attestation/control_signals.rs",
        "docs/testing.md",
        ".github/workflows/maintenance-host-runtime.yml",
        "ops/README.md",
        "ops/src/release/maintain.rs",
        "ops/src/release/attestation/maintain.rs",
        "ops/src/release/attestation/control.rs",
        "ops/src/release/attestation/control_maintain_tests.rs",
        "ops/src/release/attestation/control_signal_tests.rs",
        "ops/src/release/attestation/files.rs",
        "ops/src/release/attestation/files_tests.rs",
        "ops/src/release/workload.rs",
        "ops/src/release/manifest.rs",
        "ops/src/release/manifest_tests.rs",
        "ops/src/release/stage.rs",
        "ops/src/release/attestation/stage.rs",
        "ops/src/release/deploy.rs",
        "ops/src/release/attestation/deploy.rs",
        "ops/src/release/archive.rs",
        "ops/src/json.rs",
        "ops/src/files.rs",
        "ops/src/files_tests.rs",
        "ops/src/files_scope_tests.rs",
        "ops/src/distribution_install.rs",
        "ops/src/distribution_install_files.rs",
        "ops/src/distribution_install_control.rs",
        "ops/src/distribution_install_request.rs",
        "ops/src/distribution_install_tests.rs",
        "ops/tests/release_maintain_cli.rs",
        "ops/src/release/attestation/maintain_prerequisite/guest/effects/proxy-legacy.Caddyfile",
        "ops/src/release/attestation/maintain_prerequisite/guest/recorded_materials.json",
        "ops/src/release/archive/recorded_archive.json",
        "ops/src/release/recorded_assets.json",
        "ops/src/release/recorded_capture.json",
        "ops/src/release/recorded_manifest.json",
    ] {
        names.insert(name.into());
    }
    for name in ["files", "files_tests", "policy", "tests"] {
        names.insert(format!("crates/ci/src/apt_mirror/{name}.rs"));
    }
    for name in ["identity", "process", "protocol", "supervisor", "tests"] {
        names.insert(format!(
            "ops/src/release/attestation/maintain_prerequisite/{name}.rs"
        ));
    }
    for name in [
        "mod",
        "files",
        "plan",
        "worker",
        "execute",
        "request",
        "tests",
        "lifecycle",
    ] {
        names.insert(format!("crates/release/src/ops_artifact/export/{name}.rs"));
    }
    for name in [
        "files",
        "options",
        "process",
        "process_io_tests",
        "process_restore_io_tests",
        "process_tests",
        "tests",
        "reentry",
        "reentry/tests",
    ] {
        names.insert(format!("ops/src/release/attestation/stage/{name}.rs"));
    }
    for name in [
        "budget",
        "bulk_effect_tests",
        "effect_tests",
        "execution",
        "files",
        "maintenance",
        "maintenance_effect_tests",
        "maintenance_selection",
        "options",
        "policy",
        "process",
        "process_recovery_tests",
        "records",
        "replacement",
        "retention",
        "rollback_diagnostic",
        "rollback_effect_tests",
        "steps",
        "tests",
    ] {
        names.insert(format!("ops/src/release/attestation/deploy/{name}.rs"));
    }
    for name in [
        "assets",
        "assets_capture",
        "assets_capture_tests",
        "assets_commit",
        "assets_files",
        "assets_host",
        "assets_host_tests",
        "assets_lock",
        "assets_model",
        "assets_oracle_tests",
        "assets_scope",
        "assets_scope_tests",
        "assets_store",
        "assets_tests",
    ] {
        names.insert(format!("ops/src/release/{name}.rs"));
    }
    for name in [
        "docker",
        "generator",
        "generator_config",
        "generator_oci",
        "generator_tests",
        "index",
        "oci",
        "tests",
    ] {
        names.insert(format!("ops/src/release/archive/{name}.rs"));
    }
    for name in [
        "control",
        "deploy/mod",
        "deploy/protocol",
        "deploy/files",
        "deploy/payload",
        "deploy/probe",
        "deploy/effects",
        "deploy/readiness",
        "deploy/grants",
        "deploy/operation",
        "deploy/history",
        "deploy/injection",
        "deploy/injection_client",
        "deploy/injection_client/context",
        "deploy/injection_client/wire",
        "deploy/injection_client/namespace",
        "deploy/injection_client/tests",
        "deploy/native_report",
        "deploy/validation",
        "deploy/tests",
        "effects/actors",
        "effects/cold_pg",
        "effects/cold_pg/tests",
        "effects/effects_tests",
        "effects/migration_upgrade",
        "effects/mod",
        "effects/observation",
        "effects/pg_window",
        "effects/pg_window_tests",
        "effects/policy",
        "effects/policy/legacy_proxy",
        "effects/proxy_upgrade",
        "files",
        "guest_tests",
        "host",
        "host/effects",
        "host/events",
        "host/events/diagnostic",
        "host/events/proxy_upgrade",
        "host/files",
        "host/migration",
        "host/tests",
        "host/upgrade",
        "materials",
        "materials_reference_tests",
        "materials_tests",
        "mod",
        "native",
        "observation",
        "process",
        "process_deploy_tests",
        "protocol",
        "render",
        "render_tests",
        "sources",
        "tls",
        "tls_tests",
    ] {
        names.insert(format!(
            "ops/src/release/attestation/maintain_prerequisite/guest/{name}.rs"
        ));
    }
    for name in [
        "archive",
        "archive_tests",
        "catalog",
        "context",
        "coordinator",
        "coordinator_tests",
        "fetch",
        "files",
        "files_tests",
        "fixture",
        "kit",
        "mod",
        "native_build",
        "native_build_tests",
        "native_provenance",
        "native_tests",
        "plan",
        "progress",
        "progress_tests",
        "provenance",
        "shim",
        "signing",
        "signing_tests",
        "tests",
        "tools",
        "whole_tests",
        "worker",
        "worker_tests",
    ] {
        names.insert(format!("crates/release/src/ops_artifact/hosted/{name}.rs"));
    }
    names
}
