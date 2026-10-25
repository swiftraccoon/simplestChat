"""Run complete unchanged restore verification with real files and owned inert Docker peers.

The original CLI, snapshot, flock, atomic/fsync, restore policy and exact
container cleanup remain in the path. Only fixed daemon, root identity,
clock/UUID and named lower IO observations are inert. No network or engine.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import io
import json
import os
import signal
import stat
import sys
import tempfile
import uuid
from contextlib import ExitStack, redirect_stderr, redirect_stdout, suppress
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, Self, cast, final
from unittest.mock import patch

import release_stage_oracle as stage
from test_support import ROOT, obj, string

# isort: split
import bounded_process
import release_public as public
import restore_verify as restore
from release_json import JsonObject, JsonValue, array_value, decode_json

if TYPE_CHECKING:
    import subprocess
    from collections.abc import Sequence
    from typing import BinaryIO, Unpack

ATTEMPT = "release.fixture000"
REVISION = stage.REVISION
CHECKSUM = stage.CHECKSUM
CID = "c" * 64
IMAGE = "sha256:" + "e" * 64
NONCE = "d" * 32
FIXTURE_UUIDS = tuple(
    uuid.UUID(bytes=b"\xdd" * 15 + bytes([0xDD + index]), version=4) for index in range(2)
)
UTC = stage.UTC
MAX_OUTPUT = 2 * 1024**2
MAX_FILE = 65 * 1024**2
WORKER_SECONDS = 60
SUCCESS = "Isolated restore verified and disposable database removed.\n"
FAILURE = "Restore verification failed; inspect private restore evidence.\n"
DUMP = b"PGDMP\0INERT-ARCHIVE-NO-DATABASE\n" + bytes(range(256)) * 16
COUNTS: JsonObject = {
    "users": 3,
    "rooms": 2,
    "sessions": 4,
    "credentials": 3,
    "activeIncidents": 1,
    "resolvedIncidents": 2,
}
SCENARIOS = (
    "success",
    "failed-release-backup",
    "subset-ledger",
    "source-hardlink",
    "repeat",
    "selector-invalid",
    "root-not-root",
    "root-mode",
    "config-mode",
    "work-mode",
    "state-mode",
    "results-mode",
    "releases-mode",
    "source-mode",
    "lock-mode",
    "lock-busy",
    "current-unfinished",
    "release-unfinished",
    "journal-schema-bool",
    "outcome-missing",
    "outcome-mode",
    "outcome-symlink",
    "outcome-syntax",
    "outcome-duplicate",
    "outcome-revision",
    "outcome-digest",
    "manifest-mode",
    "manifest-symlink",
    "manifest-syntax",
    "manifest-duplicate",
    "manifest-revision",
    "ledger-missing",
    "ledger-empty",
    "ledger-version",
    "ledger-checksum",
    "ledger-not-packaged",
    "backup-empty",
    "backup-oversized",
    "backup-mode",
    "backup-symlink",
    "backup-digest",
    "snapshot-read",
    "snapshot-write",
    "snapshot-flush",
    "snapshot-fsync",
    "image-command",
    "image-identity",
    "restore-record-initial-write",
    "prior-command",
    "prior-container",
    "create-command",
    "create-unknown",
    "create-timeout-after-effect",
    "restore-record-created-write",
    "start-command",
    "ready-retry",
    "ready-deadline",
    "ready-exact-boundary",
    "roles-command",
    "restore-command",
    "restored-ledger-command",
    "restored-ledger-diff",
    "verify-sql-missing",
    "verify-sql-private0600",
    "verify-sql-hardlink",
    "verify-command",
    "verify-invalid-json",
    "amcheck-command",
    "cleanup-select-command",
    "cleanup-ambiguous",
    "cleanup-id-diff",
    "cleanup-name-diff",
    "cleanup-image-diff",
    "cleanup-owner-diff",
    "cleanup-inspect-command",
    "cleanup-remove-command",
    "cleanup-confirm-present",
    "restore-command-cleanup-remove",
    "restore-record-final-write",
    "restore-record-final-file-sync",
    "restore-record-final-directory-sync",
    "snapshot-unlink",
    "outcome-write",
    "outcome-file-sync",
    "outcome-directory-sync",
    "freshness-write",
    "freshness-file-sync",
    "freshness-directory-sync",
    "interrupt-before-restore",
    "terminate-before-restore",
)
POSITIVE = frozenset(
    {
        "success",
        "failed-release-backup",
        "subset-ledger",
        "source-hardlink",
        "repeat",
        "ready-retry",
        "ready-exact-boundary",
        "verify-sql-private0600",
        "verify-sql-hardlink",
    }
)
FAULT_CASES = frozenset(
    {
        "snapshot-read",
        "snapshot-write",
        "snapshot-flush",
        "snapshot-fsync",
        "snapshot-unlink",
        "restore-record-initial-write",
        "restore-record-created-write",
        "restore-record-final-write",
        "restore-record-final-file-sync",
        "restore-record-final-directory-sync",
        "outcome-write",
        "outcome-file-sync",
        "outcome-directory-sync",
        "freshness-write",
        "freshness-file-sync",
        "freshness-directory-sync",
        "create-timeout-after-effect",
        "interrupt-before-restore",
        "terminate-before-restore",
    }
)
OWN_PATHS = frozenset(
    {
        "ops/ansible/tests/release_restore_oracle.py",
        "ops/ansible/tests/test_release_restore_oracle.py",
    }
)
REFERENCE_PATHS = (
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/restore-verify.sql",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_support.py",
)
BASELINE_PYTHON_PATHS: tuple[str, ...] = (
    "build/bootstrap.py",
    "build/bootstrap_access.py",
    "build/bootstrap_controller.py",
    "build/build-release.py",
    "build/caddy_materials.py",
    "build/caddy_probe_guest.py",
    "build/caddy_probe_vm.py",
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
    "build/maintain_namespace_probe.py",
    "build/maintain_namespace_probe_guest.py",
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
    "build/security_vm_deploy.py",
    "build/security_vm_deploy_guest.py",
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
    "build/security_vm_stage.py",
    "build/security_vm_stage_guest.py",
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
    "ops/ansible/tests/release_deploy_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_prepare_oracle.py",
    "ops/ansible/tests/release_receive_oracle.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_automation.py",
    "ops/ansible/tests/test_backup_offsite.py",
    "ops/ansible/tests/test_bootstrap_controller.py",
    "ops/ansible/tests/test_bounded_process.py",
    "ops/ansible/tests/test_browser_assets_command.py",
    "ops/ansible/tests/test_caddy_materials.py",
    "ops/ansible/tests/test_caddy_probe_guest.py",
    "ops/ansible/tests/test_caddy_probe_vm.py",
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
    "ops/ansible/tests/test_maintain_namespace_probe.py",
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
    "ops/ansible/tests/test_release_deploy_oracle.py",
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
    "ops/ansible/tests/test_release_stage_oracle.py",
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
    "ops/ansible/tests/test_security_vm_deploy.py",
    "ops/ansible/tests/test_security_vm_fetch.py",
    "ops/ansible/tests/test_security_vm_fetch_guest.py",
    "ops/ansible/tests/test_security_vm_fetch_hosts.py",
    "ops/ansible/tests/test_security_vm_guest.py",
    "ops/ansible/tests/test_security_vm_ops.py",
    "ops/ansible/tests/test_security_vm_ops_guest.py",
    "ops/ansible/tests/test_security_vm_qmp.py",
    "ops/ansible/tests/test_security_vm_receive.py",
    "ops/ansible/tests/test_security_vm_release.py",
    "ops/ansible/tests/test_security_vm_stage.py",
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
        "xtask/src/ci/release_http_policy/reference/"
        "ops/ansible/tests/test_release_container_harness.py"
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
)

# This executable is a fixed lower daemon adapter. The unchanged Runner still
# owns every real process, pipe, deadline, bounded byte capture and consuming reap.
ADAPTER = r"""
import hashlib,json,os,sys
from pathlib import Path
root=Path(__file__).resolve().parents[1]
control=json.loads((root/'control.json').read_bytes())
path=root/'daemon.json';state=json.loads(path.read_bytes())
args=sys.argv[1:]
assert args[:2]==['--host','unix:///var/run/docker.sock']
a=args[2:];scenario=control['scenario'];cid=control['cid'];image=control['image']
source=sys.stdin.buffer.read();role='unknown';out=b'';code=0
if a[0]=='inspect' and a[-1]=='simplestchat-public-postgres-1':
 role='image';out=(image+'\n').encode()
