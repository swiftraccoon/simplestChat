"""Run the unchanged complete stage action with real files and owned inert Docker children.

The original CLI, archive validator, Runner, flock, atomic writes, fsync and
outcome-after-unlock ordering remain in the path. Only the fixed daemon,
root identity, fixture clock/UUID and named lower IO faults are adapted.
No engine, network, application or host service may be used.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import io
import json
import os
import re
import shutil
import signal
import stat
import sys
import tarfile
import tempfile
import uuid
from contextlib import ExitStack, redirect_stderr, redirect_stdout, suppress
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

import release_fetch_controller_oracle as census
from test_support import ROOT, obj, string

# isort: split
import bounded_process
import release_public as public
from release_json import JsonObject, JsonValue, decode_json

if TYPE_CHECKING:
    import subprocess
    from collections.abc import Sequence
    from typing import BinaryIO, Unpack

REVISION = "a" * 40
TAG = "simplestchat-release/production:" + REVISION
CONTAINER = "f" * 64
UTC = "2050-01-01T00:00:00Z"
UUID = "b" * 32
MAX_FILE = 4 * 1024**2
MAX_OUTPUT = 2 * 1024**2
WORKER_SECONDS = 45
RESERVE_BYTES = 1024**3
MIGRATION = "SELECT 1;\n"
CHECKSUM = hashlib.sha384(MIGRATION.encode()).hexdigest()
SCENARIOS = (
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
)
POSITIVE = {
    "received-hardlink-pair",
    "oci-config-id",
    "oci-manifest-id",
    "tool-whitespace",
    "success",
    "repeat",
    "staged-repeat",
    "disk-plus",
    "daemon-outside-graph",
}
REFERENCE_PATHS = (
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/tests/test_support.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
)
OWN_PATHS = {
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/test_release_stage_oracle.py",
}
# Explicit names from the exact receiver-composite source manifest. Future
# maintained sources remain in the full before/after census and cannot replace
# a missing original merely by keeping an aggregate total high.
BASELINE_PYTHON_PATHS: tuple[str, ...] = (
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
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/"
    + "test_release_container_harness.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
)


def encoded(value: object) -> bytes:
    """Emit ordinary finite JSON, never a private exception representation."""
    return json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def digest(data: bytes) -> str:
    """Bind original fixture bytes without executing an image layer."""
    return hashlib.sha256(data).hexdigest()


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create only a fixed exclusive private fixture leaf."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        if output.write(data) != len(data):
            message = "incomplete stage fixture write"
            raise OSError(message)
        output.flush()
        os.fchmod(output.fileno(), mode)


def directory(path: Path) -> None:
    """Create and explicitly set the intended private directory mode."""
    path.mkdir(mode=0o700)
    path.chmod(0o700)


def pin(path: Path) -> JsonObject:
    """Measure a bounded original regular file without following a final alias."""
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE:
        message = "unexpected stage oracle input"
        raise ValueError(message)
    data = path.read_bytes()
    return {"sha256": digest(data), "bytes": len(data), "mode": stat.S_IMODE(metadata.st_mode)}


def python_pins() -> JsonObject:
    """Measure the complete maintained set, excluding only this new oracle pair."""
    paths = {path for name in census.PYTHON_ROOTS for path in (ROOT / name).rglob("*.py")}
    paths.update(ROOT / name for name in census.PYTHON_VENDOR_PATHS)
    return {
        path.relative_to(ROOT).as_posix(): pin(path)
        for path in sorted(paths)
        if path.relative_to(ROOT).as_posix() not in OWN_PATHS
    }


def baseline_pins(values: JsonObject) -> JsonObject:
    """Keep each named original, independently of the maintained aggregate count."""
    if any(name not in values for name in BASELINE_PYTHON_PATHS):
        message = "original stage baseline source missing"
        raise ValueError(message)
    return {name: values[name] for name in BASELINE_PYTHON_PATHS}


def normalized(value: JsonValue, root: Path) -> JsonValue:
    """Remove fixture spelling and clock entropy while retaining complete JSON fields."""
    if isinstance(value, dict):
        return {
            key: (
                "$UTC" if key in {"startedAt", "finishedAt", "stagedAt"} else normalized(item, root)
            )
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [normalized(item, root) for item in value]
    if isinstance(value, str):
        value = value.replace(str(root / "public"), "$PUBLIC")
        value = value.replace(str(root / "config"), "$CONFIG")
        value = value.replace(str(root / "work"), "$WORK")
        value = value.replace(str(root), "$FIXTURE")
        value = re.sub(r"(?<=/results/)release\.[A-Za-z0-9_-]+", "$ATTEMPT", value)
        return value.replace("scpub-release-validate-" + UUID, "$VALIDATOR")
    return value


def archive_bytes(scenario: str) -> tuple[bytes, str, str | None]:
    """Create an ordinary deterministic Docker archive consumed by the actual validator."""
    config: JsonObject = {
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "User": "10001:10001",
            "Cmd": ["/app/simplestChat"],
            "Entrypoint": None,
            "Labels": {"org.opencontainers.image.revision": REVISION},
        },
    }
    config_data = encoded(config).rstrip(b"\n")
    if scenario == "archive-config":
        config_data = b"{"
    members = [
        (
            "manifest.json",
            encoded([{"Config": "config.json", "RepoTags": [TAG], "Layers": ["layer.tar"]}]).rstrip(
                b"\n"
            ),
        ),
        ("config.json", config_data),
        ("layer.tar", b"INERT_LAYER_BYTES_NOT_EXECUTED"),
    ]
    if scenario == "archive-empty":
        members = []
    if scenario == "archive-unsafe":
        members.append(("../escape", b"INERT"))
    if scenario == "archive-duplicate":
        members.append(("config.json", config_data))
    if scenario == "archive-layer-missing":
        members = [(name, data) for name, data in members if name != "layer.tar"]
    graph_manifest: str | None = None
    if scenario in {"oci-config-id", "oci-manifest-id"}:
        layer = b"INERT_LAYER_BYTES_NOT_EXECUTED"
        layer_hash = digest(layer)
        config_hash = digest(config_data)
        config_descriptor: JsonObject = {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:" + config_hash,
            "size": len(config_data),
        }
        layer_descriptor: JsonObject = {
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": "sha256:" + layer_hash,
            "size": len(layer),
        }
        image_data = encoded(
            {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": config_descriptor,
                "layers": [layer_descriptor],
            }
        )
        graph_manifest = digest(image_data)
        image_descriptor: JsonObject = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:" + graph_manifest,
            "size": len(image_data),
            "annotations": {"org.opencontainers.image.ref.name": TAG},
        }
        config_name, layer_name = "blobs/sha256/" + config_hash, "blobs/sha256/" + layer_hash
        members = [
            (
                "manifest.json",
                encoded([{"Config": config_name, "RepoTags": [TAG], "Layers": [layer_name]}]),
            ),
            ("oci-layout", encoded({"imageLayoutVersion": "1.0.0"})),
            ("index.json", encoded({"schemaVersion": 2, "manifests": [image_descriptor]})),
            ("blobs/sha256/" + graph_manifest, image_data),
            (config_name, config_data),
            (layer_name, layer),
        ]
    sink = io.BytesIO()
    with tarfile.open(fileobj=sink, mode="w", format=tarfile.USTAR_FORMAT) as builder:
        for name, data in members:
            member = tarfile.TarInfo(name)
            member.size, member.mode, member.uid, member.gid = len(data), 0o600, 0, 0
            builder.addfile(member, io.BytesIO(data))
    return sink.getvalue(), digest(config_data), graph_manifest


def fixture(  # noqa: C901, PLR0912, PLR0915 -- Finite lower fixture mutations.
    scenario: str,
) -> tuple[JsonObject, bytes, bytes]:
    """Select finite lower observations and original input bytes for both implementations."""
    if scenario not in SCENARIOS:
        message = "unknown stage oracle scenario"
        raise ValueError(message)
    archive, config_hash, graph_manifest = archive_bytes(scenario)
    image = "sha256:" + (
        graph_manifest
        if scenario == "oci-manifest-id" and graph_manifest is not None
        else config_hash
    )
    manifest: JsonObject = {
        "schemaVersion": 1,
        "revision": REVISION,
        "platform": "linux/amd64",
        "archiveSha256": digest(archive),
        "imageTag": TAG,
        "migrations": {"1": CHECKSUM},
        "createdAt": UTC,
    }
    if scenario == "manifest-extra-key":
        manifest["extra"] = True
    if scenario == "manifest-revision":
        manifest["revision"] = "b" * 40
    if scenario == "manifest-platform":
        manifest["platform"] = "linux/arm64"
    if scenario == "manifest-migrations":
        manifest["migrations"] = {}
    if scenario == "manifest-created":
        manifest["createdAt"] = "invalid"
    if scenario == "archive-digest":
        manifest["archiveSha256"] = "0" * 64
    if scenario == "tool-whitespace":
        manifest["migrations"] = {"1": CHECKSUM, "2": "e" * 96}
    manifest_data = encoded(manifest)
    if scenario == "manifest-syntax":
        manifest_data = b"{"
    if scenario == "manifest-duplicate":
        manifest_data = manifest_data.replace(
            b'"schemaVersion":1', b'"schemaVersion":1,"schemaVersion":1'
        )
    inspect: JsonObject = {
        "id": image,
        "os": "linux",
        "architecture": "amd64",
        "user": "10001:10001",
        "labels": {"org.opencontainers.image.revision": REVISION},
        "cmd": ["/app/simplestChat"],
        "entrypoint": None,
        "workingDir": "/app",
        "hasHealthcheck": False,
        "envNames": ["PATH"],
    }
    changes: dict[str, tuple[str, JsonValue]] = {
        "daemon-outside-graph": ("id", "sha256:" + "c" * 64),
        "image-id": ("id", "wrong"),
        "image-os": ("os", "darwin"),
        "image-architecture": ("architecture", "arm64"),
        "image-user": ("user", "0:0"),
        "image-label": ("labels", {}),
        "image-cmd": ("cmd", ["/bin/sh"]),
        "image-entrypoint": ("entrypoint", ["/bin/sh"]),
        "image-workdir": ("workingDir", "/"),
        "image-healthcheck": ("hasHealthcheck", True),
        "image-environment": ("envNames", ["LD_PRELOAD"]),
    }
    if scenario in changes:
        key, value = changes[scenario]
        inspect[key] = value
    lines = CHECKSUM + "  /app/migrations/1_fixture.sql\n"
    if scenario == "logs-invalid":
        lines = "invalid\n"
    if scenario == "logs-empty":
        lines = ""
    if scenario == "logs-duplicate":
        lines += CHECKSUM + "  /app/migrations/01_other.sql\n"
    if scenario == "logs-mismatch":
        lines = "0" * 96 + "  /app/migrations/1_fixture.sql\n"
    if scenario == "tool-whitespace":
        lines = (
            CHECKSUM
            + "  /app/migrations/001_initial.sql\r\n"
            + "e" * 96
            + "  /app/migrations/002_second.sql\u0085"
        )
    if scenario == "logs-wide-version":
        lines = CHECKSUM + "  /app/migrations/184467440737095516160_wide.sql\n"
    free = len(archive) * 2 + RESERVE_BYTES + 1
    if scenario == "disk-below":
        free -= 2
    if scenario == "disk-exact":
        free -= 1
    fault = {
        "import-journal-write": "release-state.json#import_image",
        "validate-journal-write": "release-state.json#validate_image",
        "validated-journal-write": "release-state.json#image_validated",
        "final-journal-write": "release-state.json#staged",
        "validation-name-write": "validation-name.txt",
        "staged-write": "staged.json",
        "outcome-write": "outcome.json",
        "staged-file-sync": "staged.json#file-sync",
        "staged-directory-sync": "staged.json#directory-sync",
        "outcome-file-sync": "outcome.json#file-sync",
        "outcome-directory-sync": "outcome.json#directory-sync",
    }.get(scenario)
    control: JsonObject = {
        "scenario": scenario,
        "revision": REVISION,
        "repeat": 2 if scenario == "repeat" else 1,
        "freeBytes": free,
        "configSha256": config_hash,
        "graphManifestSha256": graph_manifest,
        "fault": fault,
        "daemon": {
            "inspect": encoded(inspect).decode(),
            "create": "invalid\n" if scenario == "create-invalid" else CONTAINER + "\n",
            "wait": "7\n" if scenario == "wait-nonzero" else "0\n",
            "logs": lines,
            "faultRole": scenario.removesuffix("-failure")
            if scenario.endswith("-failure")
            else None,
        },
    }
    if scenario == "tool-whitespace":
        obj(control, "daemon")["create"] = "\u001c" + CONTAINER + "\u001f\n"
        obj(control, "daemon")["wait"] = "\u000b0\u000b"
    if scenario == "staged-image-missing":
        obj(control, "daemon")["faultRole"] = "inspect"
    if scenario == "daemon-outside-graph":
        control["nativeProfile"] = "archive-graph-image-binding"
    if scenario in {"create-close", "create-close-remove-failure"}:
        control["nativeProfile"] = "known-create-completion"
        obj(control, "daemon")["faultRole"] = "remove" if scenario.endswith("-failure") else None
    return control, manifest_data, archive


def populate(  # noqa: C901, PLR0912 -- Closed original host layout variants.
    root: Path, control: JsonObject, manifest: bytes, archive: bytes
) -> None:
    """Create the same retained initial host layout for reference and native execution."""
    scenario = string(control, "scenario")
    for name in ("public", "config", "work", "bin"):
        directory(root / name)
    for name in ("releases", "results"):
        directory(root / "public" / name)
    selected = root / "public/releases" / REVISION
    directory(selected)
    write(selected / "release.json", manifest)
    write(selected / "image.tar", archive)
    if scenario == "received-hardlink-pair":
        directory(root / "received")
        for name in ("image.tar", "release.json"):
            os.link(selected / name, root / "received" / name, follow_symlinks=False)
    write(root / "config/app.env", b"UNCHANGED_PRIVATE_APP_ENV\n")
    write(root / "config/compose.public.yml", b"UNCHANGED_PRIVATE_COMPOSE\n")
    write(root / "config/images.json", b"{}\n")
    write(root / "work/workload.lock", b"")
    write(root / "control.json", encoded(control))
    daemon: JsonObject = {"calls": [], "activeValidator": None, "loaded": False}
    write(root / "daemon.json", encoded(daemon))
    wrapper = (
        "#!"
        + sys.executable
        + "\nimport os,sys\n"
        + "os.execv("
        + repr(sys.executable)
        + ",["
        + repr(sys.executable)
        + ",'-B',"
        + repr(str(Path(__file__).resolve()))
        + ",'--daemon',"
        + repr(str(root / "daemon.json"))
        + ",'--',*sys.argv[1:]])\n"
    )
    write(root / "bin/docker", wrapper.encode(), 0o700)
    modes = {
        "root-mode": root / "public",
        "config-mode": root / "config",
        "releases-mode": root / "public/releases",
        "revision-mode": selected,
        "results-mode": root / "public/results",
        "manifest-mode": selected / "release.json",
        "archive-mode": selected / "image.tar",
        "lock-mode": root / "work/workload.lock",
    }
    if scenario in modes:
        modes[scenario].chmod(0o755)
    for name in ("manifest", "archive"):
        if scenario == name + "-symlink":
            path = selected / ("release.json" if name == "manifest" else "image.tar")
            saved = path.with_suffix(path.suffix + ".original")
            _ = path.rename(saved)
            path.symlink_to(saved.name)
    if scenario in {"current-unfinished", "release-unfinished", "journal-schema-bool"}:
        target = (
            root / "work/current.json"
            if scenario != "release-unfinished"
            else root / "public/release-state.json"
        )
        write(
            target,
            encoded(
                {
                    "schemaVersion": True if scenario == "journal-schema-bool" else 1,
                    "finalized": False,
                }
            ),
        )
    if scenario in {
        "staged-repeat",
        "staged-manifest-diff",
        "staged-image-missing",
        "staged-symlink",
        "staged-mode",
        "staged-syntax",
        "staged-missing-key",
    }:
        prior: JsonObject = {
            "schemaVersion": 1,
            "revision": REVISION,
            "serverImage": "sha256:" + string(control, "configSha256"),
            "manifestSha256": digest(manifest),
            "stagedAt": UTC,
        }
        if scenario == "staged-manifest-diff":
            prior["manifestSha256"] = "0" * 64
        if scenario == "staged-missing-key":
            del prior["serverImage"]
        write(selected / "staged.json", b"{" if scenario == "staged-syntax" else encoded(prior))
        if scenario == "staged-mode":
            (selected / "staged.json").chmod(0o644)
        if scenario == "staged-symlink":
            _ = (selected / "staged.json").rename(selected / "staged.original")
            (selected / "staged.json").symlink_to("staged.original")


def daemon(path: Path, arguments: list[str]) -> int:
    """Model only fixed Docker effects; each invocation is a real owned inert child."""
    root = path.parent
    state = obj(decode_json(path.read_bytes()))
    control = obj(decode_json((root / "control.json").read_bytes()))
    supplied = obj(control, "daemon")
    if arguments[:2] != ["--host", "unix:///var/run/docker.sock"]:
        message = "inert Docker endpoint changed"
        raise ValueError(message)
    args = arguments[2:]
    if args[:2] == ["image", "load"]:
        role = "load"
        state["loaded"] = True
    elif args[:2] == ["image", "inspect"]:
        role = "inspect"
    elif args and args[0] in {"create", "start", "wait", "logs", "rm"}:
        role = "remove" if args[0] == "rm" else args[0]
    else:
        message = "unknown inert Docker command"
        raise ValueError(message)
    cast("list[JsonValue]", state["calls"]).append(
        normalized(cast("list[JsonValue]", arguments), root)
    )
    if role == "create":
        state["activeValidator"] = "known" if supplied["create"] == CONTAINER + "\n" else "unknown"
    failed = supplied.get("faultRole") == role
    if role == "remove" and not failed:
        state["activeValidator"] = None
    _ = path.write_bytes(encoded(state))
    if failed:
        _ = sys.stderr.buffer.write(b"INERT_DAEMON_FAILURE\n")
        return 7
    output = supplied.get(role, "")
    if not isinstance(output, str):
        message = "unexpected inert Docker response"
        raise TypeError(message)
    _ = sys.stdout.buffer.write(output.encode())
    _ = sys.stdout.buffer.flush()
    return 0


def lock_available(path: Path) -> bool:
    """Probe the real workload lock without relying on a report's settlement claim."""
    descriptor = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        return True
    finally:
        os.close(descriptor)


