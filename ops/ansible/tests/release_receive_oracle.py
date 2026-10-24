"""Exercise the unchanged host receiver with real urllib and private filesystem effects.

Only identity, clock, named IO faults and the HTTPS connection are inert. The
original opener, HTTPErrorProcessor, NoRedirect, main, ZIP, flock, fsync and
hardlink publication remain in the path. No network or service is permitted.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import http.client
import io
import json
import os
import shutil
import signal
import ssl
import stat
import struct
import sys
import tempfile
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override
from unittest.mock import patch
from urllib.parse import urlsplit

import release_fetch_controller_oracle as source_fixture
import release_fresh_verify_oracle as fresh
import release_verify_oracle as cached
from test_support import ROOT, obj

# isort: split
import bounded_process
import release_fetch_receiver as receiver
import release_trust as trust
from release_json import JsonObject, JsonValue, decode_json

if TYPE_CHECKING:
    import socket
    from collections.abc import Mapping
    from typing import BinaryIO, TextIO


TODAY = "2050-01-01"
GRANT = "https://inert-host.blob.core.windows.net/a.zip?sig=INERT_RECEIVE_GRANT"
MAX_FILE = 4 * 1024**2
MAX_OUTPUT = 262144
MAX_CONFIG = 32768
FRAME_COUNT = 2
PUBLIC_COUNT = 2
PRIVATE_DIRECTORY_MODE = 0o700
DOWNLOAD_TIMEOUT = 15
REVISION = "a" * 40
SCENARIOS = (
    "success",
    "repeat",
    "identical-pair",
    "identical-manifest",
    "identical-image",
    "id-i64",
    "predicate-semantic-equal",
    "wire-trailing",
    "envelope-missing-key",
    "envelope-extra-key",
    "envelope-schema-bool",
    "envelope-id-bool",
    "envelope-id-zero",
    "envelope-id-overflow",
    "envelope-revision",
    "envelope-proof",
    "frame-empty",
    "frame-no-lf",
    "frame-oversize",
    "frame-duplicate",
    "frame-utf16-be",
    "url-invalid-host",
    "url-fragment",
    "url-oversize",
    "root-required",
    "root-mode",
    "config-mode",
    "releases-mode",
    "results-mode",
    "work-mode",
    "lock-mode",
    "lock-busy",
    "work-journal-unfinished",
    "release-journal-unfinished",
    "journal-schema-bool",
    "disk-insufficient",
    "http-status-404",
    "http-status-302",
    "http-status-201",
    "http-ce-gzip",
    "http-ce-first-valid",
    "http-ce-first-invalid",
    "http-cl-missing",
    "http-cl-first-valid",
    "http-cl-first-invalid",
    "http-cl-leading-zero",
    "http-truncated",
    "http-extra-ignored",
    "http-overflow",
    "http-checksum",
    "http-read-error",
    "http-write-error",
    "http-flush-error",
    "http-fsync-error",
    "zip-directory-cap",
    "zip-duplicate",
    "zip-unsafe",
    "zip-missing",
    "zip-crc",
    "zip-truncated",
    "zip-method",
    "zip-encrypted",
    "zip-empty-selected",
    "zip-extra-opaque",
    "zip-extra-bad-crc",
    "zip-directory-data",
    "zip-prefix-files",
    "zip-local-metadata-diff",
    "zip-local-flags-diff",
    "source-revision",
    "source-keys",
    "source-hash",
    "outcome-passed-int",
    "outcome-error-missing",
    "outcome-error-value",
    "manifest-revision",
    "manifest-timestamp",
    "bundle-mutated",
    "predicate-mismatch",
    "security-passed-int",
    "runtime-required",
    "runtime-expired",
    "runtime-clock-error",
    "vex-cardinality",
    "ignored-large-int",
    "existing-manifest-diff",
    "existing-image-diff",
    "existing-symlink",
    "first-link-error",
    "second-link-error",
    "race-eexist-equal",
    "race-eexist-diff",
    "publish-fsync-error",
    "extract-write-error",
    "receipt-create-error",
    "receipt-fsync-error",
    "final-write-error",
    "final-flush-error",
    "receiver-interrupt",
    "receiver-alarm",
    "frame-syntax",
    "race-eexist-hardlink",
    "frame-exact-limit",
    "url-host-domain",
)
POSITIVE = {
    "success",
    "repeat",
    "identical-pair",
    "identical-manifest",
    "identical-image",
    "id-i64",
    "predicate-semantic-equal",
    "wire-trailing",
    "frame-utf16-be",
    "http-ce-first-valid",
    "http-cl-missing",
    "http-cl-first-valid",
    "http-extra-ignored",
    "zip-extra-opaque",
    "zip-extra-bad-crc",
    "zip-directory-data",
    "zip-prefix-files",
    "zip-local-metadata-diff",
    "zip-local-flags-diff",
    "runtime-required",
    "ignored-large-int",
    "race-eexist-equal",
    "race-eexist-hardlink",
    "frame-exact-limit",
    "url-host-domain",
}
REFERENCE_PATHS = (
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_release_fetch.py",
    "ops/ansible/tests/test_support.py",
)
# Closed original source names from the exact6f source manifest. Additional
# maintained files remain in the full before/after census; they cannot replace
# a missing original name merely by keeping its aggregate count high.
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
    (
        "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/"
        + "test_release_container_harness.py"
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
)

PROFILE = {
    "final-write-error": "Native checked output closure keeps the original first cause.",
    "final-flush-error": "Native checked output closure keeps the original first cause.",
    "http-status-404": "Real urllib rejects HTTPError before ZIP creation.",
    "http-status-302": "Real NoRedirect rejects HTTPError before ZIP creation.",
}


def encoded(value: JsonValue) -> bytes:
    """Encode one ordinary JSON object with its required final LF."""
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def digest(data: bytes) -> str:
    """Pin original bytes without exposing their contents."""
    return hashlib.sha256(data).hexdigest()


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create and close a fixture original with an explicit final private mode."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        _ = output.write(data)
        output.flush()
        os.fchmod(output.fileno(), mode)


def directory(path: Path) -> None:
    """Create one fixture directory and explicitly set its intended private mode."""
    path.mkdir(mode=0o700)
    path.chmod(0o700)


def pin(path: Path) -> JsonObject:
    """Measure one bounded original regular fixture file."""
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE:
        message = "unexpected receive fixture file"
        raise ValueError(message)
    data = path.read_bytes()
    return {"bytes": len(data), "sha256": digest(data), "mode": stat.S_IMODE(metadata.st_mode)}


def inventory(path: Path) -> JsonObject:
    """Observe only bounded regular fixture members, never follow a symlink."""
    return {
        str(member.relative_to(path)): pin(member)
        for member in sorted(path.rglob("*"))
        if member.is_file() and not member.is_symlink()
    }


def source_pins() -> JsonObject:
    """Bind the actual complete reference and imported fixture source closure."""
    return {name: pin(ROOT / name) for name in REFERENCE_PATHS}


def original_python_pins() -> JsonObject:
    """Measure maintained original roots, excluding only this newly authored pair."""
    paths = {path for name in source_fixture.PYTHON_ROOTS for path in (ROOT / name).rglob("*.py")}
    paths.update(ROOT / name for name in source_fixture.PYTHON_VENDOR_PATHS)
    own = {
        "ops/ansible/tests/release_receive_oracle.py",
        "ops/ansible/tests/test_release_receive_oracle.py",
    }
    return {
        str(path.relative_to(ROOT)): pin(path)
        for path in sorted(paths)
        if path.relative_to(ROOT).as_posix() not in own
    }


def baseline_python_pins(all_sources: JsonObject) -> JsonObject:
    """Retain every original path and its current byte/hash/mode pin explicitly."""
    if any(name not in all_sources for name in BASELINE_PYTHON_PATHS):
        message = "an original Python source is missing from the maintained census"
        raise ValueError(message)
    return {name: all_sources[name] for name in BASELINE_PYTHON_PATHS}


@dataclass(frozen=True)
class Fixture:
    """Retain the immutable source bytes used by both independent operations."""

    files: dict[str, bytes]
    envelope: JsonObject
    first: bytes
    second: bytes
    response: bytes
    repeat: int


def fixture(scenario: str) -> Fixture:  # noqa: C901, PLR0912, PLR0915 -- Closed data cases.
    """Bind every intended mutation before ZIP and selected-envelope authentication."""
    if scenario not in SCENARIOS:
        message = "unknown closed receive scenario"
        raise ValueError(message)
    original = source_fixture.fixture("success-fresh")
    files = {name: data for name, data in original.files.items() if name != "artifact.zip"}
    selected = dict(original.selection)
    if scenario == "id-i64":
        for key in ("artifactId", "buildRunId", "ciRunId", "runAttempt"):
            selected[key] = 2**63 - 1
    metadata_case = {
        "security-passed-int": "security-passed-integer",
        "ignored-large-int": "ignored-large-integer",
    }.get(scenario, "success")
    if scenario in {"runtime-required", "runtime-expired", "runtime-clock-error"}:
        metadata_case = "required"
    _ = cached.metadata_cases(metadata_case, files, selected)
    changes: dict[str, tuple[str, str, JsonValue]] = {
        "source-revision": ("source.json", "revision", "b" * 40),
        "outcome-passed-int": ("outcome.json", "passed", 1),
        "outcome-error-value": ("outcome.json", "error", "INERT_FAILURE"),
        "manifest-revision": ("release.json", "revision", "b" * 40),
        "manifest-timestamp": ("release.json", "createdAt", "not-UTC"),
        "vex-cardinality": ("vex.openvex.json", "statements", [{}]),
    }
    if scenario in changes:
        name, key, value = changes[scenario]
        record = obj(decode_json(files[name]))
        record[key] = value
        files[name] = encoded(record)
    if scenario == "source-hash":
        record = obj(decode_json(files["source.json"]))
        obj(record, "inputsSha256")["Dockerfile"] = "x" * 64
        files["source.json"] = encoded(record)
    if scenario in {"source-keys", "outcome-error-missing"}:
        name, key = (
            ("source.json", "ignored") if scenario == "source-keys" else ("outcome.json", "error")
        )
        record = obj(decode_json(files[name]))
        if scenario == "source-keys":
            record[key] = None
        else:
            del record[key]
        files[name] = encoded(record)
    if metadata_case == "required":
        record = obj(decode_json(files["runtime-proof.json"]))
        obj(record, "sourceReview")["expires"] = "2050-01-02"
        files["runtime-proof.json"] = encoded(record)
    # Bind all final evidence bytes without resetting the named malformed records.
    security = obj(decode_json(files["image-security.json"]))
    security["runtimeProofSha256"] = digest(files["runtime-proof.json"])
    security["vexSha256"] = digest(files["vex.openvex.json"])
    files["image-security.json"] = encoded(security)
    claim = cached.claim_cases("success", files, selected)
    envelope: JsonObject = {
        **selected,
        "attestation": {
            "predicate": claim,
            "bundleSha256": digest(files[trust.BUNDLE]),
            "verificationSha256": {
                name: digest(b"INERT_VERIFY_" + name.encode()) for name in trust.SUBJECTS
            },
        },
    }
    if scenario == "predicate-semantic-equal":
        downloaded = obj(decode_json(files[trust.PREDICATE]))
        downloaded["schemaVersion"] = True
        files[trust.PREDICATE] = encoded(downloaded)
    if scenario == "predicate-mismatch":
        downloaded = obj(decode_json(files[trust.PREDICATE]))
        downloaded["runId"] = 1
        files[trust.PREDICATE] = encoded(downloaded)
    if scenario == "bundle-mutated":
        files[trust.BUNDLE] += b"\nINERT_MUTATION\n"
    zip_case = {
        "zip-unsafe": "zip-traversal",
        "zip-crc": "zip-selected-crc",
        "zip-encrypted": "zip-encrypted-extra",
    }.get(scenario, scenario if scenario.startswith("zip-") else "zip-store")
    raw = cached.zip_bytes(files, zip_case)
    if scenario == "zip-directory-cap":
        changed = bytearray(raw)
        struct.pack_into("<L", changed, changed.rfind(b"PK\x05\x06") + 12, receiver.CHUNK + 1)
        raw = bytes(changed)
    elif scenario == "zip-truncated":
        raw = raw[:-10]
    envelope["artifactZipBytes"], envelope["zipSha256"] = len(raw), digest(raw)
    files["artifact.zip"] = raw
    envelope_changes: dict[str, tuple[str, JsonValue]] = {
        "envelope-extra-key": ("extra", None),
        "envelope-schema-bool": ("schemaVersion", True),
        "envelope-id-bool": ("artifactId", True),
        "envelope-id-zero": ("artifactId", 0),
        "envelope-id-overflow": ("artifactId", 2**63),
        "envelope-revision": ("revision", "A" * 40),
        "envelope-proof": ("attestation", {}),
    }
    if scenario in envelope_changes:
        key, value = envelope_changes[scenario]
        envelope[key] = value
    if scenario == "envelope-missing-key":
        del envelope["repository"]
    first = encoded(envelope)
    if scenario == "frame-empty":
        first = b""
    elif scenario == "frame-no-lf":
        first = first[:-1]
    elif scenario == "frame-oversize":
        first = b" " * receiver.LINE_BYTES + first
    elif scenario == "frame-duplicate":
        first = b'{"schemaVersion":1,' + first[1:]
    elif scenario == "frame-syntax":
        first = b"{broken syntax\n"
    elif scenario == "frame-exact-limit":
        first = first[:-1] + b" " * (receiver.LINE_BYTES - len(first)) + b"\n"
    elif scenario == "frame-utf16-be":
        first = first.decode().encode("utf-16-be")
    url = {
        "url-invalid-host": "https://invalid.example/a?inert=1",
        "url-fragment": GRANT + "#bad",
        "url-oversize": GRANT + "x" * receiver.LINE_BYTES,
        "url-host-domain": GRANT + "x" * (13000 - len(GRANT)),
    }.get(scenario, GRANT)
    second = encoded({"url": url})
    if scenario == "wire-trailing":
        second += b"not JSON: deliberately ignored third frame\n"
    return Fixture(
        files, envelope, first, second, http_wire(scenario, raw), 2 if scenario == "repeat" else 1
    )


def http_wire(scenario: str, raw: bytes) -> bytes:
    """Retain actual raw HTTP headers and body for mature parser observations."""
    status = {
        "http-status-404": "404 Missing",
        "http-status-302": "302 Found",
        "http-status-201": "201 Created",
    }.get(scenario, "200 OK")
    headers = [("Content-Length", str(len(raw))), ("Content-Encoding", "identity")]
    if scenario == "http-status-302":
        headers.append(("Location", GRANT))
    if scenario in {"http-cl-missing", "http-overflow"}:
        headers = [("Content-Encoding", "identity")]
    if scenario == "http-ce-gzip":
        headers[1] = ("Content-Encoding", "gzip")
    if scenario.startswith("http-ce-first-"):
        pair = ("identity", "gzip") if scenario.endswith("-valid") else ("gzip", "identity")
        headers[1:] = [("Content-Encoding", value) for value in pair]
    if scenario.startswith("http-cl-first-"):
        pair = (str(len(raw)), "1") if scenario.endswith("-valid") else ("1", str(len(raw)))
        headers[:1] = [("Content-Length", value) for value in pair]
    if scenario == "http-cl-leading-zero":
        headers[0] = ("Content-Length", "0" + str(len(raw)))
    body = raw
    if scenario == "http-truncated":
        body = raw[: receiver.CHUNK + 3]
    elif scenario in {"http-extra-ignored", "http-overflow"}:
        body += b"EXTRA"
    elif scenario == "http-checksum":
        body = bytes([raw[0] ^ 1]) + raw[1:]
    prefix = "HTTP/1.1 " + status + "\r\n"
    return (
        prefix + "\r\n".join(name + ": " + value for name, value in headers) + "\r\n\r\n"
    ).encode() + body


CURL_SOURCE = """from pathlib import Path
import sys
sys.path.insert(0, ORACLE_PATH)
from release_receive_oracle import curl_worker
raise SystemExit(curl_worker(Path(__file__).resolve().parent.parent, sys.argv[1:]))
"""


def populate(root: Path, source: Fixture, scenario: str) -> JsonObject:
    """Export pristine roots and raw inputs before executing any reference code."""
    for name in ("public", "config", "work", "inputs", "bin", "capath"):
        directory(root / name)
    for name in ("releases", "results"):
        directory(root / "public" / name)
    write(root / "ca.pem", b"INERT_CA_NOT_USED_FOR_TLS\n")
    write(root / "inputs" / "envelope.bin", source.first)
    write(root / "inputs" / "url.bin", source.second)
    write(root / "inputs" / "response.http", source.response)
    for name, data in source.files.items():
        write(root / "inputs" / name, data)
    write(root / "events.jsonl", b"")
    control: JsonObject = {
        "scenario": scenario,
        "today": TODAY,
        "repeat": source.repeat,
        "envelope": source.envelope,
    }
    write(root / "control.json", encoded(control))
    script = CURL_SOURCE.replace("ORACLE_PATH", repr(str(Path(__file__).parent)))
    write(root / "bin" / "curl", (f"#!{sys.executable}\n" + script).encode(), 0o700)
    prepare(root, source, scenario)
    return control


def prepare(root: Path, source: Fixture, scenario: str) -> None:
    """Prepare real protected state and existing publication before either operation."""
    wrong_mode = {
        "root-mode": root / "public",
        "config-mode": root / "config",
        "releases-mode": root / "public" / "releases",
        "results-mode": root / "public" / "results",
        "work-mode": root / "work",
    }.get(scenario)
    if wrong_mode is not None:
        wrong_mode.chmod(0o755)
    if scenario in {"lock-mode", "lock-busy"}:
        write(root / "work" / "workload.lock", b"", 0o644 if scenario == "lock-mode" else 0o600)
    if scenario in {"work-journal-unfinished", "release-journal-unfinished", "journal-schema-bool"}:
        path = (
            root / "public" / "release-state.json"
            if scenario == "release-journal-unfinished"
            else root / "work" / "current.json"
        )
        write(
            path,
            encoded(
                {
                    "schemaVersion": True if scenario == "journal-schema-bool" else 1,
                    "finalized": scenario == "journal-schema-bool",
                }
            ),
        )
    destination = root / "public" / "releases" / REVISION
    directory(destination)
    names = {
        "identical-pair": ("release.json", "image.tar"),
        "identical-manifest": ("release.json",),
        "identical-image": ("image.tar",),
        "existing-manifest-diff": ("release.json",),
        "existing-image-diff": ("image.tar",),
    }.get(scenario, ())
    for name in names:
        write(destination / name, b"different" if scenario.endswith("diff") else source.files[name])
    if scenario == "existing-symlink":
        (destination / "release.json").symlink_to(root / "inputs" / "release.json")


def event(root: Path, role: str, **facts: JsonValue) -> None:
    """Append finite non-secret observations to an already owned fixture stream."""
    with (root / "events.jsonl").open("ab") as output:
        _ = output.write(encoded({"role": role, **facts}))


class MemoryConnection:
    """Adapt below urllib, retaining its real opener, errors and redirect policy."""

    def __init__(self, root: Path, host: str, timeout: object, **_kwargs: object) -> None:
        """Admit only one fixed credentialless host and original fifteen-second timeout."""
        if host != "inert-host.blob.core.windows.net" or timeout != DOWNLOAD_TIMEOUT:
            message = "unexpected receiver connection"
            raise ValueError(message)
        self.root: Path = root
        self.sock: None = None

    def set_debuglevel(self, value: int) -> None:
        """Reject any unexpected diagnostic stream request."""
        if value != 0:
            message = "unexpected receiver debug output"
            raise ValueError(message)

    def request(
        self,
        method: str,
        target: str,
        body: object,
        headers: Mapping[str, str],
        *,
        encode_chunked: bool,
    ) -> None:
        """Observe exact safe request facts without retaining the signed query."""
        supplied = obj(decode_json((self.root / "inputs" / "url.bin").read_bytes().splitlines()[0]))
        parsed = urlsplit(str(supplied["url"]))
        expected = parsed.path + "?" + parsed.query
        if method != "GET" or target != expected or body is not None:
            message = "unexpected receiver request"
            raise ValueError(message)
        lowered = {key.lower(): value for key, value in headers.items()}
        if "authorization" in lowered or encode_chunked:
            message = "unexpected receiver credential"
            raise ValueError(message)
        event(
            self.root,
            "http-request",
            method=method,
            authorization=False,
            accept=lowered.get("accept"),
            identity=lowered.get("accept-encoding"),
            userAgent=lowered.get("user-agent"),
        )

    def getresponse(self) -> MemoryResponse:
        """Return a genuine HTTPResponse after its mature status/header parsing."""
        response = MemoryResponse(self.root)
        response.begin()
        return response

    def close(self) -> None:
        """Record connection closure without admitting any socket or network."""
        event(self.root, "connection-closed")


class MemoryResponse(http.client.HTTPResponse):
    """Keep real HTTPResponse framing and narrowly inject one actual read failure."""

    def __init__(self, root: Path) -> None:
        """Bind the original raw message and owned in-memory response peer."""
        self.root: Path = root
        self.reads: int = 0
        data = (root / "inputs" / "response.http").read_bytes()
        self.pipe: fresh.RawPipe = fresh.RawPipe(fresh.Wire(data, data.index(b"\r\n\r\n") + 4))
        sock = cast("socket.socket", cast("object", fresh.RawSocket(self.pipe)))
        super().__init__(sock, method="GET")

    @override
    def read(self, amt: int | None = None) -> bytes:
        """Delegate the actual framing and record its original logical read boundary."""
        self.reads += 1
        control = obj(decode_json((self.root / "control.json").read_bytes()))
        scenario = control["scenario"]
        if self.reads == FRAME_COUNT and scenario in {
            "http-read-error",
            "receiver-interrupt",
            "receiver-alarm",
        }:
            if scenario == "http-read-error":
                raise OSError(errno.EIO, "inert read failure")
            number = signal.SIGINT if scenario == "receiver-interrupt" else signal.SIGALRM
            handler = signal.getsignal(number)
            if callable(handler):
                handler(number, None)
        data = super().read(amt)
        event(self.root, "http-read", requested=amt, bytes=len(data))
        return data

    @override
    def close(self) -> None:
        """Close every original in-memory peer, including HTTPError paths."""
        super().close()
        self.pipe.close()
        event(self.root, "http-closed", responseClosed=self.isclosed(), pipeClosed=self.pipe.closed)


@final
class Input:
    """Supply two actual bounded frames and refuse an unexpected EOF dependency."""

    def __init__(self, root: Path, output: Output) -> None:
        """Retain the two original inputs and their ready-handshake witness."""
        self.root, self.output, self.reads = root, output, 0

    def readline(self, size: int = -1, /) -> bytes:
        """Read at most the original cap; a third read is never a success observation."""
        self.reads += 1
        if self.reads > FRAME_COUNT or (self.reads == FRAME_COUNT and not self.output.ready):
            message = "unexpected receiver input ordering"
            raise ValueError(message)
        path = self.root / "inputs" / ("envelope.bin" if self.reads == 1 else "url.bin")
        with path.open("rb") as source:
            data = source.readline(size)
        event(self.root, "input", index=self.reads, bytes=len(data), lf=data.endswith(b"\n"))
        return data


class Output(io.StringIO):
    """Retain actual output text while exposing only closed lower write/flush faults."""

    def __init__(self, root: Path, scenario: str) -> None:
        """Retain one receipt stream and its actual ready flush state."""
        super().__init__()
        self.root: Path = root
        self.scenario: str = scenario
        self.ready: bool = False
        self.final: bool = False

    @override
    def write(self, text: str) -> int:
        """Write the original receipt, or fail at the selected final output boundary."""
        record = obj(decode_json(text))
        self.final = record.get("status") != "ready"
        if self.final and self.scenario == "final-write-error":
            raise OSError(errno.EIO, "inert final output write")
        event(self.root, "output", final=self.final, passed=record.get("passed"))
        return super().write(text)

    @override
    def flush(self) -> None:
        """Only the original ready flush authorizes the second frame read."""
        if self.final and self.scenario == "final-flush-error":
            raise OSError(errno.EIO, "inert final output flush")
        if not self.final:
            self.ready = True
        super().flush()


@final
class FaultFile:
    """Delegate real owned writes and close, failing after a real three-byte prefix."""

    def __init__(self, original: BinaryIO, fault: str) -> None:
        """Retain the one original output and selected failure boundary."""
        self.original, self.fault = original, fault

    def __enter__(self) -> FaultFile:
        """Return the retained original wrapper."""
        return self

    def __exit__(self, *_arguments: object) -> None:
        """Close the original even after write or flush failed."""
        self.original.close()

    def write(self, data: bytes) -> int:
        """Preserve the actual three-byte prefix before an injected write failure."""
        if self.fault in {"http-write-error", "extract-write-error"}:
            _ = self.original.write(data[:3])
            self.original.flush()
            raise OSError(errno.EIO, "inert three-byte write")
        return self.original.write(data)

    def flush(self) -> None:
        """Preserve the original close while injecting only the named flush failure."""
        if self.fault == "http-flush-error":
            raise OSError(errno.EIO, "inert output flush")
        self.original.flush()

    def fileno(self) -> int:
        """Borrow only the original descriptor for the real fsync boundary."""
        return self.original.fileno()


def worker(root: Path) -> JsonObject:  # noqa: C901, PLR0915 -- One owned fault scope.
    """Execute every original receiver phase with actual retained local effects."""
    control = obj(decode_json((root / "control.json").read_bytes()))
    scenario = str(control["scenario"])
    public, config, work = (root / name for name in ("public", "config", "work"))
    real_lstat, real_open, real_descriptor, real_sync, real_link = (
        os.lstat,
        Path.open,
        os.open,
        os.fsync,
        os.link,
    )
    receipt_fd: int | None = None
    lock_checks: list[JsonValue] = []
    held_lock: BinaryIO | None = None
    links: list[JsonValue] = []

    def lstat(
        path: str | bytes | os.PathLike[str] | os.PathLike[bytes], *, dir_fd: int | None = None
    ) -> os.stat_result:
        metadata = real_lstat(path, dir_fd=dir_fd)
        if os.fsdecode(path).startswith(str(root)):
            values = list(metadata)
            values[4] = 0
            return os.stat_result(values)
        return metadata

    def opened(  # noqa: PLR0913, PLR0917 -- Mirror the actual Path.open signature.
        path: Path,
        mode: str = "r",
        buffering: int = -1,
        encoding: str | None = None,
        errors: str | None = None,
        newline: str | None = None,
    ) -> BinaryIO | TextIO | FaultFile:
        original = real_open(path, mode, buffering, encoding, errors, newline)
        if mode == "xb" and (
            (path.name == "artifact.zip" and scenario in {"http-write-error", "http-flush-error"})
            or (path.name == "image.tar" and scenario == "extract-write-error")
        ):
            return FaultFile(cast("BinaryIO", original), scenario)
        return cast("BinaryIO | TextIO", original)

    def descriptor_open(
        path: object, flags: int, mode: int = 0o600, *, dir_fd: int | None = None
    ) -> int:
        nonlocal receipt_fd
        if Path(cast("str", path)).name == "fetch-outcome.json":
            with real_open(work / "workload.lock", "rb") as check:
                try:
                    fcntl.flock(check, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    locked = False
                except BlockingIOError:
                    locked = True
            lock_checks.append(locked)
            if scenario == "receipt-create-error":
                raise OSError(errno.EIO, "inert receipt create")
            receipt_fd = real_descriptor(cast("str", path), flags, mode, dir_fd=dir_fd)
            return receipt_fd
        return real_descriptor(cast("str", path), flags, mode, dir_fd=dir_fd)

    def synced(descriptor: int) -> None:
        nonlocal receipt_fd
        is_receipt = descriptor == receipt_fd
        if is_receipt:
            receipt_fd = None
        if is_receipt and scenario == "receipt-fsync-error":
            raise OSError(errno.EIO, "inert receipt sync")
        metadata = os.fstat(descriptor)
        if scenario == "http-fsync-error":
            for attempt in public.glob("releases/*/download.*"):
                path = attempt / "artifact.zip"
                if path.exists() and metadata.st_ino == path.stat().st_ino:
                    raise OSError(errno.EIO, "inert download sync")
        if (
            scenario == "publish-fsync-error"
            and len(links) == PUBLIC_COUNT
            and stat.S_ISDIR(metadata.st_mode)
        ):
            raise OSError(errno.EIO, "inert publication sync")
        real_sync(descriptor)

    def linked(source: Path, target: Path, *, follow_symlinks: bool) -> None:
        if scenario == "first-link-error" or (scenario == "second-link-error" and len(links) == 1):
            raise OSError(errno.EIO, "inert publication link")
        if scenario.startswith("race-eexist-") and target.name == "release.json":
            if scenario.endswith("hardlink"):
                real_link(source, target, follow_symlinks=False)
            else:
                write(
                    target,
                    source.read_bytes() if scenario.endswith("equal") else b"racing-different",
                )
            raise FileExistsError(errno.EEXIST, "inert competing entry")
        real_link(source, target, follow_symlinks=follow_symlinks)
        links.append(target.name)
        event(root, "link", name=target.name)

    @final
    class Connection(MemoryConnection):
        """Expose the debuglevel attribute used by the actual HTTPSHandler."""

        debuglevel = 0

        def __init__(self, host: str, timeout: object, **kwargs: object) -> None:
            """Bind only this worker's private original response."""
            super().__init__(root, host, timeout, **kwargs)

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)

    def tls(**kwargs: object) -> ssl.SSLContext:
        event(
            root, "tls-context", fixedCA=kwargs == {"cafile": "/etc/ssl/certs/ca-certificates.crt"}
        )
        return context

    def space(_path: object) -> fresh.Disk:
        return fresh.Disk(2**50, 0, 0 if scenario == "disk-insufficient" else 2**50)

    cached.FixedDateTime.calls = 0
    cached.FixedDateTime.clock = "fixed"
    if scenario == "runtime-clock-error":
        cached.FixedDateTime.clock = "error"
    elif scenario == "runtime-expired":
        cached.FixedDateTime.clock, cached.FixedDateTime.calls = "late-expiry", 1
    if scenario == "lock-busy":
        held_lock = real_open(work / "workload.lock", "rb")
        fcntl.flock(held_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    replacements: list[tuple[object, str, object]] = [
        (receiver, "ROOT", public),
        (receiver, "CONFIG", config),
        (receiver, "WORK", work),
        (os, "geteuid", lambda: 1 if scenario == "root-required" else 0),
        (os, "lstat", lstat),
        (os, "open", descriptor_open),
        (os, "fsync", synced),
        (os, "link", linked),
        (Path, "open", opened),
        (ssl, "create_default_context", tls),
        (http.client, "HTTPSConnection", Connection),
        (shutil, "disk_usage", space),
        (trust, "datetime", cached.FixedDateTime),
    ]
    codes: list[JsonValue] = []
    uncaught: list[JsonValue] = []
    receipts: list[JsonValue] = []
    try:
        with ExitStack() as stack:
            for module, name, replacement in replacements:
                _ = stack.enter_context(patch.object(module, name, replacement))
            for _index in range(cast("int", control["repeat"])):
                output = Output(root, scenario)
                try:
                    codes.append(receiver.main(Input(root, output), cast("TextIO", output)))
                except BaseException as error:  # noqa: BLE001 -- Fixed original class only.
                    codes.append(1)
                    uncaught.append(type(error).__name__)
                finally:
                    text = output.getvalue()
                    receipts.extend(decode_json(line) for line in text.splitlines())
                    output.close()
    finally:
        if held_lock is not None:
            held_lock.close()
    lock_available = True
    if (work / "workload.lock").exists():
        with real_open(work / "workload.lock", "rb") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                lock_available = False
    return {
        "exitCodes": codes,
        "uncaughtClasses": uncaught,
        "receipts": receipts,
        "lockHeldAtReceipt": lock_checks,
        "lockReleased": lock_available,
    }


def curl_arguments(root: Path, capath: Path | None = None) -> list[str]:
    """Return the fixed honest host OpenSSL curl invocation contract."""
    return [
        "--disable",
        "--silent",
        "--show-error",
        "--http1.1",
        "--include",
        "--globoff",
        "--path-as-is",
        "--noproxy",
        "*",
        "--proxy",
        "",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--connect-timeout",
        "15",
        "--tlsv1.2",
        "--no-ca-native",
        "--cacert",
        str(root / "ca.pem"),
        "--capath",
        str(root / "capath" if capath is None else capath),
        "--config",
        "-",
    ]


def curl_config(root: Path) -> bytes:
    """Construct only the fixed inert private config, with no credential argv."""
    supplied = obj(decode_json((root / "inputs" / "url.bin").read_bytes().splitlines()[0]))
    parsed = urlsplit(str(supplied["url"]))
    values = [
        ("url", "https://" + parsed.netloc + "/"),
        ("request-target", parsed.path + "?" + parsed.query),
        ("header", "Accept: application/zip"),
        ("header", "Accept-Encoding: identity"),
        ("user-agent", "simplestchat-release-receive"),
    ]
    return ("\n".join(key + " = " + json.dumps(value) for key, value in values) + "\n").encode()


def config_fields(data: bytes) -> list[tuple[str, str]]:
    """Parse the closed fixture curl config using ordinary quoted-string decoding."""
    fields: list[tuple[str, str]] = []
    for line in data.decode("ascii").splitlines():
        key, separator, value = line.partition("=")
        if not separator:
            message = "unexpected inert curl config"
            raise ValueError(message)
        decoded: object = cast("object", json.loads(value.strip()))
        if not isinstance(decoded, str):
            message = "unexpected inert curl config value"
            raise TypeError(message)
        fields.append((key.strip(), decoded))
    return fields


def valid_capath(root: Path, path: Path) -> bool:
    """Accept only the exact fixture or current private attempt's empty CA directory."""
    if not path.is_absolute() or path.resolve() != path:
        return False
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_IMODE(metadata.st_mode) != PRIVATE_DIRECTORY_MODE
        or metadata.st_uid != os.getuid()
    ):
        return False
    if path != root / "capath":
        base = root / "public" / "releases" / REVISION
        if (
            path.parent.parent != base
            or not path.parent.name.startswith("download.")
            or not path.name.startswith(".curl-ca.")
        ):
            return False
    with os.scandir(path) as children:
        return next(children, None) is None