elif a[0]=='ps':
 state['selects']+=1
 if state['selects']==1:
  role='prior'
  if scenario=='prior-container':out=(cid+'\n').encode()
 else:
  role='cleanup-select' if state['selects']==2 else 'cleanup-confirm'
  if state['present'] or scenario=='cleanup-confirm-present':out=(cid+'\n').encode()
  if scenario=='cleanup-ambiguous' and role=='cleanup-select':out+=(('f'*64)+'\n').encode()
elif a[0]=='create':
 role='create';state['name']=a[a.index('--name')+1];state['present']=True
 out=(cid+'\n').encode()
 if scenario=='create-unknown':out=b'INERT-UNKNOWN-CID\n'
elif a[0]=='start':role='start'
elif a[0]=='inspect':
 role='cleanup-inspect'
 value={'id':cid,'name':'/'+state['name'],'image':image,'owner':state['name']}
 for suffix,key in [('id','id'),('name','name'),('image','image'),('owner','owner')]:
  if scenario=='cleanup-'+suffix+'-diff':value[key]='INERT-DIFFERENT'
 out=(json.dumps(value,separators=(',',':'))+'\n').encode()
elif a[0]=='rm':role='cleanup-remove';state['present']=False
elif a[0]=='exec':
 if 'pg_isready' in ' '.join(a):
  role='ready';state['ready']+=1
  if scenario=='ready-deadline' or scenario=='ready-retry' and state['ready']<3 \
   or scenario=='ready-exact-boundary' and state['ready']<61:code=7
 elif 'pg_restore' in a:role='restore'
 elif 'pg_amcheck' in a:role='amcheck'
 elif b'CREATE ROLE simplestchat_app' in source:role='roles'
 elif b'SELECT version, success' in source:
  role='restored-ledger'
  rows=sorted(control['ledger'].items(),key=lambda x:int(x[0]))
  out=('\n'.join(k+'|t|'+v for k,v in rows)+'\n').encode()
  if scenario=='restored-ledger-diff':out=b'1|f|INERT-DIFFERENT\n'
 else:
  role='verify';out=(json.dumps(control['counts'],separators=(',',':'))+'\n').encode()
  if scenario=='verify-invalid-json':out=b'{\n'
