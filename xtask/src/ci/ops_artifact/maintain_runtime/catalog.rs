//! Exact source and frozen guest-role catalog; no Python imports select this roster.

use super::options::Purpose;
use std::collections::BTreeSet;

pub(super) fn sources_for(purpose: Purpose) -> BTreeSet<String> {
    let mut names = sources();
    if purpose == Purpose::Deploy {
        for name in [
            ".github/workflows/release-deploy-runtime.yml",
            "build/security_vm_deploy.py",
            "build/security_vm_deploy_guest.py",
            "build/security_vm_deploy_injection.py",
            "build/release_container_harness.py",
            "ops/ansible/security-vm-native-deploy.yml",
            "ops/ansible/tests/test_security_vm_deploy.py",
            "ops/ansible/tests/test_security_vm_deploy_injection.py",
            "docs/release-deploy-api-review.md",
            "docs/release-deploy-runtime-proof.md",
            "xtask/src/ci/ops_artifact.rs",
        ] {
            names.insert(name.into());
        }
    }
    names
}
pub(super) const HELPERS: [&str; 15] = [
    "bounded_process.py",
    "release-public.py",
    "release_public.py",
    "release_artifact.py",
    "release_assets.py",
    "browser_assets.py",
    "release_json.py",
    "runtime_profile.py",
    "reboot-public.py",
    "reboot_public.py",
    "fetch-release.py",
    "release_fetch_receiver.py",
    "release_trust.py",
    "force_release.py",
    "security_vm_ops_guest.py",
];
pub(super) const MIGRATIONS: [&str; 36] = [
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
];
pub(super) fn guest_sources() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for name in [
        "Caddyfile",
        "docker-compose.yml",
        "ops/ansible/files/deploy-public.sh",
        "ops/ansible/files/public-compose.sh",
        "ops/ansible/files/seed-public-room.py",
        "ops/ansible/files/seed_public_room.py",
        "ops/ansible/files/release_json.py",
        "ops/ansible/files/runtime_profile.py",
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
    for helper in HELPERS {
        names.insert(if helper == "security_vm_ops_guest.py" {
            format!("build/{helper}")
        } else {
            format!("ops/ansible/files/{helper}")
        });
    }
    for name in [
        "Cargo.toml",
        // The unchanged outer image selector reads this fixed literal as data.
        "build/caddy_probe_guest.py",
        "Cargo.lock",
        "rust-toolchain.toml",
        "build/security_vm_fetch.py",
        "build/security_vm_fetch_guest.py",
        "build/security_vm_fetch_hosts.py",
        "security/vm/fetch.yml",
        "security/vm/debian-cloud.json",
        "build/security_vm_ops.py",
        "build/security_vm_vps_guest.py",
        "build/vps_ops_bootstrap.py",
        "build/security_vm.py",
        "build/security_vm_qmp.py",
        "build/security_context.py",
        "build/security_tools.py",
        "build/python-requirements.txt",
        "security/vm/ops-stage.yml",
        "ops/ansible/ansible.cfg",
        "ops/ansible/requirements.txt",
        "ops/ansible/files/release_preflight.py",
        "build/security_vm_release.py",
        "build/release_attestation.py",
        "build/release_fetch_controller.py",
        "build/release_deploy.py",
        "build/release_build.py",
        ".github/workflows/release-receive-runtime.yml",
        "build/security_vm_receive.py",
        "build/security_vm_receive_guest.py",
        "build/security_vm_receive_route.py",
        "security/vm/receive.yml",
        "build/security_vm_maintain_prerequisite.py",
        ".github/workflows/maintenance-launcher-runtime.yml",
        "ops/ansible/security-vm-maintain-prerequisite.yml",
        "ops/ansible/tests/test_security_vm_maintain_prerequisite.py",
        "docs/maintenance-launcher-runtime-proof.md",
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
        "execute",
        "files",
        "host_baseline",
        "host_diagnostic",
        "host_projection",
        "host_upgrade",
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
            "xtask/src/ci/ops_artifact/maintain_runtime/{name}.rs"
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
        "xtask/Cargo.toml",
        "xtask/src/main.rs",
        "xtask/src/owned_process.rs",
        "xtask/src/tooling.rs",
        "xtask/src/ci/mod.rs",
        "xtask/src/ci/runner.rs",
        "xtask/src/ci/apt_mirror.rs",
        "xtask/src/ci/automation/workflow_rpm_diagnostic_tests.rs",
        "xtask/src/ci/release_controller_files.rs",
        "xtask/src/ci/release_controller_plan.rs",
        "xtask/src/ci/rpm_diagnostic.rs",
        "xtask/src/ci/rpm_diagnostic_tests.rs",
        "xtask/src/ci/ops_artifact.rs",
        "xtask/src/ci/ops_artifact/fixture.rs",
        "xtask/src/ci/ops_artifact/fixture_driver.rs",
        "xtask/src/ci/ops_artifact/fixture_driver_tests.rs",
        "xtask/src/ci/ops_artifact/native_tests.rs",
        "xtask/src/ci/ops_artifact/recipe.rs",
        "xtask/src/ci/ops_artifact/resolution.rs",
        "xtask/src/ci/tests.rs",
        "ops/src/owned_process.rs",
        "ops/src/owned_process_tests.rs",
        "ops/src/release/attestation.rs",
        "ops/src/release/attestation/control_signals.rs",
        "docs/maintain-controller-api-review.md",
        "docs/maintain-native-prerequisite-api-review.md",
        "docs/testing.md",
        ".github/workflows/maintenance-host-runtime.yml",
        "docs/release-maintain-api-review.md",
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
    ] {
        names.insert(name.into());
    }
    for name in ["files", "files_tests", "policy", "tests"] {
        names.insert(format!("xtask/src/ci/apt_mirror/{name}.rs"));
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
        names.insert(format!("xtask/src/ci/ops_artifact/export/{name}.rs"));
    }
    for name in [
        "files",
        "options",
        "oracle_tests",
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
        "oracle_tests",
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
        names.insert(format!("xtask/src/ci/ops_artifact/hosted/{name}.rs"));
    }
    names
}
pub(super) const OPS_CASES: [(&str, i64); 33] = [
    ("vps-fd-cloexec", 1),
    ("vps-stage-exchange", 1),
    ("vps-stage-unlinked", 1),
    ("vps-wrong-image", 1),
    ("vps-origin-expired", 124),
    ("source-help", 0),
    ("source-invalid-cli", 2),
    ("source-forbidden-root", 2),
    ("source-inspect", 0),
    ("source-pair", 0),
    ("install-foreign-bytes", 1),
    ("install-foreign-mode", 1),
    ("install-first", 0),
    ("install-repeat", 0),
    ("installed-help", 0),
    ("installed-inspect", 0),
    ("installed-pair", 0),
    ("preflight-ordinary", 0),
    ("preflight-base", 0),
    ("preflight-fetch", 0),
    ("preflight-force", 0),
    ("preflight-invalid-shape", 1),
    ("preflight-unknown-helper", 1),
    ("preflight-invalid-digest", 1),
    ("preflight-combined", 1),
    ("preflight-cli", 2),
    ("preflight-unprivileged", 1),
    ("preflight-mode", 1),
    ("preflight-hash", 1),
    ("release-manifest", 0),
    ("release-manifest-private", 1),
    ("installed-final-inspect", 0),
    ("installed-final-pair", 0),
];