def curl_worker(root: Path, arguments: list[str]) -> int:
    """Expose a closed no-network curl observation for the native comparator."""
    if arguments == ["--disable", "--version"]:
        _ = sys.stdout.write("curl 8.4.0 INERT OpenSSL/3.0.0\nProtocols: https\nFeatures: SSL\n")
        return 0
    if len(arguments) != len(curl_arguments(root)):
        return 99
    position = arguments.index("--capath") + 1 if "--capath" in arguments else 0
    if not position:
        return 99
    capath = Path(arguments[position])
    if arguments != curl_arguments(root, capath) or not valid_capath(root, capath):
        return 99
    config = sys.stdin.buffer.read(MAX_CONFIG + 1)
    if len(config) > MAX_CONFIG or config_fields(config) != config_fields(curl_config(root)):
        return 99
    raw = (root / "inputs" / "response.http").read_bytes()
    output = fresh.RawPipe(fresh.Wire(raw, raw.index(b"\r\n\r\n") + 4))
    sock = cast("socket.socket", cast("object", fresh.RawSocket(output)))
    response = http.client.HTTPResponse(sock, method="GET")
    response.begin()
    prefix = raw[: raw.index(b"\r\n\r\n") + 4]
    _ = sys.stdout.buffer.write(prefix)
    status = 0
    try:
        while data := response.read(65536):
            _ = sys.stdout.buffer.write(data)
        if response.length not in (None, 0):
            status = 18
    finally:
        response.close()
        output.close()
    _ = sys.stdout.buffer.flush()
    return status


