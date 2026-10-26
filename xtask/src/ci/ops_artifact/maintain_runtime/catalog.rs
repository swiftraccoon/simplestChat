//! Exact source and frozen guest-role catalog; no Python imports select this roster.

use std::collections::BTreeSet;
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
        "build/security_vm_maintain_prerequisite_guest.py",
        "build/security_vm_stage_guest.py",
        "build/caddy_probe_guest.py",
        "build/caddy_materials.py",
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
        "execute",
        "files",
        "mod",
        "options",
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
        "xtask/src/main.rs",
        "xtask/src/ci/ops_artifact.rs",
        "xtask/src/ci/tests.rs",
        "ops/src/owned_process.rs",
        "ops/src/owned_process_tests.rs",
        "ops/src/release/attestation.rs",
        "ops/src/release/attestation/control_signals.rs",
        "docs/maintain-controller-api-review.md",
    ] {
        names.insert(name.into());
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