def atomic_temp_projection(path: Path, data: bytes, root: Path) -> JsonObject | None:
    """Project only a full known private record, keeping its independent original raw pin."""
    if not re.fullmatch(r"\.release-[A-Za-z0-9_-]{1,64}", path.name) or len(data) > MAX_FILE:
        return None
    control = obj(decode_json((root / "control.json").read_bytes()))
    scenario = control["scenario"]
    staged = scenario == "staged-file-sync" and path.parent == root / "public/releases" / REVISION
    outcome = (
        scenario == "outcome-file-sync"
        and path.parent.parent == root / "public/results"
        and path.parent.name.startswith("release.")
    )
    if not (staged or outcome):
        return None
    try:
        value = obj(decode_json(data))
    except ValueError:
        return None
    # This compares actual observed bytes with this reference writer's own
    # complete framing/serializer; a valid JSON prefix missing its LF is not full.
    if data != (json.dumps(value, indent=2) + "\n").encode():
        return None
    if staged:
        fields = {"schemaVersion", "revision", "serverImage", "manifestSha256", "stagedAt"}
        inspection = obj(decode_json(string(control, "daemon", "inspect")))
        manifest = root / "public/releases" / REVISION / "release.json"
        complete = (
            set(value) == fields
            and type(value.get("schemaVersion")) is int
            and value.get("schemaVersion") == 1
            and value.get("serverImage") == inspection["id"]
            and value.get("manifestSha256") == pin(manifest)["sha256"]
            and value.get("stagedAt") == UTC
        )
    else:
        fields = {"action", "revision", "startedAt", "passed", "phase", "finishedAt"}
        complete = (
            set(value) == fields
            and value.get("action") == "stage"
            and value.get("passed") is True
            and value.get("phase") == "complete"
            and all(value.get(key) == UTC for key in ("startedAt", "finishedAt"))
        )
    if not complete or value.get("revision") != REVISION:
        return None
    return {
        "role": "staged" if staged else "outcome",
        "complete": True,
        "ownSerializationMatches": True,
        "value": normalized(value, root),
    }