def normalized(value: JsonValue, root: Path) -> JsonValue:
    """Keep report semantics while separating nondeterministic private byte pins."""
    if isinstance(value, dict):
        return {
            key: normalized(member, root)
            for key, member in value.items()
            if key != "elapsedSeconds"
        }
    if isinstance(value, list):
        return [normalized(member, root) for member in value]
    if isinstance(value, str) and value.startswith(str(root)):
        relative = Path(value).relative_to(root)
        return "/fixture/" + "/".join(
            "download.ATTEMPT" if part.startswith("download.") else part for part in relative.parts
        )
    return value


def evidence(root: Path) -> tuple[list[JsonValue], JsonObject]:
    """Measure real retained prefixes and publication inode relationships after settlement."""
    attempts: list[JsonValue] = []
    destination = root / "public" / "releases" / REVISION
    for attempt in sorted(destination.glob("download.*")):
        files = inventory(attempt)
        journals: JsonObject = {}
        for name in ("envelope.json", "fetch-outcome.json"):
            if (attempt / name).exists():
                journals[name] = normalized(decode_json((attempt / name).read_bytes()), root)
        links: JsonObject = {}
        for name in ("release.json", "image.tar"):
            target = destination / name
            source = attempt / name
            links[name] = (
                target.exists()
                and not target.is_symlink()
                and source.exists()
                and (target.stat().st_dev, target.stat().st_ino)
                == (source.stat().st_dev, source.stat().st_ino)
            )
        attempts.append({"files": files, "journals": journals, "publicationLinks": links})
    publication: JsonObject = {
        name: pin(destination / name)
        for name in ("release.json", "image.tar")
        if (destination / name).exists() and not (destination / name).is_symlink()
    }
    return attempts, publication


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Run a complete isolated reference and export separate pristine native inputs."""
    if fixture_directory is None:
        with tempfile.TemporaryDirectory(prefix="receive-oracle.") as name:
            return run(scenario, Path(name).resolve())
    root = fixture_directory
    if not root.is_absolute() or root.resolve() != root or any(root.iterdir()):
        message = "fixture directory must be fresh and canonical"
        raise ValueError(message)
    root.chmod(0o700)
    before = source_pins()
    original_python = original_python_pins()
    baseline_python = baseline_python_pins(original_python)
    source = fixture(scenario)
    for name in ("native", "reference"):
        directory(root / name)
        _ = populate(root / name, source, scenario)
    original = inventory(root / "reference" / "inputs")
    environment = {"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"}
    code, output, error = bounded_process.run(
        [
            sys.executable,
            "-B",
            str(Path(__file__).resolve()),
            "--reference-worker",
            str(root / "reference"),
        ],
        limits=bounded_process.Limits(timeout=20, stdout=MAX_OUTPUT, stderr=65536),
        env=environment,
    )
    if code != 0 or error:
        message = "closed receive worker failed"
        raise ValueError(message)
    observation = obj(decode_json(output))
    attempts, publication = evidence(root / "reference")
    events = [
        decode_json(line)
        for line in (root / "reference" / "events.jsonl").read_bytes().splitlines()
    ]
    after_python = original_python_pins()
    after_baseline_python = baseline_python_pins(after_python)
    observation.update(
        {
            "passed": all(
                value == 0 for value in cast("list[JsonValue]", observation["exitCodes"])
            ),
            "attempts": attempts,
            "publication": publication,
            "events": events,
            "originalUnchanged": original == inventory(root / "reference" / "inputs"),
            "sourceUnchanged": before == source_pins(),
            "originalPythonUnchanged": original_python == after_python,
            "originalPythonCount": len(original_python),
            "originalBaselinePythonUnchanged": baseline_python == after_baseline_python,
            "originalBaselinePythonCount": len(baseline_python),
            "originalBaselinePythonPins": baseline_python,
            "sourcePins": before,
            "workerSettled": {
                "status": code,
                "pipesClosed": True,
                "reaped": True,
                "stderrBytes": len(error),
            },
        }
    )
    control = obj(decode_json((root / "native" / "control.json").read_bytes()))
    control.update(
        {
            "frames": {
                name: pin(root / "native" / "inputs" / name) for name in ("envelope.bin", "url.bin")
            },
            "ca": pin(root / "native" / "ca.pem"),
            "rawResponse": pin(root / "native" / "inputs" / "response.http"),
        }
    )
    if scenario in PROFILE:
        control["nativeProfile"] = PROFILE[scenario]
    result: JsonObject = {
        "schemaVersion": 1,
        "scenario": scenario,
        "input": control,
        "observation": observation,
    }
    if len(encoded(result)) > MAX_OUTPUT:
        message = "receive oracle output bound exceeded"
        raise ValueError(message)
    return result


class Arguments(argparse.Namespace):
    """Keep the closed producer and private worker selector fields typed."""

    scenario: str | None = None
    fixture_directory: Path | None = None
    reference_worker: Path | None = None


def main(argv: list[str] | None = None) -> int:
    """Emit one bounded ordinary observation after every owned worker is settled."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    _ = parser.add_argument("--reference-worker", type=Path, help=argparse.SUPPRESS)
    options = parser.parse_args(argv, namespace=Arguments())
    if options.reference_worker is not None:
        result = worker(options.reference_worker)
    elif options.scenario is not None:
        result = run(options.scenario, options.fixture_directory)
    else:
        parser.error("--scenario is required")
    _ = sys.stdout.buffer.write(encoded(result))
    _ = sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
