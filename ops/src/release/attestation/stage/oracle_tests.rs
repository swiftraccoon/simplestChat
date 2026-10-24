//! Genuine complete unchanged Python stage comparison. No engine/authentication claim.
use super::*;
use crate::test_support::Fixture;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};
const WHOLE: Duration = Duration::from_secs(180);
const RESERVE: Duration = Duration::from_secs(30);
const PYTHON_BODY: Duration = Duration::from_secs(60);
const PYTHON_TAIL: Duration = Duration::from_secs(10);
const FILE_BYTES: u64 = 4 * 1024 * 1024;
const SCENARIOS: &[&str] = &[
    "success",
    "received-hardlink-pair",
    "oci-config-id",
    "oci-manifest-id",
    "tool-whitespace",
    "repeat",
    "staged-repeat",
    "disk-plus",
    "daemon-outside-graph",
    "root-mode",
    "config-mode",
    "releases-mode",
    "revision-mode",
    "results-mode",
    "manifest-mode",
    "archive-mode",
    "manifest-symlink",
    "archive-symlink",
    "lock-mode",
    "lock-busy",
    "current-unfinished",
    "release-unfinished",
    "journal-schema-bool",
    "manifest-syntax",
    "manifest-duplicate",
    "manifest-extra-key",
    "manifest-revision",
    "manifest-platform",
    "manifest-migrations",
    "manifest-created",
    "archive-digest",
    "archive-empty",
    "archive-unsafe",
    "archive-config",
    "archive-duplicate",
    "archive-layer-missing",
    "staged-manifest-diff",
    "staged-image-missing",
    "staged-symlink",
    "staged-mode",
    "staged-syntax",
    "staged-missing-key",
    "disk-below",
    "disk-exact",
    "load-failure",
    "inspect-failure",
    "image-id",
    "image-os",
    "image-architecture",
    "image-user",
    "image-label",
    "image-cmd",
    "image-entrypoint",
    "image-workdir",
    "image-healthcheck",
    "image-environment",
    "create-close",
    "create-close-remove-failure",
    "create-failure",
    "create-invalid",
    "start-failure",
    "wait-failure",
    "wait-nonzero",
    "logs-failure",
    "logs-invalid",
    "logs-empty",
    "logs-duplicate",
    "logs-mismatch",
    "logs-wide-version",
    "remove-failure",
    "import-journal-write",
    "validate-journal-write",
    "validated-journal-write",
    "final-journal-write",
    "validation-name-write",
    "staged-write",
    "outcome-write",
    "staged-file-sync",
    "staged-directory-sync",
    "outcome-file-sync",
    "outcome-directory-sync",
];
const BASELINE_PYTHON_PATHS: &[&str] = &[
    "build/bootstrap.py",
    "build/bootstrap_access.py",
    "build/bootstrap_controller.py",
    "build/build-release.py",
    "build/capacity.py",
    "build/capacity_artifacts.py",
    "build/ci-local-evidence.py",
    "build/ci_cache_retention.py",
    "build/ci_local_act.py",
    "build/ci_release_checks.py",
    "build/ci_verified.py",
    "build/deploy.py",
    "build/fetch-release.py",
    "build/force_deploy.py",
    "build/issue_account_recovery.py",
    "build/migrate.py",
    "build/migration_artifact.py",
    "build/migration_controller.py",
    "build/migration_dns.py",
    "build/native_security.py",
    "build/native_security_cache.py",
    "build/release_attestation.py",
    "build/release_build.py",
    "build/release_container_fixture.py",
    "build/release_container_harness.py",
    "build/release_deploy.py",
    "build/release_fetch_controller.py",
    "build/release_http_policy.py",
    "build/run-vps-capacity.py",
    "build/security_actions.py",
    "build/security_archive.py",
    "build/security_awslc.py",
    "build/security_check.py",
    "build/security_codeql.py",
    "build/security_codeql_cache.py",
    "build/security_codeql_cargo.py",
    "build/security_codeql_local.py",
    "build/security_codeql_resources.py",
    "build/security_codeql_rust_sources.py",
    "build/security_codeql_sources.py",
    "build/security_codeql_tools.py",
    "build/security_codeql_triage.py",
    "build/security_context.py",
    "build/security_dependency_licenses.py",
    "build/security_elf.py",
    "build/security_findings.py",
    "build/security_image.py",
    "build/security_image_advisories.py",
    "build/security_image_cache.py",
    "build/security_image_policy.py",
    "build/security_license_identity.py",
    "build/security_mutation.py",
    "build/security_native.py",
    "build/security_openssl.py",
    "build/security_policy.py",
    "build/security_rpm_notices.py",
    "build/security_rulesets.py",
    "build/security_runtime.py",
    "build/security_secret_projection.py",
    "build/security_secret_spans.py",
    "build/security_secrets.py",
    "build/security_semgrep.py",
    "build/security_semgrep_summary.py",
    "build/security_source_scope.py",
    "build/security_tools.py",
    "build/security_vendor.py",
    "build/security_vm.py",
    "build/security_vm_fetch.py",
    "build/security_vm_fetch_guest.py",
    "build/security_vm_fetch_hosts.py",
    "build/security_vm_guest.py",
    "build/security_vm_ops.py",
    "build/security_vm_ops_guest.py",
    "build/security_vm_qmp.py",
    "build/security_vm_receive.py",
    "build/security_vm_receive_guest.py",
    "build/security_vm_receive_route.py",
    "build/security_vm_release.py",
    "build/security_vm_vps_guest.py",
    "build/test-release-container.py",
    "build/verify-release.py",
    "build/vps_capacity_controller.py",
    "build/vps_capacity_remote.py",
    "build/vps_ops_bootstrap.py",
    "ops/ansible/callback_plugins/release_timing.py",
    "ops/ansible/files/backup_offsite.py",
    "ops/ansible/files/backup_public.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/browser_assets.py",
    "ops/ansible/files/database_logging.py",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/force_release.py",
    "ops/ansible/files/migrate_public.py",
    "ops/ansible/files/migration_inspect.py",
    "ops/ansible/files/migration_snapshot.py",
    "ops/ansible/files/migration_tls.py",
    "ops/ansible/files/monitoring_alerts.py",
    "ops/ansible/files/monitoring_collect.py",
    "ops/ansible/files/monitoring_external.py",
    "ops/ansible/files/monitoring_prepare.py",
    "ops/ansible/files/monitoring_report.py",
    "ops/ansible/files/reboot-public.py",
    "ops/ansible/files/reboot_public.py",
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_preflight.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/seed-public-room.py",
    "ops/ansible/files/seed_public_room.py",
    "ops/ansible/files/turn_public.py",
    "ops/ansible/filter_plugins/sizing.py",
    "ops/ansible/tests/release_build_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_prepare_oracle.py",
    "ops/ansible/tests/release_receive_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_automation.py",
    "ops/ansible/tests/test_backup_offsite.py",
    "ops/ansible/tests/test_bootstrap_controller.py",
    "ops/ansible/tests/test_bounded_process.py",
    "ops/ansible/tests/test_browser_assets_command.py",
    "ops/ansible/tests/test_capacity.py",
    "ops/ansible/tests/test_capacity_artifacts.py",
    "ops/ansible/tests/test_capacity_container.py",
    "ops/ansible/tests/test_capacity_ownership.py",
    "ops/ansible/tests/test_ci_cache_retention.py",
    "ops/ansible/tests/test_ci_database_check.py",
    "ops/ansible/tests/test_ci_local_act.py",
    "ops/ansible/tests/test_ci_local_evidence.py",
    "ops/ansible/tests/test_ci_release_checks.py",
    "ops/ansible/tests/test_ci_verified.py",
    "ops/ansible/tests/test_database_logging.py",
    "ops/ansible/tests/test_deploy_controller.py",
    "ops/ansible/tests/test_dns_handoff.py",
    "ops/ansible/tests/test_force_release.py",
    "ops/ansible/tests/test_host_playbooks.py",
    "ops/ansible/tests/test_issue_account_recovery.py",
    "ops/ansible/tests/test_migrate_public.py",
    "ops/ansible/tests/test_migration_abort.py",
    "ops/ansible/tests/test_migration_artifact.py",
    "ops/ansible/tests/test_migration_controller.py",
    "ops/ansible/tests/test_migration_dns.py",
    "ops/ansible/tests/test_migration_inspect.py",
    "ops/ansible/tests/test_migration_prepare.py",
    "ops/ansible/tests/test_migration_tls.py",
    "ops/ansible/tests/test_monitoring.py",
    "ops/ansible/tests/test_monitoring_database.py",
    "ops/ansible/tests/test_monitoring_external.py",
    "ops/ansible/tests/test_native_security.py",
    "ops/ansible/tests/test_ops_artifact_workflow.py",
    "ops/ansible/tests/test_ops_runtime_workflow.py",
    "ops/ansible/tests/test_performance_compare_ci.py",
    "ops/ansible/tests/test_public_backup.py",
    "ops/ansible/tests/test_public_deployment.py",
    "ops/ansible/tests/test_public_reboot.py",
    "ops/ansible/tests/test_public_release.py",
    "ops/ansible/tests/test_public_seed.py",
    "ops/ansible/tests/test_public_templates.py",
    "ops/ansible/tests/test_python_quality.py",
    "ops/ansible/tests/test_release_assets.py",
    "ops/ansible/tests/test_release_attestation.py",
    "ops/ansible/tests/test_release_build.py",
    "ops/ansible/tests/test_release_build_oracle.py",
    "ops/ansible/tests/test_release_container_ci.py",
    "ops/ansible/tests/test_release_container_fixture.py",
    "ops/ansible/tests/test_release_container_harness.py",
    "ops/ansible/tests/test_release_fetch.py",
    "ops/ansible/tests/test_release_fetch_controller.py",
    "ops/ansible/tests/test_release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_release_fetch_runtime_workflow.py",
    "ops/ansible/tests/test_release_fresh_verify_oracle.py",
    "ops/ansible/tests/test_release_http_policy.py",
    "ops/ansible/tests/test_release_json.py",
    "ops/ansible/tests/test_release_playbook.py",
    "ops/ansible/tests/test_release_preflight.py",
    "ops/ansible/tests/test_release_prepare_oracle.py",
    "ops/ansible/tests/test_release_receive_oracle.py",
    "ops/ansible/tests/test_release_timing.py",
    "ops/ansible/tests/test_release_verify_oracle.py",
    "ops/ansible/tests/test_release_workflow.py",
    "ops/ansible/tests/test_restore_database.py",
    "ops/ansible/tests/test_restore_verify.py",
    "ops/ansible/tests/test_runtime_components.py",
    "ops/ansible/tests/test_runtime_profile.py",
    "ops/ansible/tests/test_security_actions.py",
    "ops/ansible/tests/test_security_archive.py",
    "ops/ansible/tests/test_security_base_secrets.py",
    "ops/ansible/tests/test_security_check.py",
    "ops/ansible/tests/test_security_ci.py",
    "ops/ansible/tests/test_security_codeql_cache.py",
    "ops/ansible/tests/test_security_codeql_cargo.py",
    "ops/ansible/tests/test_security_codeql_health.py",
    "ops/ansible/tests/test_security_codeql_local.py",
    "ops/ansible/tests/test_security_codeql_resources.py",
    "ops/ansible/tests/test_security_codeql_rust_sources.py",
    "ops/ansible/tests/test_security_codeql_sources.py",
    "ops/ansible/tests/test_security_codeql_triage.py",
    "ops/ansible/tests/test_security_context.py",
    "ops/ansible/tests/test_security_dependency_licenses.py",
    "ops/ansible/tests/test_security_elf.py",
    "ops/ansible/tests/test_security_fedora_license.py",
    "ops/ansible/tests/test_security_findings.py",
    "ops/ansible/tests/test_security_image_advisories.py",
    "ops/ansible/tests/test_security_image_cache.py",
    "ops/ansible/tests/test_security_image_evidence.py",
    "ops/ansible/tests/test_security_image_finalization.py",
    "ops/ansible/tests/test_security_image_parallel.py",
    "ops/ansible/tests/test_security_image_policy.py",
    "ops/ansible/tests/test_security_image_prepare.py",
    "ops/ansible/tests/test_security_image_runner.py",
    "ops/ansible/tests/test_security_image_secrets.py",
    "ops/ansible/tests/test_security_license_identity.py",
    "ops/ansible/tests/test_security_migrations.py",
    "ops/ansible/tests/test_security_mutation.py",
    "ops/ansible/tests/test_security_native.py",
    "ops/ansible/tests/test_security_openssl.py",
    "ops/ansible/tests/test_security_policy.py",
    "ops/ansible/tests/test_security_rpm_license.py",
    "ops/ansible/tests/test_security_rpm_notices.py",
    "ops/ansible/tests/test_security_rpm_secrets.py",
    "ops/ansible/tests/test_security_rulesets.py",
    "ops/ansible/tests/test_security_runtime.py",
    "ops/ansible/tests/test_security_runtime_licenses.py",
    "ops/ansible/tests/test_security_secret_projection.py",
    "ops/ansible/tests/test_security_secret_spans.py",
    "ops/ansible/tests/test_security_secrets.py",
    "ops/ansible/tests/test_security_semgrep.py",
    "ops/ansible/tests/test_security_semgrep_summary.py",
    "ops/ansible/tests/test_security_tools.py",
    "ops/ansible/tests/test_security_vendor.py",
    "ops/ansible/tests/test_security_vm.py",
    "ops/ansible/tests/test_security_vm_boot.py",
    "ops/ansible/tests/test_security_vm_fetch.py",
    "ops/ansible/tests/test_security_vm_fetch_guest.py",
    "ops/ansible/tests/test_security_vm_fetch_hosts.py",
    "ops/ansible/tests/test_security_vm_guest.py",
    "ops/ansible/tests/test_security_vm_ops.py",
    "ops/ansible/tests/test_security_vm_ops_guest.py",
    "ops/ansible/tests/test_security_vm_qmp.py",
    "ops/ansible/tests/test_security_vm_receive.py",
    "ops/ansible/tests/test_security_vm_release.py",
    "ops/ansible/tests/test_security_vm_vps_guest.py",
    "ops/ansible/tests/test_session_soak_ci.py",
    "ops/ansible/tests/test_support.py",
    "ops/ansible/tests/test_turn_public.py",
    "ops/ansible/tests/test_vps_capacity_controller.py",
    "ops/ansible/tests/test_vps_capacity_oracle.py",
    "ops/ansible/tests/test_vps_capacity_remote.py",
    "ops/ansible/tests/test_vps_ops_bootstrap.py",
    "ops/ansible/tests/vps_capacity_oracle.py",
    "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/link_file_in_build_dir.py",
    "vendor/mediasoup-sys-0.19.0/tasks.py",
    "xtask/src/ci/release_http_policy/__init__.py",
    "xtask/src/ci/release_http_policy/oracle.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_fixture.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_harness.py",
    "xtask/src/ci/release_http_policy/reference/build/release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/bounded_process.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_artifact.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_assets.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_json.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_public.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/runtime_profile.py",
    concat!(
        "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/",
        "test_release_container_harness.py"
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Daemon {
    inspect: String,
    create: String,
    wait: String,
    logs: String,
    fault_role: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    scenario: String,
    revision: String,
    repeat: usize,
    free_bytes: u64,
    config_sha256: String,
    graph_manifest_sha256: Option<String>,
    fault: Option<String>,
    daemon: Daemon,
    native_profile: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reference {
    schema_version: u8,
    scenario: String,
    input: Input,
    observation: Value,
}
fn fault(scenario: &str) -> Option<files::Fault> {
    Some(match scenario {
        "import-journal-write" => files::Fault::ImportJournalWrite,
        "validate-journal-write" => files::Fault::ValidateJournalWrite,
        "validated-journal-write" => files::Fault::ValidatedJournalWrite,
        "final-journal-write" => files::Fault::FinalJournalWrite,
        "validation-name-write" => files::Fault::ValidationNameWrite,
        "staged-write" => files::Fault::StagedWrite,
        "outcome-write" => files::Fault::OutcomeWrite,
        "staged-file-sync" => files::Fault::StagedFileSync,
        "staged-directory-sync" => files::Fault::StagedDirectorySync,
        "outcome-file-sync" => files::Fault::OutcomeFileSync,
        "outcome-directory-sync" => files::Fault::OutcomeDirectorySync,
        _ => return None,
    })
}
fn failure(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn replace_token(value: &mut String, prefix: &str, width: usize, replacement: &str) {
    while let Some(start) = value.find(prefix) {
        let end = start + prefix.len() + width;
        if end <= value.len()
            && value.as_bytes()[start + prefix.len()..end]
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            value.replace_range(start..end, replacement);
        } else {
            break;
        }
    }
}
fn normalize(value: &mut Value, root: &Path) {
    match value {
        Value::Object(values) => {
            for (name, value) in values {
                if matches!(name.as_str(), "startedAt" | "finishedAt" | "stagedAt") {
                    *value = "$UTC".into();
                } else {
                    normalize(value, root);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, root);
            }
        }
        Value::String(text) => {
            for (path, replacement) in [
                (root.join("public"), "$PUBLIC"),
                (root.join("config"), "$CONFIG"),
                (root.join("work"), "$WORK"),
                (root.to_path_buf(), "$FIXTURE"),
            ] {
                if let Some(path) = path.to_str() {
                    *text = text.replace(path, replacement);
                }
            }
            let prefix = "$PUBLIC/results/release.";
            if let Some(start) = text.find(prefix) {
                let end = text[start + prefix.len()..]
                    .find('/')
                    .map_or(text.len(), |offset| start + prefix.len() + offset);
                text.replace_range(start + "$PUBLIC/results/".len()..end, "$ATTEMPT");
            }
            replace_token(text, "scpub-release-validate-", 32, "$VALIDATOR");
        }
        _ => {}
    }
}
fn complete_atomic_temp(
    path: &Path,
    data: &[u8],
    root: &Path,
    end: Instant,
) -> io::Result<Option<Value>> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let Some(suffix) = name.strip_prefix(".release-") else {
        return Ok(None);
    };
    if suffix.is_empty()
        || suffix.len() > 64
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Ok(None);
    }
    let control = pin(&root.join("control.json"), root, end)?;
    let control = &control["value"];
    let scenario = control["scenario"].as_str();
    let staged = scenario == Some("staged-file-sync")
        && path.parent() == Some(root.join("public/releases").join("a".repeat(40)).as_path());
    let outcome = scenario == Some("outcome-file-sync")
        && path.parent().and_then(Path::parent) == Some(root.join("public/results").as_path())
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("release."));
    if !staged && !outcome {
        return Ok(None);
    }
    let maximum = usize::try_from(FILE_BYTES).map_err(|_| failure("temp bound"))?;
    let Some(mut value) = crate::json::decode_limited(data, maximum)
        .ok()
        .and_then(|_| serde_json::from_slice::<Value>(data).ok())
    else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.get("revision").and_then(Value::as_str) != Some("a".repeat(40).as_str()) {
        return Ok(None);
    }
    let date = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|v| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%SZ").is_ok())
    };
    let own = if staged {
        let expected = [
            "schemaVersion",
            "revision",
            "serverImage",
            "manifestSha256",
            "stagedAt",
        ];
        let inspection = control["daemon"]["inspect"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok());
        let manifest = pin(
            &root
                .join("public/releases")
                .join("a".repeat(40))
                .join("release.json"),
            root,
            end,
        )?;
        if object.len() != expected.len()
            || !expected.iter().all(|name| object.contains_key(*name))
            || value["schemaVersion"].as_u64() != Some(1)
            || !date("stagedAt")
            || inspection.as_ref().and_then(|v| v.get("id")) != value.get("serverImage")
            || value.get("manifestSha256") != manifest.get("sha256")
        {
            return Ok(None);
        }
        super::bytes(&value).map_err(|_| failure("own staged serializer"))?
    } else {
        let expected = [
            "action",
            "revision",
            "startedAt",
            "passed",
            "phase",
            "finishedAt",
        ];
        if object.len() != expected.len()
            || !expected.iter().all(|name| object.contains_key(*name))
            || value["action"] != "stage"
            || value["passed"] != true
            || value["phase"] != "complete"
            || !date("startedAt")
            || !date("finishedAt")
        {
            return Ok(None);
        }
        // The operation serializes this private record from its ordered Report
        // type, not a generic BTree map. Use that same writer for exact framing.
        let Some(revision) = value["revision"].as_str() else {
            return Ok(None);
        };
        let Some(started) = value["startedAt"].as_str() else {
            return Ok(None);
        };
        let Some(finished) = value["finishedAt"].as_str() else {
            return Ok(None);
        };
        let report = Report {
            action: "stage",
            revision: revision.into(),
            started_at: started.into(),
            passed: true,
            phase: "complete",
            failure: None,
            finished_at: Some(finished.into()),
        };
        super::bytes(&report).map_err(|_| failure("own outcome serializer"))?
    };
    if own != data {
        return Ok(None);
    }
    normalize(&mut value, root);
    Ok(Some(
        json!({"role":if staged{"staged"}else{"outcome"},"complete":true,
        "ownSerializationMatches":true,"value":value}),
    ))
}
fn pin(path: &Path, root: &Path, end: Instant) -> io::Result<Value> {
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let mut file = File::from(descriptor);
    let observed = (|| {
        let before = file.metadata()?;
        if !before.is_file() || before.len() > FILE_BYTES || Instant::now() >= end {
            return Err(failure("stage evidence bound or original deadline"));
        }
        let mut data = Vec::new();
        let mut buffer = vec![0; 65536];
        loop {
            if Instant::now() >= end {
                return Err(failure("stage evidence read expired"));
            }
            let count = file.read(&mut buffer)?;
            if Instant::now() >= end {
                return Err(failure("stage evidence read completed late"));
            }
            if count == 0 {
                break;
            }
            if data
                .len()
                .checked_add(count)
                .is_none_or(|n| u64::try_from(n).map_or(true, |n| n > FILE_BYTES))
            {
                return Err(failure("stage evidence growth bound"));
            }
            data.extend_from_slice(&buffer[..count]);
        }
        let after = file.metadata()?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.uid() != after.uid()
            || before.gid() != after.gid()
            || before.nlink() != after.nlink()
            || (
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec(),
            ) != (
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec(),
            )
            || after.len() != u64::try_from(data.len()).map_err(|_| failure("evidence length"))?
        {
            return Err(failure("stage evidence changed during read"));
        }
        let mut value = json!({"sha256":super::super::files::hex(&Sha256::digest(&data)),
            "bytes":data.len(),"mode":after.mode() & 0o7777});
        let temporary = path
            .file_name()
            .is_some_and(|v| v.to_string_lossy().starts_with(".release-"));
        if temporary {
            if let Some(projection) = complete_atomic_temp(path, &data, root, end)? {
                value["atomicTemp"] = projection;
            } else if let Ok(text) = String::from_utf8(data) {
                // Unknown/partial/invalid temporary prefixes stay exact raw data.
                value["text"] = text.into();
            }
        } else if path.extension().is_some_and(|v| v == "json")
            || path.file_name().is_some_and(|v| v == "validation-name.txt")
        {
            let maximum =
                usize::try_from(FILE_BYTES).map_err(|_| failure("observation JSON bound"))?;
            let semantic = crate::json::decode_limited(&data, maximum)
                .ok()
                .and_then(|_| serde_json::from_slice::<Value>(&data).ok());
            if let Some(mut decoded) = semantic {
                normalize(&mut decoded, root);
                value["value"] = decoded;
            } else if let Ok(text) = String::from_utf8(data) {
                let mut text = Value::String(text);
                if path
                    .file_name()
                    .is_some_and(|name| name == "validation-name.txt")
                {
                    normalize(&mut text, root);
                }
                value["text"] = text;
            }
        }
        Ok(value)
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    observed.and_then(|value| closed.map(|()| value))
}
fn tree(path: &Path, root: &Path, end: Instant) -> io::Result<Value> {
    let mut output = serde_json::Map::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if Instant::now() >= end {
            return Err(failure("stage evidence enumeration expired"));
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = entry.path().symlink_metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(path)
                .map_err(|_| failure("stage evidence spelling"))?
                .to_str()
                .ok_or_else(|| failure("stage fixture UTF8"))?
                .to_owned();
            let value = if metadata.file_type().is_symlink() {
                let target = fs::read_link(entry.path())?;
                let target = target
                    .to_str()
                    .ok_or_else(|| failure("stage link spelling"))?;
                json!({"symlink":target,"mode":metadata.mode() & 0o7777})
            } else {
                pin(&entry.path(), root, end)?
            };
            output.insert(relative, value);
        }
    }
    Ok(Value::Object(output))
}
fn directory_modes(root: &Path, end: Instant) -> io::Result<Value> {
    let mut pending = ["public", "config", "work"]
        .map(|name| root.join(name))
        .to_vec();
    let mut modes = serde_json::Map::new();
    while let Some(path) = pending.pop() {
        if Instant::now() >= end {
            return Err(failure("directory observation deadline"));
        }
        let mut spelling = Value::String(
            path.to_str()
                .ok_or_else(|| failure("directory UTF8"))?
                .into(),
        );
        normalize(&mut spelling, root);
        modes.insert(
            spelling
                .as_str()
                .ok_or_else(|| failure("directory spelling"))?
                .into(),
            (path.symlink_metadata()?.mode() & 0o7777).into(),
        );
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            if entry.path().symlink_metadata()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(Value::Object(modes))
}
fn semantic_tree(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    let mut renamed = serde_json::Map::new();
    for (name, mut fact) in std::mem::take(object) {
        if let Some(fields) = fact.as_object_mut() {
            if let Some(temp) = fields.get("atomicTemp") {
                assert_eq!(temp["complete"], true);
                assert_eq!(temp["ownSerializationMatches"], true);
                assert!(matches!(temp["role"].as_str(), Some("staged" | "outcome")));
                // Raw SHA stays in the independent original tree. Only its
                // cross-writer lexical comparison is omitted in this clone;
                // exact byte count, mode and complete closed value remain.
                fields.remove("sha256");
            } else if fields.contains_key("value")
                || name == "validation-name.txt" && fields.contains_key("text")
            {
                fields.remove("sha256");
                fields.remove("bytes");
            }
        }
        let name = if name.starts_with(".release-") {
            "$ATOMIC_TMP".to_owned()
        } else {
            name
        };
        assert!(
            renamed.insert(name, fact).is_none(),
            "distinct retained temporary prefix"
        );
    }
    *object = renamed;
    value
}
struct Inert<'a> {
    input: &'a Input,
    root: PathBuf,
    calls: Vec<Value>,
    cleanup: Vec<Value>,
    active: Option<&'static str>,
    loaded: bool,
    windows: bool,
}
impl Executor for Inert<'_> {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        let cap = match request.role {
            Role::Load => 300,
            Role::Wait => 20,
            _ => 30,
        };
        // The known daemon cleanup debt is inside original B; M remains
        // reserved for local finalizers. Normal validator roles owe that tail.
        let end = if !request.cleanup
            && matches!(
                request.role,
                Role::Create | Role::Start | Role::Wait | Role::Logs | Role::Remove
            ) {
            owner
                .body_end()
                .checked_sub(Duration::from_secs(40))
                .unwrap()
        } else {
            owner.body_end()
        };
        self.windows &= request.body_end <= owner.now() + Duration::from_secs(cap)
            && request.body_end <= request.settlement_end
            && request.settlement_end <= end
            && request.settlement_end.duration_since(request.body_end) <= Duration::from_secs(10)
            && request.program == self.root.join("bin/docker");
        let arguments = request
            .arguments
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>();
        let Some(arguments) = arguments else {
            owner.record(Cause::Input);
            return CommandOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                status: None,
                error: Some(Cause::Input),
                settled: false,
                stdout_complete: false,
            };
        };
        let mut value = json!(arguments);
        normalize(&mut value, &self.root);
        if request.cleanup {
            self.cleanup.push(value);
        } else {
            self.calls.push(value);
        }
        let role = match request.role {
            Role::Load => "load",
            Role::Inspect => "inspect",
            Role::Create => "create",
            Role::Start => "start",
            Role::Wait => "wait",
            Role::Logs => "logs",
            Role::Remove => "remove",
        };
        if request.role == Role::Load {
            self.loaded = true;
        }
        if request.role == Role::Create {
            self.active = Some(
                if self.input.daemon.create == format!("{}\n", "f".repeat(64)) {
                    "known"
                } else {
                    "unknown"
                },
            );
        }
        let failed = self.input.daemon.fault_role.as_deref() == Some(role);
        if request.role == Role::Remove && !failed {
            self.active = None;
        }
        let stdout = if failed {
            Vec::new()
        } else {
            match request.role {
                Role::Inspect => self.input.daemon.inspect.as_bytes(),
                Role::Create => self.input.daemon.create.as_bytes(),
                Role::Wait => self.input.daemon.wait.as_bytes(),
                Role::Logs => self.input.daemon.logs.as_bytes(),
                _ => &[],
            }
            .to_vec()
        };
        let create_close = request.role == Role::Create
            && matches!(
                self.input.scenario.as_str(),
                "create-close" | "create-close-remove-failure"
            );
        CommandOutput {
            stdout,
            stderr: if failed {
                b"INERT_DAEMON_FAILURE\n".to_vec()
            } else {
                Vec::new()
            },
            status: Some(if failed { 7 } else { 0 }),
            error: create_close.then_some(Cause::Close),
            settled: !create_close,
            stdout_complete: true,
        }
    }
}
struct NativeCase {
    codes: Vec<i32>,
    reports: Vec<Value>,
    attempts: Vec<Value>,
    release: Value,
    state: Value,
    directory_modes: Value,
    config_before: Value,
    config_after: Value,
    link_counts_before: Value,
    link_counts_after: Value,
    inputs_before: Value,
    inputs_after: Value,
    calls: Vec<Value>,
    cleanup: Vec<Value>,
    active: Option<&'static str>,
    loaded: bool,
    windows: bool,
    shared_closes: usize,
    shared_fallback: usize,
    record_closes: usize,
    record_fallback: usize,
    lock_released: bool,
    primary: Option<Cause>,
    owner_finished: bool,
}
fn input_link_counts(root: &Path) -> io::Result<Value> {
    let mut counts = serde_json::Map::new();
    for name in ["image.tar", "release.json"] {
        counts.insert(
            name.into(),
            root.join("public/releases")
                .join("a".repeat(40))
                .join(name)
                .symlink_metadata()?
                .nlink()
                .into(),
        );
    }
    Ok(Value::Object(counts))
}
fn lock_available(path: &Path) -> io::Result<bool> {
    let file = File::options().read(true).write(true).open(path)?;
    let checked = rustix::fs::flock(
        file.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_ok();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    closed.map(|()| checked)
}
async fn native(
    input: &Input,
    root: &Path,
    owner: &mut Owner,
    started: Instant,
) -> io::Result<NativeCase> {
    let end = owner.end();
    let destination = root.join("public/releases").join(&input.revision);
    let inputs_before = tree(&destination, root, end)?;
    let link_counts_before = input_link_counts(root)?;
    let config_before = tree(&root.join("config"), root, end)?;
    let mut executor = Inert {
        input,
        root: root.to_path_buf(),
        calls: Vec::new(),
        cleanup: Vec::new(),
        active: None,
        loaded: false,
        windows: true,
    };
    let held = if input.scenario == "lock-busy" {
        let file = File::options()
            .read(true)
            .write(true)
            .open(root.join("work/workload.lock"))?;
        let acquired = rustix::fs::flock(
            file.as_fd(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        );
        if let Err(error) = acquired {
            let _ = nix::unistd::close(file);
            return Err(error.into());
        }
        Some(file)
    } else {
        None
    };
    let access = Access {
        root: root.join("public"),
        config: root.join("config"),
        work: root.join("work"),
        docker: root.join("bin/docker"),
        socket: None,
        uid: rustix::process::getuid().as_raw(),
        free_bytes: Some(input.free_bytes),
        fault: fault(&input.scenario),
    };
    let options = Options {
        revision: input.revision.clone(),
        whole: WHOLE,
        reserve: RESERVE,
    };
    let mut reports = Vec::new();
    let mut codes = Vec::new();
    super::super::files::start_probe(None);
    files::start_probe();
    for _ in 0..input.repeat {
        let output = execute(&options, &access, owner, &mut executor, started).await;
        codes.push(if output.passed && owner.error().is_none() {
            0
        } else {
            1
        });
        if !output.bytes.is_empty() {
            match serde_json::from_slice::<Value>(&output.bytes) {
                Ok(mut value) => {
                    normalize(&mut value, root);
                    reports.push(value);
                }
                Err(_) => owner.record(Cause::Policy),
            }
        }
    }
    let (shared_closes, shared_fallback) = super::super::files::take_probe();
    let (record_closes, record_fallback) = files::take_probe();
    let closed = held
        .map(nix::unistd::close)
        .transpose()
        .map_err(io::Error::from);
    let _ = closed?;
    let mut attempts = Vec::new();
    for path in fs::read_dir(root.join("public/results"))? {
        let path = path?.path();
        if path.is_dir() {
            attempts.push(tree(&path, root, end)?);
        }
    }
    let release = tree(&destination, root, end)?;
    let inputs_after = Value::Object(
        inputs_before
            .as_object()
            .ok_or_else(|| failure("input observation object"))?
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    release.get(name).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    );
    let state_path = root.join("public/release-state.json");
    let state = if state_path.exists() {
        pin(&state_path, root, end)?
            .get("value")
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(NativeCase {
        codes,
        reports,
        attempts,
        release,
        state,
        directory_modes: directory_modes(root, end)?,
        config_before,
        config_after: tree(&root.join("config"), root, end)?,
        link_counts_before,
        link_counts_after: input_link_counts(root)?,
        inputs_before,
        inputs_after,
        calls: executor.calls,
        cleanup: executor.cleanup,
        active: executor.active,
        loaded: executor.loaded,
        windows: executor.windows,
        shared_closes,
        shared_fallback,
        record_closes,
        record_fallback,
        lock_released: lock_available(&root.join("work/workload.lock"))?,
        primary: owner.error(),
        owner_finished: false,
    })
}
fn observed_count(value: &Value) -> usize {
    value.as_object().map_or(0, serde_json::Map::len)
}
fn failure_class(scenario: &str, reference: &str, native: &str) -> bool {
    if reference == native {
        return true;
    }
    // These closed mappings describe parser/custody and typed IO diagnostics.
    // The full phase, verdict, journals and effects remain separately exact.
    let pair = match scenario {
        "archive-digest" => (
            "Image archive SHA256 does not match release manifest",
            "native_archive_refused",
        ),
        "archive-empty" => (
            "Missing or oversized Docker image metadata",
            "native_archive_refused",
        ),
        "archive-unsafe" => (
            "Noncanonical Docker archive member path",
            "native_archive_refused",
        ),
        "archive-config" => ("Invalid artifact JSON", "native_archive_refused"),
        "archive-duplicate" => (
            "Duplicate or nonregular Docker archive member",
            "native_archive_refused",
        ),
        "archive-layer-missing" => (
            "Docker image layer is missing or nonregular",
            "native_archive_refused",
        ),
        "staged-syntax" => ("JSONDecodeError", "native_policy_refused"),
        "staged-missing-key" => ("KeyError", "native_policy_refused"),
        "staged-mode" | "staged-symlink" => {
            let expected = format!(
                "Unexpected ownership or permissions: $PUBLIC/releases/{}/staged.json",
                "a".repeat(40)
            );
            return reference == expected
                && native
                    == if scenario == "staged-mode" {
                        "native_policy_refused"
                    } else {
                        "OSError"
                    };
        }
        "create-close" | "create-close-remove-failure" => ("OSError", "native_close_refused"),
        "image-environment" => ("RuntimeProfileError", "runtime_environment_override"),
        _ => return false,
    };
    reference == pair.0 && native == pair.1
}
fn reports_equal(scenario: &str, reference: &Value, native: &Value) {
    let mut expected = reference.clone();
    if let (Some(old), Some(new)) = (
        expected.get("failure").and_then(Value::as_str),
        native.get("failure").and_then(Value::as_str),
    ) {
        assert!(
            failure_class(scenario, old, new),
            "{scenario}: exact declared public failure mapping: {old:?} -> {new:?}"
        );
        expected["failure"] = new.into();
    }
    assert_eq!(
        native, &expected,
        "{scenario}: complete report/journal object"
    );
}
fn compare(native: &NativeCase, reference: &Reference) {
    let name = &reference.scenario;
    assert!(exact_hex(&reference.input.config_sha256, 64));
    assert_eq!(reference.input.fault.is_some(), fault(name).is_some());
    if let Some(hash) = &reference.input.graph_manifest_sha256 {
        assert!(exact_hex(hash, 64));
    }
    if matches!(name.as_str(), "oci-config-id" | "oci-manifest-id") {
        let image: Value = serde_json::from_str(&reference.input.daemon.inspect).unwrap();
        let hash = if name == "oci-config-id" {
            &reference.input.config_sha256
        } else {
            reference.input.graph_manifest_sha256.as_ref().unwrap()
        };
        assert_eq!(image["id"], format!("sha256:{hash}"));
    }
    let observed = &reference.observation;
    assert_eq!(reference.schema_version, 1);
    assert_eq!(reference.input.scenario, *name);
    assert_eq!(
        reference.input.repeat,
        if name == "repeat" { 2 } else { 1 },
        "{name}: every requested whole action ran"
    );
    assert_eq!(
        reference.input.native_profile.as_deref(),
        match name.as_str() {
            "daemon-outside-graph" => Some("archive-graph-image-binding"),
            "create-close" | "create-close-remove-failure" => Some("known-create-completion"),
            _ => None,
        }
    );
    assert_eq!(
        observed["workerSettled"],
        json!({"status":0,"pipesClosed":true,"reaped":true,"stderrBytes":0})
    );
    for field in [
        "sourceUnchanged",
        "originalPythonUnchanged",
        "originalBaselinePythonUnchanged",
        "inputUnchanged",
        "configUnchanged",
        "lockReleased",
    ] {
        assert_eq!(observed[field], true, "{name}: {field}");
    }
    let expected = BASELINE_PYTHON_PATHS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let actual = observed["originalBaselinePythonPins"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        expected, actual,
        "{name}: every named original Python source"
    );
    assert_eq!(
        observed["originalBaselinePythonCount"].as_u64(),
        Some(u64::try_from(expected.len()).unwrap())
    );
    assert!(
        observed["originalPythonCount"]
            .as_u64()
            .is_some_and(|n| n >= u64::try_from(expected.len()).unwrap())
    );
    assert_eq!(
        observed_count(&observed["originalBaselinePythonPins"]),
        expected.len()
    );
    for child in observed["children"].as_array().unwrap() {
        assert!(child["status"].is_i64());
        assert_eq!(child["pipesClosed"], true);
        assert_eq!(child["logsClosed"], true);
    }
    assert_eq!(
        native.shared_fallback, 0,
        "{name}: shared handles explicitly closed"
    );
    assert_eq!(
        native.record_fallback, 0,
        "{name}: record handles explicitly closed"
    );
    assert!(
        native.lock_released,
        "{name}: actual workload lock reacquisition"
    );
    assert!(native.windows, "{name}: original phase/end limits");
    assert_eq!(
        native.config_before, native.config_after,
        "{name}: untouched application configuration"
    );
    assert_eq!(
        native.link_counts_before,
        reference.observation["inputLinkCountsBefore"]
    );
    assert_eq!(
        native.link_counts_after, native.link_counts_before,
        "{name}: original pair links unchanged"
    );
    assert_eq!(
        reference.observation["inputLinkCountsAfter"],
        reference.observation["inputLinkCountsBefore"]
    );
    if name == "received-hardlink-pair" {
        assert_eq!(
            native.link_counts_before,
            json!({"image.tar":2,"release.json":2})
        );
    }
    assert_eq!(
        native.inputs_before, native.inputs_after,
        "{name}: unchanged original artifact/staged inputs"
    );
    assert!(
        native.shared_closes > 0,
        "{name}: admitted original handles were explicitly finalized"
    );
    if !native.attempts.is_empty() {
        assert!(
            native.record_closes > 0,
            "{name}: retained records explicitly finalized"
        );
    }
    if matches!(
        name.as_str(),
        "create-close" | "create-close-remove-failure"
    ) {
        assert_eq!(
            native.primary,
            Some(Cause::Close),
            "{name}: known body close stays primary"
        );
    }
    assert_eq!(
        native.directory_modes, observed["directoryModes"],
        "{name}: all product directory effects/modes"
    );
    assert_eq!(native.loaded, observed["daemon"]["loaded"]);
    if reference.input.native_profile.as_deref() == Some("archive-graph-image-binding") {
        assert_eq!(observed["exitCodes"], json!([0]));
        assert_eq!(native.codes, vec![1]);
        assert!(native.primary.is_some());
        assert!(!native.owner_finished);
        assert_eq!(
            native.calls,
            observed["daemon"]["calls"].as_array().unwrap()[..2]
        );
        assert!(native.cleanup.is_empty());
        assert_eq!(native.active, None);
        assert_eq!(
            native.release, native.inputs_before,
            "{name}: no staged selection"
        );
        assert_eq!(
            native.state,
            json!({"schemaVersion":1,"attempt":"$PUBLIC/results/$ATTEMPT",
            "finalized":false,"phase":"import_image"})
        );
        let report = json!({"action":"stage","revision":reference.input.revision,
            "startedAt":"$UTC","finishedAt":"$UTC","passed":false,"phase":"stage",
            "failure":"Imported image differs from verified archive graph",
            "evidence":"$PUBLIC/results/$ATTEMPT"});
        assert_eq!(native.reports, vec![report.clone()]);
        assert_eq!(native.attempts.len(), 1);
        let mut expected = serde_json::Map::new();
        let old = &observed["attempts"][0];
        for name in ["001.stdout", "001.stderr", "002.stdout", "002.stderr"] {
            expected.insert(name.into(), old[name].clone());
        }
        let mut outcome = report;
        outcome.as_object_mut().unwrap().remove("evidence");
        expected.insert("outcome.json".into(), json!({"mode":0o600,"value":outcome}));
        assert_eq!(
            semantic_tree(native.attempts[0].clone()),
            semantic_tree(Value::Object(expected))
        );
        return;
    }
    assert_eq!(
        native.calls,
        observed["daemon"]["calls"].as_array().unwrap().clone(),
        "{name}: exact fixed Docker arguments/order"
    );
    assert_eq!(
        serde_json::to_value(&native.codes).unwrap(),
        observed["exitCodes"],
        "{name}: all final operation statuses"
    );
    assert_eq!(
        native.primary.is_none(),
        native.codes.iter().all(|code| *code == 0),
        "{name}: original typed owner authority"
    );
    assert_eq!(
        native.owner_finished,
        native.codes.iter().all(|code| *code == 0),
        "{name}: explicit signal-owner completion"
    );
    let reports = observed["reports"].as_array().unwrap();
    assert_eq!(
        native.reports.len(),
        reports.len(),
        "{name}: final report count"
    );
    for (actual, expected) in native.reports.iter().zip(reports) {
        reports_equal(name, expected, actual);
    }
    assert_eq!(
        native.attempts.len(),
        observed["attempts"].as_array().unwrap().len(),
        "{name}: retained attempt count"
    );
    // Attempt names carry unrelated entropy. Compare their complete retained
    // multisets while reports and Docker calls retain exact chronological order.
    let mut actual_attempts = native.attempts.clone();
    let mut expected_attempts = observed["attempts"].as_array().unwrap().clone();
    actual_attempts
        .sort_by_cached_key(|value| serde_json::to_string(&semantic_tree(value.clone())).unwrap());
    expected_attempts
        .sort_by_cached_key(|value| serde_json::to_string(&semantic_tree(value.clone())).unwrap());
    for (actual, expected) in actual_attempts.iter().zip(expected_attempts.iter()) {
        let mut actual = semantic_tree(actual.clone());
        let mut expected = semantic_tree(expected.clone());
        if !native.cleanup.is_empty() {
            let prefix = format!("{:03}", native.calls.len() + 1);
            let empty = super::super::files::hex(&Sha256::digest([]));
            assert_eq!(
                actual
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("{prefix}.stdout")),
                Some(json!({"sha256":empty,"bytes":0,"mode":0o600})),
                "{name}: exact additional cleanup stdout"
            );
            let stderr = if matches!(
                name.as_str(),
                "remove-failure" | "create-close-remove-failure"
            ) {
                b"INERT_DAEMON_FAILURE\n".as_slice()
            } else {
                b"".as_slice()
            };
            assert_eq!(
                actual
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("{prefix}.stderr")),
                Some(
                    json!({"sha256":super::super::files::hex(&Sha256::digest(stderr)),
                    "bytes":stderr.len(),"mode":0o600})
                ),
                "{name}: exact additional cleanup stderr"
            );
        }
        if let (Some(a), Some(b)) = (
            actual.get("outcome.json").and_then(|v| v.get("value")),
            expected.get("outcome.json").and_then(|v| v.get("value")),
        ) {
            reports_equal(name, b, a);
            expected["outcome.json"]["value"] = a.clone();
        }
        assert_eq!(
            actual, expected,
            "{name}: complete retained files/modes/prefixes"
        );
    }
    assert_eq!(
        semantic_tree(native.release.clone()),
        semantic_tree(observed["releaseFiles"].clone()),
        "{name}: complete release tree/staged record"
    );
    assert_eq!(
        native.state, observed["releaseState"],
        "{name}: exact final persistent journal"
    );
    if native.cleanup.is_empty() {
        assert_eq!(
            native.active,
            observed["daemon"]["activeValidator"].as_str(),
            "{name}: original daemon effect"
        );
    } else {
        assert_eq!(
            native.cleanup.len(),
            1,
            "{name}: only exact known validator cleanup"
        );
        assert_eq!(
            native.cleanup[0],
            json!([
                "--host",
                "unix:///var/run/docker.sock",
                "rm",
                "--force",
                "f".repeat(64)
            ])
        );
        assert_eq!(
            observed["daemon"]["activeValidator"], "known",
            "{name}: original retained effect explicitly differs"
        );
        if matches!(
            name.as_str(),
            "remove-failure" | "create-close-remove-failure"
        ) {
            assert_eq!(native.active, Some("known"));
        } else {
            assert_eq!(native.active, None);
        }
    }
    let writes = observed["writes"].as_array().unwrap();
    for write in writes {
        let outcome = write["path"].as_str().unwrap().ends_with("/outcome.json");
        assert_eq!(
            write["lockHeld"], !outcome,
            "{name}: original outcome after real unlock"
        );
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pinned controller Python; run explicitly by operations automation"]
async fn whole_stage_matches_unchanged_python() {
    assert!(!SCENARIOS.is_empty());
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    for scenario in SCENARIOS {
        let fixture = Fixture::new();
        let started = Instant::now();
        let mut owner = Owner::new(started, WHOLE, RESERVE).unwrap();
        let python = process::run(
            &mut owner,
            process::Invocation {
                program: prefix.join("bin/python3"),
                arguments: vec![
                    "-B".into(),
                    repository
                        .join("ops/ansible/tests/release_stage_oracle.py")
                        .into_os_string(),
                    "--scenario".into(),
                    (*scenario).into(),
                    "--fixture-directory".into(),
                    fixture.0.as_os_str().into(),
                ],
                environment: BTreeMap::from([
                    ("PATH".into(), prefix.join("bin").into_os_string()),
                    ("LC_ALL".into(), "C".into()),
                    ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                ]),
                body_end: started + PYTHON_BODY,
                settlement_end: started + PYTHON_BODY + PYTHON_TAIL,
                cleanup: false,
            },
        )
        .await;
        let reference = serde_json::from_slice::<Reference>(&python.stdout);
        let observed = if python.result.is_ok() {
            match &reference {
                Ok(reference) => {
                    native(
                        &reference.input,
                        &fixture.0.join("native"),
                        &mut owner,
                        started,
                    )
                    .await
                }
                Err(_) => Err(failure("whole original stage observation malformed")),
            }
        } else {
            Err(failure("whole original stage child refused"))
        };
        let primary = owner.error();
        let settled = owner.finish();
        // Every original producer pipe/group and every native file/lock owner
        // has completed before these assertions or fallible-result unwraps.
        assert!(
            python.result.is_ok(),
            "{scenario}: original child {primary:?}"
        );
        let proof = &python.observation;
        assert_eq!(proof.exit_status, Some(0));
        assert!(proof.reaped && proof.group_retired);
        assert!(proof.stdin_closed && proof.stdout_closed && proof.stderr_closed);
        assert!(proof.stdout_eof && proof.stderr_eof);
        assert!(python.stderr.is_empty());
        let reference = reference.unwrap();
        let mut observed = observed.unwrap();
        observed.owner_finished = settled.is_ok();
        compare(&observed, &reference);
    }
}