def inventory(path: Path, root: Path) -> JsonObject:
    """Retain every ordinary file, mode, byte pin and semantic journal in a fixed tree."""
    result: JsonObject = {}
    for member in sorted(path.rglob("*")):
        name = member.relative_to(path).as_posix()
        metadata = member.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            result[name] = {
                "symlink": str(member.readlink()),
                "mode": stat.S_IMODE(metadata.st_mode),
            }
        elif stat.S_ISREG(metadata.st_mode):
            item = pin(member)
            if member.name.startswith(".release-"):
                data = member.read_bytes()
                if len(data) != item["bytes"] or digest(data) != item["sha256"]:
                    message = "atomic temporary changed during observation"
                    raise ValueError(message)
                projection = atomic_temp_projection(member, data, root)
                if projection is not None:
                    item["atomicTemp"] = projection
                else:
                    with suppress(UnicodeError):
                        item["text"] = data.decode("utf-8")
            elif member.suffix == ".json" or member.name == "validation-name.txt":
                try:
                    item["value"] = normalized(decode_json(member.read_bytes()), root)
                except ValueError:
                    text = member.read_text()
                    item["text"] = (
                        normalized(text, root) if member.name == "validation-name.txt" else text
                    )
            result[name] = item
    return result


def directory_modes(root: Path) -> JsonObject:
    """Observe every product directory, including an unexpectedly empty new leaf."""
    paths = [root / name for name in ("public", "config", "work")]
    paths.extend(
        path
        for parent in tuple(paths)
        for path in parent.rglob("*")
        if path.is_dir() and not path.is_symlink()
    )
    return {
        str(normalized(str(path), root)): stat.S_IMODE(path.lstat().st_mode)
        for path in sorted(paths)
    }