assert role!='unknown'
if (scenario==role+'-command' or scenario=='restore-command-cleanup-remove'
 and role in {'restore','cleanup-remove'}):
 code=7
 if role=='cleanup-remove':state['present']=True
if role=='image' and scenario=='image-identity':out=b'INERT-NOT-IMAGE\n'
state['calls'].append({'arguments':args,'role':role,'inputBytes':len(source),
 'inputSha256':hashlib.sha256(source).hexdigest(),'status':code,
 'stdoutHex':out.hex(),'stderrHex':''})
fd=os.open(path,os.O_WRONLY|os.O_TRUNC|os.O_NOFOLLOW,0o600)
with os.fdopen(fd,'wb') as target:
 raw=(json.dumps(state,separators=(',',':'))+'\n').encode()
 assert target.write(raw)==len(raw);target.flush()
assert sys.stdout.buffer.write(out)==len(out);sys.stdout.buffer.flush()
sys.exit(code)
"""


def encoded(value: object) -> bytes:
    """Encode bounded fixture observations; no raw credentials or database records."""
    return json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def digest(data: bytes) -> str:
    """Pin fixture bytes without interpreting an opaque custom-format database dump."""
    return hashlib.sha256(data).hexdigest()


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create privately first, then apply the exact final fixture mode after complete bytes."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as target:
        if target.write(data) != len(data):
            message = "incomplete restore fixture write"
            raise OSError(message)
        target.flush()
        os.fchmod(target.fileno(), mode)


def directory(path: Path, mode: int = 0o700) -> None:
    """Create and explicitly set the intended owned directory mode."""
    path.mkdir(mode=0o700)
    path.chmod(mode)


def pin(path: Path) -> JsonObject:
    """Retain exact current bytes, mode and link count for ordinary bounded fixture files."""
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode):
        return {"link": str(path.readlink()), "mode": stat.S_IMODE(metadata.st_mode)}
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE:
        message = "restore fixture file outside bound"
        raise ValueError(message)
    with path.open("rb") as stream:
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"bytes": metadata.st_size, "sha256": sha, "mode": stat.S_IMODE(metadata.st_mode)}


def python_pins() -> JsonObject:
    """Measure all maintained Python plus the explicit entire original archived path set."""
    names = set(BASELINE_PYTHON_PATHS)
    for subtree in ("build", "ops", "typings", "xtask/src/ci/release_http_policy"):
        for parent, directories, files in os.walk(ROOT / subtree):
            directory = Path(parent)
            if directory == ROOT / "ops/ansible" and ".venv" in directories:
                directories.remove(".venv")
            names.update(
                (directory / name).relative_to(ROOT).as_posix()
                for name in files
                if name.endswith(".py")
            )
    return {name: pin(ROOT / name) for name in sorted(names - OWN_PATHS)}


def baseline_pins(values: JsonObject) -> JsonObject:
    """Named original paths must survive independently of any changing aggregate census."""
    if any(path not in values for path in BASELINE_PYTHON_PATHS):
        message = "original restore baseline source missing"
        raise ValueError(message)
    return {path: values[path] for path in BASELINE_PYTHON_PATHS}


def fixture(scenario: str) -> tuple[JsonObject, bytes, bytes]:
    """Produce only a closed scenario, valid original manifest and opaque input archive."""
    if scenario not in SCENARIOS:
        message = "unknown restore oracle scenario"
        raise ValueError(message)
    _, manifest, _ = stage.fixture("success")
    control: JsonObject = {
        "scenario": scenario,
        "attempt": "../not-owned" if scenario == "selector-invalid" else ATTEMPT,
        "revision": REVISION,
        "cid": CID,
        "image": IMAGE,
        "nonce": NONCE,
        "timestamp": UTC,
        "counts": COUNTS,
        "ledger": {"1": CHECKSUM},
        "repeat": 2 if scenario == "repeat" else 1,
        "nativeProfile": (
            "strict-readiness30"
            if scenario == "ready-exact-boundary"
            else "fixed-verifier-sql644"
            if scenario == "verify-sql-private0600"
            else None
        ),
    }
    return control, manifest, DUMP


def populate(  # noqa: C901, PLR0912, PLR0915 -- Closed filesystem mutation inventory.
    root: Path, control: JsonObject, manifest: bytes, archive: bytes
) -> None:
    """Create independent equal native/reference trees, with every fixture mode explicit."""
    scenario = string(control, "scenario")
    for name in ("public", "config", "work", "state", "library", "bin"):
        directory(root / name, {"state": 0o750, "library": 0o755}.get(name, 0o700))
    for name in ("results", "releases"):
        directory(root / "public" / name)
    release = root / "public/releases" / REVISION
    source = root / "public/results" / ATTEMPT
    directory(release)
    directory(source)
    write(release / "release.json", manifest)
    write(source / "database-before.dump", archive)
    outcome: JsonObject = {
        "action": "maintain",
        "passed": scenario != "failed-release-backup",
        "revision": REVISION,
        "backupSha256": digest(archive),
        "backupMigrations": {"1": CHECKSUM},
    }
    changes: tuple[tuple[str, str, JsonValue], ...] = (
        ("outcome-revision", "revision", "INVALID"),
        ("outcome-digest", "backupSha256", "INVALID"),
        ("ledger-empty", "backupMigrations", {}),
        ("ledger-version", "backupMigrations", {"01": CHECKSUM}),
        ("ledger-checksum", "backupMigrations", {"1": "INVALID"}),
        ("ledger-not-packaged", "backupMigrations", {"2": CHECKSUM}),
    )
    for name, field, changed in changes:
        if scenario == name:
            outcome[field] = changed
    if scenario == "ledger-missing":
        del outcome["backupMigrations"]
    if scenario == "subset-ledger":
        selected = obj(decode_json(manifest))
        obj(selected, "migrations")["2"] = "f" * 96
        manifest = encoded(selected)
        (release / "release.json").unlink()
        write(release / "release.json", manifest)
    outcome_data = encoded(outcome)
    if scenario == "outcome-syntax":
        outcome_data = b"{"
    if scenario == "outcome-duplicate":
        outcome_data = outcome_data.replace(b'"revision":', b'"revision":"x","revision":')
    write(source / "outcome.json", outcome_data)
    write(root / "work/workload.lock", b"")
    write(root / "state/restore-verified.timestamp", b"UNCHANGED_PREVIOUS_FRESHNESS\n")
    write(root / "library/restore-verify.sql", (ROOT / REFERENCE_PATHS[1]).read_bytes(), 0o644)
    write(root / "control.json", encoded(control))
    write(
        root / "daemon.json",
        encoded(
            {
                "calls": [],
                "present": scenario == "prior-container",
                "selects": 0,
                "ready": 0,
                "name": "scpub-restore-" + "f" * 32,
            }
        ),
    )
    write(root / "observer-attempts.json", b"[]\n")
    write(root / "bin/docker", ("#!" + sys.executable + "\n" + ADAPTER).encode(), 0o700)
    modes = {
        "root-mode": root / "public",
        "config-mode": root / "config",
        "work-mode": root / "work",
        "state-mode": root / "state",
        "results-mode": root / "public/results",
        "releases-mode": root / "public/releases",
        "source-mode": source,
        "lock-mode": root / "work/workload.lock",
        "outcome-mode": source / "outcome.json",
        "manifest-mode": release / "release.json",
        "backup-mode": source / "database-before.dump",
    }
    if scenario in modes:
        modes[scenario].chmod(0o755)
    for name, path in (
        ("outcome", source / "outcome.json"),
        ("manifest", release / "release.json"),
        ("backup", source / "database-before.dump"),
    ):
        if scenario == name + "-symlink":
            saved = path.with_suffix(path.suffix + ".original")
            _ = path.rename(saved)
            path.symlink_to(saved.name)
    if scenario == "outcome-missing":
        (source / "outcome.json").unlink()
    if scenario in {"manifest-syntax", "manifest-duplicate", "manifest-revision"}:
        value = b"{" if scenario == "manifest-syntax" else manifest
        if scenario == "manifest-duplicate":
            value = value.replace(b'"revision":', b'"revision":"x","revision":')
        if scenario == "manifest-revision":
            parsed = obj(decode_json(manifest))
            parsed["revision"] = "b" * 40
            value = encoded(parsed)
        (release / "release.json").unlink()
        write(release / "release.json", value)
    if scenario in {"backup-empty", "backup-digest", "backup-oversized"}:
        path = source / "database-before.dump"
        path.unlink()
        write(path, b"" if scenario == "backup-empty" else archive + b"DIFFERENT")
        if scenario == "backup-oversized":
            with path.open("r+b") as stream:
                _ = stream.truncate(restore.MAX_BACKUP + 1)
    if scenario == "source-hardlink":
        os.link(source / "database-before.dump", root / "original-dump-alias")
    if scenario == "verify-sql-missing":
        (root / "library/restore-verify.sql").unlink()
    if scenario == "verify-sql-private0600":
        (root / "library/restore-verify.sql").chmod(0o600)
    if scenario == "verify-sql-hardlink":
        os.link(root / "library/restore-verify.sql", root / "original-verifier-alias")
    if scenario in {"current-unfinished", "release-unfinished", "journal-schema-bool"}:
        target = root / (
            "public/release-state.json" if scenario == "release-unfinished" else "work/current.json"
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


def normalized(value: JsonValue, root: Path) -> JsonValue:
    """Normalize only fixed fixture roots and generated restore/atomic path components."""
    if isinstance(value, dict):
        return {name: normalized(item, root) for name, item in value.items()}
    if isinstance(value, list):
        return [normalized(item, root) for item in value]
    if isinstance(value, str):
        result = value.replace(str(root), "$ROOT")
        names = [
            string_value(x)
            for x in array_value(decode_json((root / "observer-attempts.json").read_bytes()))
        ]
        for index, name in enumerate(names, 1):
            result = result.replace("/restores/" + name, f"/restores/$RESTORE-{index:03}")
        # Source-fixed UUIDv4 observations are identical across adapters. Keep
        # every container name/owner/selector exact, including foreign-looking ones.
        return result
    return value


def inventory(path: Path, root: Path) -> JsonObject:
    """Measure all product leaves; keep raw pins separately from narrow JSON comparison facts."""
    result: JsonObject = {}
    if not path.exists():
        return result
    temporary = 0
    for item in sorted(path.rglob("*")):
        metadata = item.lstat()
        if stat.S_ISDIR(metadata.st_mode):
            continue
        name = string_value(normalized(item.relative_to(root).as_posix(), root))
        if item.name.startswith(".release-"):
            temporary += 1
            name = str(Path(name).parent / f"$ATOMIC-{temporary}")
        value = pin(item)
        value["links"] = metadata.st_nlink
        if stat.S_ISREG(metadata.st_mode) and metadata.st_size <= MAX_OUTPUT:
            data = item.read_bytes()
            if (
                item.suffix == ".json"
                or item.name.startswith(".release-")
                or item.name == "restore-verified.timestamp"
            ):
                with suppress(ValueError, UnicodeError):
                    value["json"] = normalized(decode_json(data), root)
        result[name] = value
    return result


def string_value(value: JsonValue) -> str:
    """Keep local normalization typed without coercing a malformed observation."""
    if not isinstance(value, str):
        message = "restore oracle string required"
        raise TypeError(message)
    return value


def product_tree(root: Path) -> JsonObject:
    """Retain the complete public/state/work namespace, including failed atomic prefixes."""
    result: JsonObject = {}
    for subtree in ("public", "state", "work"):
        result.update(inventory(root / subtree, root))
    return result


def directory_modes(root: Path) -> JsonObject:
    """Measure physical product-directory modes independently from file payloads."""
    return {
        string_value(normalized(path.relative_to(root).as_posix(), root)): stat.S_IMODE(
            path.lstat().st_mode
        )
        for name in ("public", "state", "work", "config")
        for path in (root / name, *(root / name).rglob("*"))
        if path.is_dir() and not path.is_symlink()
    }


def lock_available(path: Path) -> bool:
    """Probe the actual canonical flock without changing or recreating its original."""
    if not path.exists():
        return True
    with path.open("rb") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        fcntl.flock(stream, fcntl.LOCK_UN)
    return True


@final
class Clock:
    """Advance only original readiness sleeps; child ownership keeps real monotonic time."""

    def __init__(self) -> None:
        """Choose one deterministic interval origin, never a new per-phase clock."""
        self.elapsed = 100.0

    def monotonic(self) -> float:
        """Observe the same virtual original interval."""
        return self.elapsed

    def sleep(self, seconds: float) -> None:
        """Consume the exact requested original polling interval without a wall sleep."""
        if seconds < 0 or seconds > 1:
            message = "unexpected restore oracle sleep"
            raise ValueError(message)
        self.elapsed += seconds


@final
class SnapshotStream:
    """Delegate real snapshot I/O while injecting one fixed before/after boundary refusal."""

    def __init__(
        self, stream: BinaryIO, scenario: str, *, outgoing: bool, faults: list[str]
    ) -> None:
        """Retain the admitted original even when a later operation fails."""
        self.stream, self.scenario, self.outgoing = stream, scenario, outgoing
        self.faults = faults

    def __enter__(self) -> Self:
        """Preserve the original with-statement ownership boundary."""
        return self

    def __exit__(self, *_error: object) -> None:
        """Close the actual original on every success or refusal."""
        self.stream.close()

    def fileno(self) -> int:
        """Borrow only the same original descriptor."""
        return self.stream.fileno()

    def read(self, size: int) -> bytes:
        """Refuse before reading the first source byte when the closed case requests it."""
        if self.scenario == "snapshot-read":
            self.faults.append(self.scenario)
            message = "INERT_SNAPSHOT_READ"
            raise OSError(message)
        return self.stream.read(size)

    def write(self, data: bytes) -> int:
        """Write actual bytes unless the explicit first-write boundary refuses."""
        if self.scenario == "snapshot-write":
            self.faults.append(self.scenario)
            message = "INERT_SNAPSHOT_WRITE"
            raise OSError(message)
        return self.stream.write(data)

    def flush(self) -> None:
        """Perform the real flush before injecting the closed after-write refusal."""
        self.stream.flush()
        if self.outgoing and self.scenario == "snapshot-flush":
            self.faults.append(self.scenario)
            message = "INERT_SNAPSHOT_FLUSH"
            raise OSError(message)


def worker(root: Path) -> JsonObject:  # noqa: C901, PLR0915 -- One complete original.
    """Execute actual cli/main, preserving all original policy and every owned child."""
    control = obj(decode_json((root / "control.json").read_bytes()))
    scenario = string(control, "scenario")
    output, error = io.StringIO(), io.StringIO()
    writes: list[JsonValue] = []
    command_limits: list[JsonValue] = []
    children: list[tuple[subprocess.Popen[bytes], BinaryIO, BinaryIO]] = []
    snapshot_streams: list[BinaryIO] = []
    original_main, original_run = restore.main, public.Runner.run
    original_init = public.Runner.__init__
    original_atomic, original_pump = public.atomic, bounded_process.pump
    original_fdopen, original_fstat = os.fdopen, os.fstat
    original_lstat, original_stat, original_fsync = os.lstat, os.stat, os.fsync
    original_path_open, original_unlink = Path.open, Path.unlink
    uid, gid = os.getuid(), os.getgid()
    raised: list[JsonValue] = []
    faults: list[str] = []
    held: BinaryIO | None = None
    active_atomic: list[tuple[Path, str]] = []
    restore_records: dict[Path, int] = {}
    in_snapshot = False
    snapshot_unlinks = 0
    clock = Clock()
    previous_mask = os.umask(0o077)
    handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}

    def root_stat(value: os.stat_result) -> os.stat_result:
        fields = list(value)
        if value.st_uid == uid:
            fields[4] = 0
        if value.st_gid == gid:
            fields[5] = 0
        return os.stat_result(
            fields,
            {
                "st_atime_ns": value.st_atime_ns,
                "st_mtime_ns": value.st_mtime_ns,
                "st_ctime_ns": value.st_ctime_ns,
            },
        )

    def lstat(path: str | os.PathLike[str], *, dir_fd: int | None = None) -> os.stat_result:
        value = original_lstat(path, dir_fd=dir_fd)
        return root_stat(value) if str(path).startswith(str(root)) else value

    def metadata(
        path: str | os.PathLike[str],
        *,
        dir_fd: int | None = None,
        follow_symlinks: bool = True,
    ) -> os.stat_result:
        value = original_stat(path, dir_fd=dir_fd, follow_symlinks=follow_symlinks)
        return root_stat(value) if str(path).startswith(str(root)) else value

    def metadata_fd(fd: int) -> os.stat_result:
        return root_stat(original_fstat(fd))

    def atomic(path: Path, data: JsonValue | bytes) -> None:
        phase = "other"
        if path.name == "restore.json":
            count = restore_records.get(path, 0) + 1
            restore_records[path] = count
            phase = {1: "initial", 2: "created"}.get(count, "final")
        elif path.name == "outcome.json":
            phase = "outcome"
        elif path.name == "restore-verified.timestamp":
            phase = "freshness"
        prefix = "restore-record-" + phase if path.name == "restore.json" else phase
        event: JsonObject = {
            "path": normalized(str(path), root),
            "data": normalized(data.decode() if isinstance(data, bytes) else data, root),
            "lockHeld": not lock_available(root / "work/workload.lock"),
            "completed": False,
        }
        writes.append(event)
        if scenario == prefix + "-write":
            faults.append(scenario)
            message = "INERT_ATOMIC_WRITE"
            raise OSError(message)
        active_atomic.append((path, prefix))
        try:
            original_atomic(path, data)
            event["completed"] = True
        finally:
            _ = active_atomic.pop()

    def fsync(fd: int) -> None:
        if in_snapshot and scenario == "snapshot-fsync":
            faults.append(scenario)
            message = "INERT_SNAPSHOT_FSYNC"
            raise OSError(message)
        if active_atomic:
            _target, phase = active_atomic[-1]
            mode = original_fstat(fd).st_mode
            kind = "directory-sync" if stat.S_ISDIR(mode) else "file-sync"
            if scenario == phase + "-" + kind:
                faults.append(scenario)
                message = "INERT_ATOMIC_FSYNC"
                raise OSError(message)
        original_fsync(fd)

    def input_fd(fd: int, mode: str) -> BinaryIO:
        stream = cast("BinaryIO", original_fdopen(fd, mode))
        if in_snapshot:
            snapshot_streams.append(stream)
            return cast(
                "BinaryIO",
                cast("object", SnapshotStream(stream, scenario, outgoing=False, faults=faults)),
            )
        return stream

    def path_open(  # noqa: PLR0913, PLR0917 -- Mirror the original SDK open signature.
        path: Path,
        mode: str = "r",
        buffering: int = -1,
        encoding: str | None = None,
        errors: str | None = None,
        newline: str | None = None,
    ) -> BinaryIO:
        stream = cast(
            "BinaryIO", original_path_open(path, mode, buffering, encoding, errors, newline)
        )
        if in_snapshot:
            snapshot_streams.append(stream)
            return cast(
                "BinaryIO",
                cast("object", SnapshotStream(stream, scenario, outgoing=True, faults=faults)),
            )
        return stream

    original_snapshot = restore.snapshot

    def snapshot(source: Path, target: Path, expected: str) -> None:
        nonlocal in_snapshot
        in_snapshot = True
        try:
            original_snapshot(source, target, expected)
        finally:
            in_snapshot = False

    def unlink(path: Path, *, missing_ok: bool = False) -> None:
        nonlocal snapshot_unlinks
        if path.name == "snapshot.dump":
            snapshot_unlinks += 1
        if scenario == "snapshot-unlink" and path.name == "snapshot.dump":
            faults.append(scenario)
            message = "INERT_SNAPSHOT_UNLINK"
            raise OSError(message)
        original_unlink(path, missing_ok=missing_ok)

    def init(runner: public.Runner, attempt: Path) -> None:
        original_init(runner, attempt)
        path = root / "observer-attempts.json"
        names: list[JsonValue] = list(array_value(decode_json(path.read_bytes())))
        names.append(attempt.name)
        with path.open("wb") as target:
            data = encoded(names)
            if target.write(data) != len(data):
                message = "incomplete restore observer index"
                raise OSError(message)

    def main() -> None:
        try:
            original_main()
        except BaseException as caught:
            raised.append({"type": type(caught).__name__, "message": str(caught)})
            raise

    def run(
        runner: public.Runner, args: Sequence[str], **options: Unpack[public.CommandOptions]
    ) -> bytes:
        input_path = options.get("input_path")
        command_limits.append(
            {
                "arguments": list(args),
                "timeout": options.get("timeout", 30),
                "inputPath": normalized(str(input_path), root) if input_path is not None else None,
                "inputBytes": len(options.get("input_data") or b""),
                "inputSha256": digest(options.get("input_data") or b""),
            }
        )
        if (
            scenario in {"interrupt-before-restore", "terminate-before-restore"}
            and "pg_restore" in args
        ):
            number = signal.SIGINT if scenario == "interrupt-before-restore" else signal.SIGTERM
            handler = signal.getsignal(number)
            if callable(handler):
                faults.append(scenario)
                handler(number, None)
        result = original_run(runner, args, **options)
        if scenario == "create-timeout-after-effect" and "create" in args:
            faults.append(scenario)
            import subprocess  # noqa: PLC0415 -- Only a fixed lower timeout observation.

            raise subprocess.TimeoutExpired(list(args), options.get("timeout", 30))
        return result

    def pump(
        child: subprocess.Popen[bytes],
        stdout: BinaryIO,
        stderr: BinaryIO,
        limits: bounded_process.Limits,
        *,
        input_data: bytes = b"",
    ) -> int:
        children.append((child, stdout, stderr))
        return original_pump(child, stdout, stderr, limits, input_data=input_data)

    codes: list[JsonValue] = []
    identifiers = iter(FIXTURE_UUIDS)
    try:
        if scenario == "lock-busy":
            held = (root / "work/workload.lock").open("r+b")
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with ExitStack() as stack:
            patches: tuple[tuple[object, str, object], ...] = (
                (public, "ROOT", root / "public"),
                (public, "CONFIG", root / "config"),
                (public, "WORK", root / "work"),
                (
                    public,
                    "DOCKER",
                    [str(root / "bin/docker"), "--host", "unix:///var/run/docker.sock"],
                ),
                (public, "atomic", atomic),
                (public, "timestamp", lambda: UTC),
                (restore, "STATE", root / "state"),
                (restore, "VERIFY_SQL", root / "library/restore-verify.sql"),
                (restore, "snapshot", snapshot),
                (restore, "main", main),
                (restore, "time", SimpleNamespace(monotonic=clock.monotonic, sleep=clock.sleep)),
                (uuid, "uuid4", lambda: next(identifiers)),
                (os, "geteuid", lambda: 1 if scenario == "root-not-root" else 0),
                (os, "lstat", lstat),
                (os, "stat", metadata),
                (os, "fstat", metadata_fd),
                (os, "fdopen", input_fd),
                (os, "fsync", fsync),
                (Path, "open", path_open),
                (Path, "unlink", unlink),
                (public.Runner, "run", run),
                (public.Runner, "__init__", init),
                (bounded_process, "pump", pump),
            )
            for owner, name, replacement in patches:
                _ = stack.enter_context(patch.object(owner, name, replacement))
            _ = stack.enter_context(redirect_stdout(output))
            _ = stack.enter_context(redirect_stderr(error))
            _ = stack.enter_context(
                patch.object(sys, "argv", ["restore_verify.py", control["attempt"]])
            )
            codes.extend(restore.cli() for _ in range(cast("int", control["repeat"])))
    finally:
        if held is not None:
            held.close()
        for number, handler in handlers.items():
            _ = signal.signal(number, handler)
        _ = os.umask(previous_mask)
    value: JsonObject = {
        "exitCodes": codes,
        "stdout": output.getvalue(),
        "stderr": error.getvalue(),
        "raised": raised,
        "faultObserved": bool(faults),
        "snapshotUnlinkAttempts": snapshot_unlinks,
        "faultEvents": cast("list[JsonValue]", list(faults)),
        "writes": writes,
        "commandLimits": command_limits,
        "children": [
            {
                "status": child.returncode,
                "pipesClosed": all(
                    stream is None or stream.closed
                    for stream in (child.stdin, child.stdout, child.stderr)
                ),
                "logsClosed": stdout.closed and stderr.closed,
            }
            for child, stdout, stderr in children
        ],
        "snapshotOriginalsClosed": all(stream.closed for stream in snapshot_streams),
        "lockReleased": lock_available(root / "work/workload.lock"),
        "virtualElapsed": clock.elapsed - 100,
        "daemon": obj(decode_json((root / "daemon.json").read_bytes())),
    }
    return obj(normalized(value, root))


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Run the complete original inside one owned worker and preserve pristine native inputs."""
    if fixture_directory is None:
        with tempfile.TemporaryDirectory(prefix="restore-oracle.") as temporary:
            return run(scenario, Path(temporary).resolve())
    root = fixture_directory
    if not root.is_absolute() or root.resolve() != root or any(root.iterdir()):
        message = "restore fixture must be fresh and canonical"
        raise ValueError(message)
    root.chmod(0o700)
    source_before: JsonObject = {name: pin(ROOT / name) for name in REFERENCE_PATHS}
    python_before = python_pins()
    baseline = baseline_pins(python_before)
    control, manifest, archive = fixture(scenario)
    for label in ("native", "reference"):
        directory(root / label)
        populate(root / label, control, manifest, archive)
    before = inventory(root / "reference/public/results" / ATTEMPT, root / "reference")
    code, stdout, stderr = bounded_process.run(
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
    if code != 0 or stderr:
        message = "closed original restore worker failed"
        raise ValueError(message)
    observed = obj(decode_json(stdout))
    after = inventory(root / "reference/public/results" / ATTEMPT, root / "reference")
    python_after = python_pins()
    observed.update(
        {
            "files": product_tree(root / "reference"),
            "directoryModes": directory_modes(root / "reference"),
            "inputPinsBefore": before,
            "inputPinsAfter": after,
            "inputUnchanged": before == after,
            "sourcePins": source_before,
            "sourceUnchanged": source_before
            == {name: pin(ROOT / name) for name in REFERENCE_PATHS},
            "originalPythonUnchanged": python_before == python_after,
            "originalPythonCount": len(python_before),
            "originalBaselinePythonPins": baseline,
            "originalBaselinePythonCount": len(baseline),
            "originalBaselinePythonUnchanged": baseline == baseline_pins(python_after),
            "workerSettled": {
                "status": code,
                "reaped": True,
                "pipesClosed": True,
                "stderrBytes": len(stderr),
            },
        }
    )
    result: JsonObject = {
        "schemaVersion": 1,
        "scenario": scenario,
        "input": control,
        "observation": observed,
    }
    if len(encoded(result)) > MAX_OUTPUT:
        message = "restore oracle output bound exceeded"
        raise ValueError(message)
    return result


class Arguments(argparse.Namespace):
    """Expose only finite fixture roles and an owned fresh export directory."""

    scenario: str | None = None
    fixture_directory: Path | None = None
    reference_worker: Path | None = None


def main(argv: list[str] | None = None) -> int:
    """Return one bounded observation without executing any engine or network role."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    _ = parser.add_argument("--scenario", choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    _ = parser.add_argument("--reference-worker", type=Path)
    values = parser.parse_args(argv, namespace=Arguments())
    if values.reference_worker is not None:
        result = worker(values.reference_worker)
    else:
        if values.scenario is None:
            parser.error("--scenario is required")
        result = run(values.scenario, values.fixture_directory)
    raw = encoded(result)
    if sys.stdout.buffer.write(raw) != len(raw):
        message = "incomplete restore oracle output"
        raise OSError(message)
    _ = sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