def input_link_counts(root: Path) -> JsonObject:
    """Measure the original pair's actual link domain before and after staging."""
    return {
        name: (root / "public/releases" / REVISION / name).lstat().st_nlink
        for name in ("image.tar", "release.json")
    }


def worker(root: Path) -> JsonObject:  # noqa: C901, PLR0915 -- Whole owner and named faults.
    """Execute actual main/Runner, restoring worker-only global state before returning facts."""
    control = obj(decode_json((root / "control.json").read_bytes()))
    free = cast("int", control["freeBytes"])
    output, error = io.StringIO(), io.StringIO()
    writes: list[JsonValue] = []
    commands: list[JsonValue] = []
    processes: list[tuple[subprocess.Popen[bytes], BinaryIO, BinaryIO]] = []
    original_pump, original_atomic, original_fsync = bounded_process.pump, public.atomic, os.fsync
    original_runner = public.Runner.run
    active: list[str] = []
    faulted = False

    def pump(
        child: subprocess.Popen[bytes],
        out: BinaryIO,
        err: BinaryIO,
        limits: bounded_process.Limits,
        *,
        input_data: bytes = b"",
    ) -> int:
        processes.append((child, out, err))
        commands.append(
            {"timeout": limits.timeout, "stdoutLimit": limits.stdout, "stderrLimit": limits.stderr}
        )
        return original_pump(child, out, err, limits, input_data=input_data)

    def command(
        runner: public.Runner, args: Sequence[str], **options: Unpack[public.CommandOptions]
    ) -> bytes:
        result = original_runner(runner, args, **options)
        if control["scenario"] in {"create-close", "create-close-remove-failure"} and args[3:4] == [
            "create"
        ]:
            message = "INERT_COMPLETE_CREATE_CLOSE_FAILURE"
            raise OSError(message)
        return result

    def atomic(path: Path, data: JsonValue | bytes) -> None:
        nonlocal faulted
        phase = str(data.get("phase", "")) if isinstance(data, dict) else ""
        label = path.name + ("#" + phase if phase else "")
        active.append(path.name)
        try:
            if not faulted and control["fault"] in {label, path.name}:
                faulted = True
                message = "INERT_ATOMIC_WRITE_FAILURE"
                raise OSError(message)
            original_atomic(path, data)
            observed = normalized(data.decode() if isinstance(data, bytes) else data, root)
            writes.append(
                {
                    "path": normalized(str(path), root),
                    "value": observed,
                    "lockHeld": not lock_available(root / "work/workload.lock"),
                }
            )
        finally:
            _ = active.pop()

    def fsync(descriptor: int) -> None:
        nonlocal faulted
        kind = "directory-sync" if stat.S_ISDIR(os.fstat(descriptor).st_mode) else "file-sync"
        if active and not faulted and control["fault"] == active[-1] + "#" + kind:
            faulted = True
            message = "INERT_ATOMIC_SYNC_FAILURE"
            raise OSError(message)
        original_fsync(descriptor)

    held: BinaryIO | None = None
    codes: list[JsonValue] = []
    previous_umask = os.umask(0o077)
    handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
    try:
        if control["scenario"] == "lock-busy":
            held = (root / "work/workload.lock").open("r+b")
            fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        with (
            ExitStack() as stack,
            redirect_stdout(output),
            redirect_stderr(error),
            patch.object(public, "ROOT", root / "public"),
            patch.object(public, "CONFIG", root / "config"),
            patch.object(public, "WORK", root / "work"),
            patch.object(public, "ROOT_UID", os.getuid()),
            patch.object(os, "geteuid", return_value=0),
            patch.object(
                public,
                "DOCKER",
                [str(root / "bin/docker"), "--host", "unix:///var/run/docker.sock"],
            ),
            patch.object(public, "timestamp", return_value=UTC),
            patch.object(uuid, "uuid4", return_value=SimpleNamespace(hex=UUID)),
            patch.object(shutil, "disk_usage", return_value=SimpleNamespace(free=free)),
            patch.object(public, "atomic", side_effect=atomic),
            patch.object(os, "fsync", side_effect=fsync),
            patch.object(bounded_process, "pump", side_effect=pump),
            patch.object(public.Runner, "run", new=command),
        ):
            _ = stack.enter_context(
                patch.object(sys, "argv", ["release-public.py", "stage", REVISION])
            )
            codes.extend(public.cli() for _ in range(cast("int", control["repeat"])))
    finally:
        if held is not None:
            held.close()
        for number, handler in handlers.items():
            _ = signal.signal(number, handler)
        _ = os.umask(previous_umask)
    reports = [normalized(decode_json(line), root) for line in output.getvalue().splitlines()]
    child_facts: list[JsonValue] = [
        {
            "status": child.returncode,
            "pipesClosed": all(
                stream is None or stream.closed
                for stream in (child.stdin, child.stdout, child.stderr)
            ),
            "logsClosed": out.closed and err.closed,
        }
        for child, out, err in processes
    ]
    return {
        "exitCodes": codes,
        "reports": reports,
        "stderr": error.getvalue(),
        "writes": writes,
        "commandLimits": commands,
        "children": child_facts,
        "lockReleased": lock_available(root / "work/workload.lock"),
        "daemon": obj(decode_json((root / "daemon.json").read_bytes())),
    }


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Run a complete isolated original and export separate pristine native inputs."""
    if fixture_directory is None:
        with tempfile.TemporaryDirectory(prefix="stage-oracle.") as name:
            return run(scenario, Path(name).resolve())
    root = fixture_directory
    if not root.is_absolute() or root.resolve() != root or any(root.iterdir()):
        message = "stage fixture must be fresh and canonical"
        raise ValueError(message)
    root.chmod(0o700)
    before: JsonObject = {name: pin(ROOT / name) for name in REFERENCE_PATHS}
    sources = python_pins()
    originals = baseline_pins(sources)
    control, manifest, archive = fixture(scenario)
    for label in ("native", "reference"):
        directory(root / label)
        populate(root / label, control, manifest, archive)
    inputs_before = inventory(root / "reference/public/releases" / REVISION, root / "reference")
    config_before = inventory(root / "reference/config", root / "reference")
    links_before = input_link_counts(root / "reference")
    code, output, error = bounded_process.run(
        [
            sys.executable,
            "-B",
            str(Path(__file__).resolve()),
            "--reference-worker",
            str(root / "reference"),
        ],
        limits=bounded_process.Limits(timeout=WORKER_SECONDS, stdout=MAX_OUTPUT, stderr=65536),
        env={"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"},
    )
    if code != 0 or error:
        message = "closed original stage worker failed"
        raise ValueError(message)
    observation = obj(decode_json(output))
    after_sources = python_pins()
    current_inputs = inventory(root / "reference/public/releases" / REVISION, root / "reference")
    inputs_after = {name: current_inputs.get(name) for name in inputs_before}
    config_after = inventory(root / "reference/config", root / "reference")
    extra: JsonObject = {
        "attempts": [
            inventory(path, root / "reference")
            for path in sorted((root / "reference/public/results").glob("release.*"))
        ],
        "releaseFiles": inventory(
            root / "reference/public/releases" / REVISION, root / "reference"
        ),
        "releaseState": normalized(
            decode_json((root / "reference/public/release-state.json").read_bytes()),
            root / "reference",
        )
        if (root / "reference/public/release-state.json").exists()
        else None,
        "directoryModes": directory_modes(root / "reference"),
        "inputLinkCountsBefore": links_before,
        "inputLinkCountsAfter": input_link_counts(root / "reference"),
        "configPinsBefore": config_before,
        "configPinsAfter": config_after,
        "configUnchanged": config_before == config_after,
        "inputPinsBefore": inputs_before,
        "inputPinsAfter": inputs_after,
        "inputUnchanged": inputs_before == inputs_after,
        "sourceUnchanged": before == {name: pin(ROOT / name) for name in REFERENCE_PATHS},
        "sourcePins": before,
        "originalPythonUnchanged": sources == after_sources,
        "originalPythonCount": len(sources),
        "originalBaselinePythonUnchanged": originals == baseline_pins(after_sources),
        "originalBaselinePythonCount": len(originals),
        "originalBaselinePythonPins": originals,
        "workerSettled": {
            "status": code,
            "pipesClosed": True,
            "reaped": True,
            "stderrBytes": len(error),
        },
    }
    observation.update(extra)
    result: JsonObject = {
        "schemaVersion": 1,
        "scenario": scenario,
        "input": control,
        "observation": observation,
    }
    if len(encoded(result)) > MAX_OUTPUT:
        message = "stage oracle output bound exceeded"
        raise ValueError(message)
    return result


class Arguments(argparse.Namespace):
    """Keep the finite producer and private worker controls typed."""

    scenario: str | None = None
    fixture_directory: Path | None = None
    reference_worker: Path | None = None
    daemon: Path | None = None
    daemon_arguments: list[str]

    def __init__(self) -> None:
        """Initialize the per-parse remainder without mutable shared class state."""
        super().__init__()
        self.daemon_arguments = []


def main(argv: list[str] | None = None) -> int:
    """Emit one bounded observation only after the original worker and its pipes settle."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    _ = parser.add_argument("--reference-worker", type=Path, help=argparse.SUPPRESS)
    _ = parser.add_argument("--daemon", type=Path, help=argparse.SUPPRESS)
    _ = parser.add_argument("daemon_arguments", nargs=argparse.REMAINDER)
    values = parser.parse_args(argv, namespace=Arguments())
    if values.daemon is not None:
        return daemon(
            values.daemon,
            values.daemon_arguments[1:]
            if values.daemon_arguments[:1] == ["--"]
            else values.daemon_arguments,
        )
    if values.reference_worker is not None:
        result = worker(values.reference_worker)
    elif values.scenario is not None:
        result = run(values.scenario, values.fixture_directory)
    else:
        parser.error("one closed scenario is required")
    data = encoded(result)
    if len(data) > MAX_OUTPUT:
        message = "stage oracle record too large"
        raise ValueError(message)
    if sys.stdout.buffer.write(data) != len(data):
        message = "stage oracle stdout incomplete"
        raise OSError(message)
    _ = sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
