"""Fixed signed HOST deploy proof inside one marked disposable Debian guest.

The original installed FD, payloads and source owners remain live across the
prepared/continue handshake. Reports are passive until all peers and VM settle.
"""

from __future__ import annotations

import base64
import fcntl
import hashlib
import json
import math
import os
import re
import selectors
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final

ROOT = Path("/root/simplestchat-vm-fixture")
PUBLIC = Path("/srv/simplestchat-public")
CONFIG = Path("/etc/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
PROJECT = ROOT / "deploy-source"
INSTALLED = LIBRARY / "simplestchat-ops"
MAX_RECORD = 131072
MAX_FRAME = 16384
MAX_DOCKER_TIMESTAMP = 64
DOCKER_ZERO_TIME = "0001-01-01T00:00:00Z"
MAX_STREAM = 2 * 1024**2
MAX_COMMANDS = 384
MAX_NATIVE_COMMANDS = 192
NATIVE_LOG_TAIL = 8
NATIVE_LOG_ENTRIES = MAX_NATIVE_COMMANDS * 2 + 64
FRAME_COUNT = 2
TRIAL_COUNT = 2
ACTOR_COUNT = 4
VALIDATOR_COUNT = 2
PRIVATE_MODE = 0o600
CADDY_MODE = 0o644
DIRECTORY_MODE = 0o700
INJECTION_SERVICE_SECONDS = 20
EVIDENCE_FILES = 1536
EVIDENCE_ARGC = 3
LEDGER_FIELDS = 3
BODY_SECONDS = 4320
SETUP_SECONDS = 600
NATIVE_SECONDS = 1800
NATIVE_FINALIZATION = 60
CONTINUE_MINIMUM = 3720
MAX_IMAGE = 2 * 1024**3
MAX_DUMP = 4 * 1024**3
DEPENDENCY_FIELDS = 5
HISTORY_LIMIT = 256
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": str(ROOT)}
PROJECT_FILES = (
    "build/release_container_fixture.py",
    "build/release_container_harness.py",
    "docker-compose.yml",
    "Caddyfile",
    *tuple(
        "ops/ansible/templates/public-" + name + ".j2"
        for name in (
            "app.env",
            "migration.env",
            "proxy.env",
            "compose.yml",
            "pg_hba.conf",
            "init-database.sql",
            "runtime-grants.sql",
            "postgres-admin-password",
        )
    ),
)
GUEST_SOURCES = (
    *PROJECT_FILES,
    "build/security_vm_stage_guest.py",
    "build/security_vm_deploy_injection.py",
)
PREPARED_FIELDS = {
    "schemaVersion",
    "kind",
    "runId",
    "revision",
    "previousRevision",
    "passed",
    "binarySha256",
    "firstImage",
    "dependencySha256",
    "migrationSha256",
}
RESULT_FIELDS = {
    "schemaVersion",
    "kind",
    "runId",
    "revision",
    "previousRevision",
    "passed",
    "cleanupPassed",
    "binarySha256",
    "firstImage",
    "currentImage",
    "trials",
    "dependenciesUnchanged",
    "markerUnchanged",
    "seedAggregateUnchanged",
    "schemaUnchanged",
    "currentSelected",
    "actorHistoryComplete",
    "ownedInjectorStoppedCandidate",
    "nativeValidatorIsolationIndependentlyInspected",
    "phaseSeconds",
    "separateProbe",
}

if __name__ == "__main__":
    sys.path[:0] = [str(ROOT), str(LIBRARY), str(PROJECT / "build")]
import security_vm_deploy_injection as injection  # noqa: E402 -- Fixed negative-only fixture.
import security_vm_ops_guest as ops_guest  # noqa: E402 -- Fixed owned source boundary.
import security_vm_stage_guest as stage  # noqa: E402 -- Fixed owned source boundary.

# isort: split
import bounded_process  # noqa: E402 -- Fixed installed original helper.
import release_artifact as artifact  # noqa: E402 -- Fixed installed original helper.
import release_assets as assets  # noqa: E402 -- Fixed installed original helper.
import release_container_fixture as render  # noqa: E402 -- Fixed projected renderer closure.
import release_container_harness as fixture  # noqa: E402 -- Fixed projected renderer closure.
import release_public as reference  # noqa: E402 -- Read-only independent policy probes only.

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

    from release_json import JsonObject

require, exact, digest, integer, record = (
    stage.require,
    stage.exact,
    stage.digest,
    stage.integer,
    stage.record,
)


# Only the package-provided mount role has Debian's exact setuid-root mode.
PREPARE_TOOLS = (
    ("docker", "/usr/bin/docker", 0o755),
    ("curl", "/usr/bin/curl", 0o755),
    ("openssl", "/usr/bin/openssl", 0o755),
    ("nsenter", "/usr/bin/nsenter", 0o755),
    ("unshare", "/usr/bin/unshare", 0o755),
    ("mount", "/usr/bin/mount", 0o4755),
    ("update-ca-certificates", "/usr/sbin/update-ca-certificates", 0o755),
)


MAX_SIGNED_EXIT = 255
DIAGNOSTIC_SCOPE = "HostDeployVmFailureFacts"
DIAGNOSTIC_PHASES = frozenset({"request", "prepare", "continue", "exercise"})
SELECTION_CLASSES = frozenset({"current", "previous", "absent", "other", "unproven"})
JOURNAL_CLASSES = frozenset({"rolled-back", "replace-unfinished", "complete", "other", "unproven"})
DIAGNOSTIC_CHECKPOINTS = frozenset(
    {
        "request",
        "prepare",
        "prepare-marker",
        "prepare-guest-source",
        "prepare-projected-source",
        "prepare-helper",
        "prepare-installed-native",
        *("prepare-tool-" + role for role, _, _ in PREPARE_TOOLS),
        "prepare-socket",
        "prepare-current-payload",
        "prepare-current-manifest",
        "prepare-previous-payload",
        "prepare-previous-manifest",
        "prepare-schema",
        "exercise",
        "negative-injection-namespace-entry",
        "negative-injection-request",
        "negative-injection-pass-through",
        "negative-injection-rollback",
        "negative-injection-inspect",
        "negative-injection-stop",
        "negative-injection-receipt",
        "negative-injection-delivery",
        "negative-injection-acknowledged",
        "negative-injection-close",
        "final-continuity",
        "separate-probe",
        "final-state",
        "cleanup",
        "body-completion",
        *(
            prefix + "-" + suffix
            for prefix in ("positive", "negative")
            for suffix in (
                "native",
                "report",
                "report-schema",
                "report-fields",
                "report-rollback",
                "report-timestamps",
                "report-backup",
                "report-evidence",
                "report-directory",
                "report-process",
                "report-attempts",
                "outcome",
                "journal",
                "staged",
                "backup",
                "selection",
                "events",
                "actors",
                "continuity",
                "retired",
            )
        ),
    }
)
DIAGNOSTIC_ROLES = frozenset(
    {
        "native-deploy",
        "docker-compose",
        "docker-image",
        "docker-inspect",
        "docker-exec",
        "docker-events",
        "docker-ps",
        "docker-stop",
        "docker-rm",
        "docker-pull",
        "docker-network",
        "docker-other",
        "curl",
        "openssl",
        "namespace-probe",
        "trust-update",
        "other-fixed-role",
    }
)
FAILURE_LABELS = {
    "native_input_refused": "native-input",
    "native_policy_refused": "native-policy",
    "native_archive_refused": "native-archive",
    "native_input_changed": "native-input-changed",
    "OSError": "filesystem",
    "native_close_refused": "native-close",
    "TimeoutExpired": "deadline",
    "Release interrupted": "interrupted",
    "command_output_limit": "output-limit",
    "ReleaseError": "release-error",
    "RuntimeProfileError": "runtime-profile",
    "asset_tar_path": "asset-tar-path",
    "AssetError": "asset-error",
    "Application readiness deadline exceeded": "readiness",
    "Insufficient backup headroom": "backup-headroom",
    "Empty live database backup": "empty-backup",
    "Running configuration differs from disk; use reviewed maintenance": "running-config",
    "Browser asset proxy configuration changed; use explicit maintenance": "browser-proxy-config",
    "Invalid deployed identity": "deployed-identity",
    "This image is already selected": "same-image",
    "Unexpected public origin": "public-origin",
    "Database must be healthy": "database-health",
    "Schema changes require the explicit maintenance deployment": "schema-change",
    "Candidate migration mismatch": "candidate-migrations",
    "Inspect retained migration container first": "migration-container",
    "Running app differs from selected image": "selected-image",
}
# Exact first-party asset failure literals; never project an arbitrary prefix-matching value.
STATIC_ASSET_FAILURES = frozenset(
    {
        "asset_capture_count",
        "asset_capture_file",
        "asset_capture_revision",
        "asset_command_owner",
        "asset_generation_count",
        "asset_generation_fields",
        "asset_generation_files",
        "asset_immutable_collision",
        "asset_index_duplicate",
        "asset_index_generations",
        "asset_index_schema",
        "asset_index_size",
        "asset_index_total",
        "asset_lifetime",
        "asset_name",
        "asset_operation_failed",
        "asset_public_files",
        "asset_public_pointer",
        "asset_public_target",
        "asset_record",
        "asset_record_fields",
        "asset_required_capacity",
        "asset_revision",
        "asset_revision_changed",
        "asset_selected_revision",
        "asset_store_digest",
        "asset_store_file_size",
        "asset_store_foreign",
        "asset_store_ownership",
        "asset_store_unindexed",
        "asset_tar_changed",
        "asset_tar_content",
        "asset_tar_content_size",
        "asset_tar_count",
        "asset_tar_duplicate",
        "asset_tar_empty",
        "asset_tar_extension",
        "asset_tar_file_size",
        "asset_tar_header_count",
        "asset_tar_names",
        "asset_tar_path",
        "asset_tar_root",
        "asset_tar_size",
        "asset_tar_total",
        "asset_tar_type",
        "asset_tar_unsupported",
        "asset_union_digest",
        "asset_union_files",
        "asset_union_size",
        "asset_version_schema",
        "asset_version_size",
        "asset_workload_busy",
    }
)
for _asset_code in STATIC_ASSET_FAILURES:
    _ = FAILURE_LABELS.setdefault(_asset_code, _asset_code)

NATIVE_FACT_KEYS = frozenset(
    {
        "nativeRawExit",
        "nativeReportState",
        "nativePassed",
        "nativeRollbackAttempted",
        "nativeRollbackPassed",
        "nativePhase",
        "nativeFailureLabel",
        "nativeCommandOrdinal",
    }
)
DIAGNOSTIC_KEYS = NATIVE_FACT_KEYS | {
    "schemaVersion",
    "scope",
    "runId",
    "phase",
    "checkpoint",
    "firstFailureKind",
    "lastObservedRole",
    "lastObservedOrdinal",
    "lastObservedExit",
    "guestSignalStatus",
    "snapshotBoundary",
    "completedTrials",
    "selectionClass",
    "journalClass",
    "nativeLogTail",
}
FAILURE_KINDS = frozenset(
    {
        "OSError",
        "ValueError",
        "RuntimeError",
        "ProcessError",
        "TimeoutExpired",
        "CheckError",
        "ToolError",
        "JSONDecodeError",
        "UnicodeDecodeError",
        "KeyError",
        "TypeError",
        "AssertionError",
        "OtherError",
    }
)


def native_facts(data: bytes | None, status: int | None) -> dict[str, object]:
    """Project only closed passive native facts, never raw failure text or live authority."""
    result: dict[str, object] = {
        "nativeRawExit": status,
        "nativeReportState": "not-observed" if data is None else "invalid",
        "nativePassed": None,
        "nativeRollbackAttempted": None,
        "nativeRollbackPassed": None,
        "nativePhase": None,
        "nativeFailureLabel": None,
        "nativeCommandOrdinal": None,
    }
    if data is None or len(data) > MAX_STREAM:
        return result
    try:
        value = record(data)
    except (ValueError, RuntimeError, RecursionError):
        return result
    if value.get("action") != "deploy":
        return result
    result["nativeReportState"] = "decoded"
    passed = value.get("passed")
    result["nativePassed"] = passed if type(passed) is bool else None
    for source, destination in (
        ("rollbackAttempted", "nativeRollbackAttempted"),
        ("rollbackPassed", "nativeRollbackPassed"),
    ):
        flag = value.get(source)
        result[destination] = flag if type(flag) is bool else None
    phase = value.get("phase")
    result["nativePhase"] = (
        phase if phase in ("stage", "preflight", "replace_application", "complete") else None
    )
    failure = value.get("failure")
    if type(failure) is str:
        result["nativeFailureLabel"] = FAILURE_LABELS.get(failure, "other-refusal")
        command = re.fullmatch(r"Command ([0-9]{3}) failed; inspect private output", failure)
        if command is not None and 0 < int(command[1]) <= MAX_NATIVE_COMMANDS:
            result["nativeFailureLabel"] = "native-command"
            result["nativeCommandOrdinal"] = int(command[1])
    return result


def command_role(args: Sequence[str]) -> str:
    """Classify only fixed source roles; never expose an argv element or path."""
    if args[:3] == ["simplestchat-ops", "release", "deploy"]:
        return "native-deploy"
    if args[: len(reference.DOCKER)] == list(reference.DOCKER):
        tail = args[len(reference.DOCKER) :]
        name = "docker-" + tail[0] if tail else "docker-other"
        return name if name in DIAGNOSTIC_ROLES else "docker-other"
    return {
        "/usr/bin/curl": "curl",
        "/usr/bin/openssl": "openssl",
        "/usr/bin/nsenter": "namespace-probe",
        "/usr/sbin/update-ca-certificates": "trust-update",
    }.get(args[0] if args else "", "other-fixed-role")


def diagnostic_enum(value: object, choices: frozenset[str] | set[str]) -> bool:
    """Reject wrong types before membership, including unhashable hostile JSON fields."""
    return type(value) is str and value in choices


def unknown_native_logs() -> dict[str, object]:
    """Keep absent/unsafe/unreadable diagnostic evidence explicitly unknown."""
    return {"state": "unavailable", "lastOrdinal": None, "rows": []}


def log_tail_projection(value: object) -> dict[str, object]:
    """Project only eight closed numbered observations, never content or content hashes."""
    tail = exact(value, {"state", "lastOrdinal", "rows"})
    require(diagnostic_enum(tail["state"], {"unavailable", "available", "partial"}))
    require(type(tail["rows"]) is list)
    rows = cast("list[object]", tail["rows"])
    require(len(rows) <= NATIVE_LOG_TAIL)
    ordinals: list[int] = []
    partial = False
    for raw in rows:
        row = exact(raw, {"ordinal", "stdout", "stderr"})
        ordinals.append(integer(row["ordinal"], 1, MAX_NATIVE_COMMANDS))
        for name in ("stdout", "stderr"):
            stream = exact(row[name], {"present", "class", "bytes"})
            present = stream["present"]
            require(present is None or type(present) is bool)
            if present is not True:
                require(stream["class"] is None and stream["bytes"] is None)
                partial |= present is None
                continue
            require(diagnostic_enum(stream["class"], {"empty", "barrier-refused", "opaque"}))
            count = integer(stream["bytes"], 0, MAX_STREAM)
            require((stream["class"] == "empty") is (count == 0))
            require(stream["class"] != "barrier-refused" or count == len(injection.FAILURE))
    require(ordinals == sorted(set(ordinals)))
    require(tail["lastOrdinal"] == (ordinals[-1] if ordinals else None))
    if tail["lastOrdinal"] is not None:
        _ = integer(tail["lastOrdinal"], 1, MAX_NATIVE_COMMANDS)
    if tail["state"] == "unavailable":
        require(not rows and tail["lastOrdinal"] is None)
    else:
        require((tail["state"] == "partial") is partial)
    return dict(tail)


def native_log_stream(directory: int, name: str, control: Control) -> dict[str, object]:
    """Read only a numeric leaf relative to the original admitted directory FD."""
    unknown: dict[str, object] = {"present": None, "class": None, "bytes": None}
    stream: BinaryIO | None = None
    result: dict[str, object] = unknown
    first: BaseException | None = None
    try:
        control.check()
        match = re.fullmatch(r"([0-9]{3})\.(stdout|stderr)", name)
        require(match is not None and 0 < int(required(match)[1]) <= MAX_NATIVE_COMMANDS)
        descriptor = os.open(
            name,
            os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
            dir_fd=directory,
        )
        stream = stage.adopt(descriptor, "rb")
        control.check()
        before = os.fstat(stream.fileno())
        require(
            stat.S_ISREG(before.st_mode)
            and before.st_uid == before.st_gid == 0
            and stat.S_IMODE(before.st_mode) == PRIVATE_MODE
            and before.st_nlink == 1
            and 0 <= before.st_size <= MAX_STREAM
        )
        data = bytearray()
        while True:
            control.check()
            chunk = stream.read(min(65536, before.st_size + 1 - len(data)))
            control.check()
            if not chunk:
                break
            data.extend(chunk)
            require(len(data) <= before.st_size)
        require(len(data) == before.st_size)
        control.check()
        require(ops_guest.stamp(os.fstat(stream.fileno())) == ops_guest.stamp(before))
        control.check()
        require(
            ops_guest.stamp(os.stat(name, dir_fd=directory, follow_symlinks=False))
            == ops_guest.stamp(before)
        )
        control.check()
        result = {
            "present": True,
            "class": "empty"
            if not data
            else "barrier-refused"
            if bytes(data) == injection.FAILURE
            else "opaque",
            "bytes": len(data),
        }
    except BaseException as error:  # noqa: BLE001 -- Every admitted stream still closes.
        first = error
    actions: list[Callable[[], None]] = []
    if stream is not None:
        actions.append(stream.close)
    actions.append(control.check)
    first = ops_guest.settle(actions, first)
    return result if first is None else unknown


def native_log_tail(attempt: Path, control: Control) -> dict[str, object]:
    """Read a bounded last-eight inventory from the original private attempt directory."""
    descriptor = -1
    scan = None
    first: BaseException | None = None
    result = unknown_native_logs()
    try:
        control.check()
        descriptor = os.open(attempt, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        before = os.fstat(descriptor)
        require(
            stat.S_ISDIR(before.st_mode)
            and before.st_uid == before.st_gid == 0
            and stat.S_IMODE(before.st_mode) == DIRECTORY_MODE
        )
        control.check()
        require(ops_guest.stamp(attempt.lstat()) == ops_guest.stamp(before))
        control.check()
        scan = os.scandir(descriptor)
        ordinals: set[int] = set()
        for count, entry in enumerate(scan, 1):
            control.check()
            require(count <= NATIVE_LOG_ENTRIES)
            match = re.fullmatch(r"([0-9]{3})\.(stdout|stderr)", entry.name)
            if match is not None:
                ordinal = int(match[1])
                require(0 < ordinal <= MAX_NATIVE_COMMANDS)
                ordinals.add(ordinal)
        control.check()
        rows: list[dict[str, object]] = []
        for ordinal in sorted(ordinals)[-NATIVE_LOG_TAIL:]:
            control.check()
            require(ops_guest.stamp(os.fstat(descriptor)) == ops_guest.stamp(before))
            require(ops_guest.stamp(attempt.lstat()) == ops_guest.stamp(before))
            rows.append(
                {
                    "ordinal": ordinal,
                    **{
                        name: native_log_stream(descriptor, f"{ordinal:03}.{name}", control)
                        for name in ("stdout", "stderr")
                    },
                }
            )
        control.check()
        require(ops_guest.stamp(os.fstat(descriptor)) == ops_guest.stamp(before))
        require(ops_guest.stamp(attempt.lstat()) == ops_guest.stamp(before))
        partial = any(
            cast("dict[str, object]", row[name])["present"] is None
            for row in rows
            for name in ("stdout", "stderr")
        )
        result = {
            "state": "partial" if partial else "available",
            "lastOrdinal": max(ordinals) if ordinals else None,
            "rows": rows,
        }
    except BaseException as error:  # noqa: BLE001 -- Read-only diagnostic remains best effort.
        first = error
    actions: list[Callable[[], None]] = []
    if scan is not None:
        actions.append(scan.close)
    if descriptor >= 0:
        actions.append(lambda: os.close(descriptor))
    actions.append(control.check)
    first = ops_guest.settle(actions, first)
    return log_tail_projection(result) if first is None else unknown_native_logs()


def failure_projection(value: object, run_id: str) -> dict[str, object]:
    """Accept one exact bounded diagnostic schema without copying unknown nested data."""
    data = exact(value, set(DIAGNOSTIC_KEYS))
    require(
        type(data["schemaVersion"]) is int
        and data["schemaVersion"] == 1
        and data["scope"] == DIAGNOSTIC_SCOPE
        and data["runId"] == run_id
        and re.fullmatch(r"[a-f0-9]{32}", run_id)
        and data["snapshotBoundary"] == "before-guest-finalizers"
    )
    require(
        diagnostic_enum(data["phase"], DIAGNOSTIC_PHASES)
        and diagnostic_enum(data["checkpoint"], DIAGNOSTIC_CHECKPOINTS)
    )
    require(
        data["firstFailureKind"] is None or diagnostic_enum(data["firstFailureKind"], FAILURE_KINDS)
    )
    require(
        data["lastObservedRole"] is None
        or diagnostic_enum(data["lastObservedRole"], DIAGNOSTIC_ROLES)
    )
    observed = integer(data["lastObservedOrdinal"], 0, MAX_COMMANDS)
    require((data["lastObservedRole"] is None) is (observed == 0))
    require(observed != 0 or data["lastObservedExit"] is None)
    _ = log_tail_projection(data["nativeLogTail"])
    _ = integer(data["completedTrials"], 0, TRIAL_COUNT)
    for key in ("lastObservedExit", "nativeRawExit"):
        require(
            data[key] is None
            or (
                type(data[key]) is int
                and -MAX_SIGNED_EXIT <= cast("int", data[key]) <= MAX_SIGNED_EXIT
            )
        )
    require(
        data["nativeReportState"] in ("not-observed", "invalid", "decoded")
        and (data["nativePassed"] is None or type(data["nativePassed"]) is bool)
        and data["nativePhase"] in (None, "stage", "preflight", "replace_application", "complete")
        and data["guestSignalStatus"] in (0, 130, 143)
        and type(data["guestSignalStatus"]) is int
    )
    labels = {*FAILURE_LABELS.values(), "other-refusal", "native-command"}
    require(
        data["nativeFailureLabel"] is None or diagnostic_enum(data["nativeFailureLabel"], labels)
    )
    require(
        all(
            data[key] is None or type(data[key]) is bool
            for key in ("nativeRollbackAttempted", "nativeRollbackPassed")
        )
    )
    require(diagnostic_enum(data["selectionClass"], SELECTION_CLASSES))
    require(diagnostic_enum(data["journalClass"], JOURNAL_CLASSES))
    ordinal = data["nativeCommandOrdinal"]
    require(ordinal is None or (type(ordinal) is int and 0 < ordinal <= MAX_NATIVE_COMMANDS))
    return dict(data)


def failure_snapshot(
    phase: str,
    owner: Fixture | None,
    first: BaseException | None,
    control: Control,
) -> dict[str, object]:
    """Capture the body failure anchor before cleanup commands can overwrite observed status."""
    commands = None if owner is None else owner.commands
    kind = None if first is None else type(first).__name__
    return {
        "schemaVersion": 1,
        "scope": DIAGNOSTIC_SCOPE,
        "runId": None if owner is None else owner.value["runId"],
        "phase": phase,
        "checkpoint": phase if owner is None else owner.checkpoint,
        "firstFailureKind": kind if kind is None or kind in FAILURE_KINDS else "OtherError",
        "lastObservedOrdinal": 0 if commands is None else commands.last_ordinal,
        "lastObservedRole": None if commands is None else commands.last_role,
        "lastObservedExit": None if commands is None else commands.last_status,
        "guestSignalStatus": control.status,
        "completedTrials": 0 if owner is None else len(owner.trials),
        "selectionClass": "unproven" if owner is None else owner.selection_class,
        "journalClass": "unproven" if owner is None else owner.journal_class,
        "nativeLogTail": unknown_native_logs() if owner is None else owner.native_log_tail,
        "snapshotBoundary": "before-guest-finalizers",
        **(native_facts(None, None) if commands is None else commands.native_observation),
    }


def failure_diagnostic(run_id: str) -> bytes:
    """Read only the fixed marker-bound diagnostic record; no private stream export."""
    require(os.geteuid() == 0 and re.fullmatch(r"[a-f0-9]{32}", run_id))
    marker, _ = ops_guest.read_fixed(Path("/etc/simplestchat-vm-fixture"), 128, PRIVATE_MODE)
    require(marker == (run_id + "\n").encode())
    data, _ = ops_guest.read_fixed(
        ROOT / "deploy-observer-diagnostic.json", MAX_FRAME, PRIVATE_MODE
    )
    return encoded(failure_projection(record(data), run_id))


def required[T](value: T | None) -> T:
    """Refuse a missing already-owned role without adding authority."""
    if value is None:
        message = "deploy_vm_owned_role_missing"
        raise ValueError(message)
    return value


def encoded(value: object) -> bytes:
    """Use one bounded LF frame; native pretty JSON stays private inside the guest."""
    data = json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    require(len(data) <= MAX_FRAME)
    return data


@final
class Control:
    """One guest entry clock, narrowed once by the controller's remaining durations."""

    def __init__(self, started: float) -> None:
        """Retain this fixed role and its original owners before checking effects."""
        self.started = started
        self.body = started + BODY_SECONDS
        self.setup_end = started + 30
        self.end = self.body + 30
        self.status = 0
        self.settling = False
        self.phase = "input"

    @property
    def body_end(self) -> float:
        """Clip work to the current phase without restarting the clock."""
        return min(self.setup_end, self.body) if self.phase != "exercise" else self.body

    def signal(self, number: int, _frame: object) -> None:
        """Latch cancellation until all admitted peers settle."""
        self.status = self.status or (130 if number == signal.SIGINT else 143)

    def check(self) -> None:
        """Fence effects against the original owner and visible originals."""
        now = time.monotonic()
        require(
            math.isfinite(now)
            and self.started <= now < (self.end if self.settling else self.body_end)
        )
        require(not self.status or self.settling)

    def configure(self, value: Mapping[str, object]) -> None:
        """Narrow this same guest owner to controller remaining durations."""
        self.check()
        require(self.phase == "input")
        setup = integer(value["setupSeconds"], 1, SETUP_SECONDS)
        body = integer(value["bodySeconds"], CONTINUE_MINIMUM, BODY_SECONDS)
        self.setup_end = self.started + setup
        self.body = min(self.body, self.started + body)
        self.end = self.body + 30
        self.phase = "setup"
        self.check()

    def advance(self) -> None:
        """Admit exactly one exercise phase without refreshing time."""
        self.check()
        self.exercise_budget()
        self.check()
        self.phase = "exercise"

    def exercise_budget(self) -> None:
        """Observe the unchanged two-call debt while the original setup phase still owns it."""
        require(self.phase == "setup" and self.body - time.monotonic() >= CONTINUE_MINIMUM)


@final
class Wire:
    """Exactly two fixed frames; EOF, replay and an expired original owner refuse."""

    def __init__(self, control: Control) -> None:
        """Retain this fixed role and its original owners before checking effects."""
        control.check()
        self.control = control
        self.selector = selectors.DefaultSelector()
        self.buffer = bytearray()
        self.count = 0
        self.closed = False
        try:
            _ = self.selector.register(sys.stdin.fileno(), selectors.EVENT_READ)
        except BaseException as error:
            first = ops_guest.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def read(self) -> dict[str, object]:
        """Read exactly one bounded fixed frame while retaining stdin."""
        self.control.check()
        require(self.count < FRAME_COUNT)
        while b"\n" not in self.buffer:
            self.control.check()
            if not self.selector.select(0.1):
                continue
            chunk = os.read(sys.stdin.fileno(), min(65536, MAX_FRAME + 1 - len(self.buffer)))
            self.control.check()
            require(chunk)
            self.buffer.extend(chunk)
            require(len(self.buffer) <= MAX_FRAME)
        data, _, tail = self.buffer.partition(b"\n")
        self.buffer = bytearray(tail)
        self.count += 1
        require(not self.buffer)
        return record(bytes(data))

    def no_extra(self) -> None:
        """Refuse EOF or extra input before either native operation."""
        self.control.check()
        require(not self.buffer)
        require(not self.selector.select(0))

    def close(self) -> None:
        """Attempt every admitted original close once."""
        if not self.closed:
            self.closed = True
            self.selector.close()


def request(data: object) -> dict[str, object]:
    """Validate only fixed scalar identities and closed original pin maps."""
    value = exact(
        data,
        {
            "schemaVersion",
            "runId",
            "revision",
            "previousRevision",
            "setupSeconds",
            "bodySeconds",
            "guestSha256",
            "sourcePins",
            "helperPins",
            "binary",
            "current",
            "previous",
        },
    )
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1)
    _ = digest(value["runId"], 32)
    _ = digest(value["revision"], 40)
    _ = digest(value["previousRevision"], 40)
    require(value["revision"] != value["previousRevision"])
    _ = digest(value["guestSha256"])
    _ = integer(value["setupSeconds"], 1, SETUP_SECONDS)
    _ = integer(value["bodySeconds"], CONTINUE_MINIMUM, BODY_SECONDS)
    for name in ("current", "previous"):
        payload = exact(value[name], {"image.tar", "release.json"})
        for key, maximum in (("image.tar", MAX_IMAGE), ("release.json", 2**20)):
            pin = exact(payload[key], {"sha256", "bytes"})
            _ = digest(pin["sha256"])
            _ = integer(pin["bytes"], 1, maximum)
    binary = exact(value["binary"], {"sha256", "bytes"})
    _ = digest(binary["sha256"])
    _ = integer(binary["bytes"], 1, ops_guest.MAX_BINARY)
    sources = exact(value["sourcePins"], set(GUEST_SOURCES))
    for sha in sources.values():
        _ = digest(sha)
    helpers = exact(
        value["helperPins"],
        set(ops_guest.BASE | ops_guest.FETCH | ops_guest.FORCE) | {"security_vm_ops_guest.py"},
    )
    for sha in helpers.values():
        _ = digest(sha)
    return value


@dataclass(frozen=True)
@final
class Command:
    """Carry an original signed status and bounded private observation."""

    code: int
    data: bytes
    error: Path

    def text(self) -> str:
        """Decode the bounded private observation without logging it."""
        return self.data.decode("utf-8").strip()


@final
class Commands:
    """A bounded fixed-fixture adapter over the mature owned process pump."""

    def __init__(self, control: Control, wire: Wire, originals: list[stage.Original]) -> None:
        """Retain this fixed role and its original owners before checking effects."""
        self.control, self.wire, self.originals = control, wire, originals
        self.sequence = 0
        self.native: stage.Original | None = None
        self.known: set[str] = set()
        self.injector: Injector | None = None
        self.polling = False
        self.first: BaseException | None = None
        self.settlement_unconfirmed = False
        self.last_failure: JsonObject | None = None
        self.cleanup_deadline: float | None = None
        self.last_status: int | None = None
        self.last_ordinal = 0
        self.last_role: str | None = None
        self.native_observation = native_facts(None, None)
        self.socket: tuple[Path, os.stat_result] | None = None

    def check(self) -> None:
        """Fence effects against the original owner and visible originals."""
        self.control.check()
        if not self.control.settling:
            self.wire.no_extra()
        if self.socket is not None:
            path, before = self.socket
            current = path.lstat()
            require(
                Path("/var/run/docker.sock").resolve(strict=True) == path
                and (
                    current.st_dev,
                    current.st_ino,
                    current.st_mode,
                    current.st_uid,
                    current.st_gid,
                )
                == (before.st_dev, before.st_ino, before.st_mode, before.st_uid, before.st_gid)
            )
        for original in self.originals:
            require(
                ops_guest.stamp(os.fstat(original.file.fileno()))
                == ops_guest.stamp(original.before)
                and ops_guest.stamp(original.path.lstat()) == ops_guest.stamp(original.before)
            )

    def healthy(self) -> bool:
        """Poll original ownership and the exact candidate injector."""
        try:
            self.check()
            if self.injector is not None:
                self.injector.check_active()
                if not self.polling:
                    self.polling = True
                    try:
                        self.injector.poll()
                    finally:
                        self.polling = False
        except (OSError, ValueError, RuntimeError) as error:
            self.first = self.first or error
            return False
        return True

    def command_end(self, outer: float) -> float:
        """Clip nested fixed injection commands to the same original service debt."""
        if self.injector is not None and self.injector.service_end is not None:
            return min(outer, self.injector.service_end)
        return outer

    def spawn(
        self,
        args: list[str],
        candidate: stage.Original | None,
        inherited: tuple[int, ...],
        source: BinaryIO | None,
    ) -> subprocess.Popen[bytes]:
        """Register the actual namespace leader before its first health callback."""
        entry = (
            self.injector.entry(candidate, args)
            if candidate is not None and self.injector is not None
            else None
        )
        child = subprocess.Popen(  # noqa: S603 -- Fixed fixture role argv; no shell.
            entry if entry is not None else args,
            executable="/proc/self/fd/" + str(candidate.file.fileno())
            if candidate is not None and entry is None
            else None,
            pass_fds=inherited,
            stdin=source or subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=ENV,
            cwd=ROOT,
            start_new_session=True,
        )
        if entry is not None:
            required(self.injector).leader_pid = child.pid
        return child

    def run(  # noqa: C901, PLR0915 -- One owned child and all peers.
        self, args: list[str], **options: object
    ) -> Command:
        """Retain the fixed run role contract."""
        # A new attempted native role cannot inherit a previous trial's passive success facts.
        self.native_observation = (
            native_facts(None, None)
            if args[:3] == ["simplestchat-ops", "release", "deploy"]
            else self.native_observation
        )
        self.check()
        self.sequence += 1
        role = command_role(args)
        sequence = self.sequence
        self.last_role, self.last_ordinal, self.last_status = role, sequence, None
        require(self.sequence <= MAX_COMMANDS)
        timeout = options.get("timeout", 30)
        require(type(timeout) in (int, float))
        self.cleanup_deadline = self.control.end if self.control.settling else self.control.body_end
        self.cleanup_deadline = self.command_end(self.cleanup_deadline)
        seconds = min(float(cast("float", timeout)), self.cleanup_deadline - time.monotonic())
        require(seconds > 0)
        success = options.get("success", True)
        require(type(success) is bool)
        source: BinaryIO | None = None
        streams: list[BinaryIO] = []
        child: subprocess.Popen[bytes] | None = None
        first: BaseException | None = None
        consumed = False
        status, data = 1, b""
        observed_status: int | None = None
        try:
            input_path = options.get("input_path")
            if input_path is not None:
                require(isinstance(input_path, Path))
                original = retain(
                    cast("Path", input_path), MAX_DUMP, 0o600, self.control, self.originals
                )
                source = original.file
                original.check()
                _ = source.seek(0)
            input_file = options.get("input_file")
            if input_file is not None:
                require(source is None and hasattr(input_file, "fileno"))
                source = cast("BinaryIO", input_file)
                _ = source.seek(0)
            for suffix in ("stdout", "stderr"):
                self.check()
                path = ROOT / f"deploy-observer-{sequence:03d}.{suffix}"
                descriptor = os.open(
                    path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600
                )
                streams.append(stage.adopt(descriptor, "w+b"))
            inherited = cast("tuple[int, ...]", options.get("pass_fds", ()))
            candidate = self.native if args and args[0] == "simplestchat-ops" else None
            if candidate is not None:
                candidate.check()
                inherited = (*inherited, candidate.file.fileno())
            self.check()
            child = self.spawn(args, candidate, inherited, source)
            status = bounded_process.pump(
                child,
                streams[0],
                streams[1],
                bounded_process.Limits(
                    timeout=seconds, stdout=MAX_STREAM, stderr=MAX_STREAM, healthy=self.healthy
                ),
            )
            consumed = True
            observed_status = status
            self.last_role, self.last_ordinal, self.last_status = role, sequence, status
            self.native_observation = (
                native_facts(None, status) if candidate is not None else self.native_observation
            )
            for stream in streams:
                stream.flush()
            _ = streams[0].seek(0)
            data = streams[0].read(MAX_STREAM + 1)
            require(len(data) <= MAX_STREAM)
            if candidate is not None:
                self.native_observation = native_facts(data, status)
            self.check()
            if candidate is not None:
                candidate.check()
        except BaseException as error:  # noqa: BLE001 -- First refusal through every peer.
            first = self.first or error
            self.settlement_unconfirmed = True
            self.last_failure = {"phase": "owned-observer", "passed": False}
            if child is not None and not consumed and child.returncode is None:
                first = ops_guest.settle((lambda: bounded_process.stop(child),), first)
            observed_status = child.returncode if child is not None else None
        finally:
            self.last_role, self.last_ordinal, self.last_status = role, sequence, observed_status
            actions = [stream.close for stream in streams]
            if child is not None:
                actions.extend(
                    stream.close
                    for stream in (child.stdin, child.stdout, child.stderr)
                    if stream is not None
                )
            first = ops_guest.settle(actions, first)
        if first is not None:
            raise first
        require(not success or status == 0)
        self.check()
        return Command(status, data, ROOT / f"deploy-observer-{sequence:03d}.stderr")

    def docker(self, *args: str, **options: object) -> Command:
        """Use the fixed original Docker command and local Unix socket."""
        return self.run([*stage.DOCKER, *args], **options)

    def compose(self, *args: str, **options: object) -> Command:
        """Use only this fixture fixed Compose project and configuration."""
        return self.docker(
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
            str(CONFIG),
            "--env-file",
            str(CONFIG / "app.env"),
            "-f",
            str(CONFIG / "compose.public.yml"),
            *args,
            **options,
        )


def retain(
    path: Path, maximum: int, mode: int, control: Control, originals: list[stage.Original]
) -> stage.Original:
    """Borrow the existing fixed Original adapter with this actual same control."""
    control.check()
    original = stage.Original(path, maximum, mode, cast("stage.Control", cast("object", control)))
    originals.append(original)
    return original


def dependency_digest(value: Mapping[str, object]) -> str:
    """Hash only the closed immutable dependency observations."""
    require(set(value) == {"postgres", "caddy"})
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


@final
class ConfigInputs:
    """Hold the deliberately mixed-UID fixture originals before candidate execution."""

    def __init__(self, control: Control) -> None:
        """Retain this fixed role and its original owners before checking effects."""
        control.check()
        self.control, self.closed = control, False
        self.files: list[ops_guest.Held] = []
        self.streams: list[BinaryIO] = []
        self.parent = ops_guest.directory(CONFIG)
        try:
            self.before = os.fstat(self.parent)
            names = {
                "migration.env": 0,
                "proxy.env": 0,
                "runtime-grants.sql": 0,
                "postgres-admin-password": 999,
                "pg_hba.conf": 999,
                "init-database.sql": 999,
                "Caddyfile": 0,
                "fixture.key": 10001,
                "fixture.crt": 10001,
            }
            for name, uid in names.items():
                mode = CADDY_MODE if name == "Caddyfile" else PRIVATE_MODE
                control.check()
                descriptor = os.open(
                    name,
                    os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                    dir_fd=self.parent,
                )
                stream = stage.adopt(descriptor, "rb")
                self.streams.append(stream)
                item = ops_guest.Held(stream, name, os.fstat(stream.fileno()), "", 0)
                self.files.append(item)
                require(
                    stat.S_ISREG(item.metadata.st_mode)
                    and item.metadata.st_uid == uid
                    and item.metadata.st_gid == uid
                    and item.metadata.st_nlink == 1
                    and stat.S_IMODE(item.metadata.st_mode) == mode
                    and 0 < item.metadata.st_size <= 2**20
                )
                item.bytes = item.metadata.st_size
                item.sha256 = ops_guest.digest_stream(stream, item.bytes, control.check)
            self.check()
        except BaseException as error:
            first = ops_guest.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def check(self) -> None:
        """Fence effects against the original owner and visible originals."""
        self.control.check()
        require(
            ops_guest.stamp(os.fstat(self.parent))[:5] == ops_guest.stamp(self.before)[:5]
            and ops_guest.stamp(CONFIG.lstat())[:5] == ops_guest.stamp(self.before)[:5]
        )
        for item in self.files:
            item.check(self.parent, self.control.check)

    def close(self) -> None:
        """Attempt every admitted original close once."""
        if not self.closed:
            self.closed = True
            first = ops_guest.settle(
                (*[stream.close for stream in self.streams], lambda: os.close(self.parent))
            )
            if first is not None:
                raise first


def selection_snapshot(control: Control) -> dict[str, bytes]:
    """Read the three mutable selections through temporary original owners before native writes."""
    originals: list[stage.Original] = []
    first: BaseException | None = None
    result: dict[str, bytes] = {}
    try:
        for name in reference.SELECTION:
            original = retain(CONFIG / name, 2**20, PRIVATE_MODE, control, originals)
            result[name] = original.data(2**20)
        for original in originals:
            original.check()
    except BaseException as error:  # noqa: BLE001 -- Preserve admission before checked closes.
        first = error
    first = ops_guest.settle(tuple(original.close for original in originals), first)
    if first is not None:
        raise first
    control.check()
    return result


@final
class MigrationProbe:
    """Borrow the unchanged Stage checksum probe through this same bounded command owner."""

    def __init__(self, commands: Commands) -> None:
        """Keep the same actual command owner and original control."""
        self.commands = commands
        self.control = commands.control

    def command(self, arguments: Sequence[str], *, timeout: float = 30) -> tuple[int, bytes]:
        """Execute only arguments supplied by the retained fixed Stage probe."""
        result = self.commands.docker(*arguments, timeout=timeout)
        return result.code, result.data


@final
@dataclass(frozen=True)
class ActorBinding:
    """Keep exact signed graph and independently retained dependency facts together."""

    graphs: Mapping[str, tuple[set[str], dict[str, object]]]
    postgres_id: str
    postgres_image: str
    token: str


@final
@dataclass(frozen=True)
class InjectionBinding:
    """Bind the one negative seam to the original trial and dependency identities."""

    image: str
    old: str
    token: str
    baseline: frozenset[str]
    before: frozenset[str]
    rollback_image: str | None = None
    selection: Mapping[str, bytes] | None = None
    dependencies: Mapping[str, object] | None = None
    protected: ConfigInputs | None = None


@final
class Injector:
    """Hold one real inspection response until the exact owned candidate is stopped.

    Only the negative native process receives a private mount namespace. Its
    fixed Docker interposer cannot forward the real running inspection before
    this parent finishes the actual stop and seals its private receipt.
    """

    def __init__(self, commands: Commands, binding: InjectionBinding) -> None:
        """Register each admitted socket before any later operation can fail."""
        self.commands, self.binding = commands, binding
        self.identifier: str | None = None
        self.stopped = False
        self.checkpoint = "negative-injection-namespace-entry"
        self.service_end: float | None = None
        self.client: socket.socket | None = None
        self.server: socket.socket | None = None
        self.socket_stamp: os.stat_result | None = None
        self.requests = 0
        self.leader_pid: int | None = None
        self.awaiting_ack = False
        self.closed = False
        self.commands.check()
        try:
            self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.commands.check()
            self.server.bind(str(injection.SOCKET))
            injection.SOCKET.chmod(PRIVATE_MODE)
            self.socket_stamp = injection.SOCKET.lstat()
            require(stat.S_ISSOCK(self.socket_stamp.st_mode))
            self.server.listen(1)
            self.server.settimeout(0)
            self.commands.check()
        except BaseException as error:
            first = ops_guest.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def write_owned(self, path: Path, data: bytes) -> stage.Original:
        """Seal a new fixed private leaf and its directory before granting its receipt."""
        self.check_active()
        directory = os.open(
            path.parent,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        output: BinaryIO | None = None
        before: os.stat_result | None = None
        first: BaseException | None = None
        try:
            self.check_active()
            before = os.fstat(directory)
            require(stat.S_ISDIR(before.st_mode) and before.st_uid == before.st_gid == 0)
            require(stat.S_IMODE(before.st_mode) == DIRECTORY_MODE)
            descriptor = os.open(
                path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
            )
            # adopt() consumes and checked-closes the raw descriptor on failure.
            output = stage.adopt(descriptor, "wb")
            self.check_active()
            require(output.write(data) == len(data))
            self.check_active()
            output.flush()
            self.check_active()
            os.fsync(output.fileno())
            self.check_active()
        except BaseException as error:  # noqa: BLE001 -- First body error precedes all closes.
            first = error
        first = ops_guest.settle((output.close,) if output is not None else (), first)
        if first is None:
            try:
                self.check_active()
                os.fsync(directory)
                self.check_active()
                current = path.parent.lstat()
                before = required(before)
                require(
                    (
                        current.st_dev,
                        current.st_ino,
                        current.st_mode,
                        current.st_uid,
                        current.st_gid,
                    )
                    == (
                        before.st_dev,
                        before.st_ino,
                        before.st_mode,
                        before.st_uid,
                        before.st_gid,
                    )
                )
            except BaseException as error:  # noqa: BLE001 -- Directory failure is still primary.
                first = error
        first = ops_guest.settle((lambda: os.close(directory),), first)
        if first is not None:
            raise first
        self.check_active()
        return retain(
            path,
            MAX_RECORD,
            PRIVATE_MODE,
            self.commands.control,
            self.commands.originals,
        )

    def entry(self, native: stage.Original, arguments: list[str]) -> list[str]:
        """Prepare the private aliases before the negative NativeTool is discovered."""
        expected = [
            "simplestchat-ops",
            "release",
            "deploy",
            "--revision",
            "",
            "--quiet-seconds",
            "0",
            "--timeout-seconds",
            str(NATIVE_SECONDS),
            "--finalization-seconds",
            str(NATIVE_FINALIZATION),
        ]
        require(len(arguments) == len(expected))
        revision = digest(arguments[4], 40)
        expected[4] = revision
        require(arguments == expected)
        original = next(
            value for value in self.commands.originals if value.path == Path("/usr/bin/docker")
        )
        original.check()
        native.check()
        _ = self.write_owned(injection.ALIAS, b"")
        config = {
            "schemaVersion": 1,
            "nonce": self.binding.token,
            "bodyEnd": self.commands.control.body_end,
            "image": self.binding.image,
            "realCliSha256": original.sha256,
            "realCliStamp": list(ops_guest.stamp(original.before)),
            "nativeSha256": native.sha256,
            "nativeStamp": list(ops_guest.stamp(native.before)),
            "namespaceInode": Path("/proc/self/ns/mnt").stat().st_ino,
        }
        _ = self.write_owned(injection.CONFIG, encoded(config))
        self.check_active()
        return [
            "/usr/bin/unshare",
            "--mount",
            "--propagation",
            "private",
            "--fork",
            "--kill-child=TERM",
            "/usr/bin/python3",
            "-I",
            "-B",
            str(injection.HELPER),
            "--enter",
            str(native.file.fileno()),
            revision,
        ]

    def check_active(self) -> None:
        """Retain the original body and refuse a vanished/extra-byte client."""
        self.commands.control.check()
        if self.service_end is None:
            return
        require(time.monotonic() < self.service_end and self.client is not None)
        try:
            # Framing sets operation timeouts; an explicit nonblocking peek
            # must not inherit Python's preceding positive socket timeout.
            required(self.client).settimeout(0)
            pending = required(self.client).recv(1, socket.MSG_PEEK | socket.MSG_DONTWAIT)
        except BlockingIOError:
            return
        if pending and self.awaiting_ack:
            return
        message = "deploy_vm_injection_peer_closed" if not pending else "deploy_vm_extra_request"
        raise ValueError(message)

    def peer_parent(self, pid: int) -> int:
        """Read one fixed bounded kernel parent fact without exporting command bytes."""
        self.check_active()
        require(type(pid) is int and pid > 0)
        descriptor = os.open(
            Path("/proc") / str(pid) / "stat",
            os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        first: BaseException | None = None
        data = b""
        try:
            self.check_active()
            data = os.read(descriptor, MAX_FRAME + 1)
            self.check_active()
        except BaseException as error:  # noqa: BLE001 -- Preserve kernel-read cause through close.
            first = error
        first = ops_guest.settle((lambda: os.close(descriptor),), first)
        if first is not None:
            raise first
        require(len(data) <= MAX_FRAME and data.startswith(str(pid).encode() + b" ("))
        _, separator, tail = data.rpartition(b") ")
        fields = tail.split()
        require(separator and len(fields) > 1 and fields[1].isdigit())
        result = int(fields[1])
        require(result > 0)
        self.check_active()
        return result

    def poll(self) -> None:
        """Service bounded fixed requests; the wrapper blocks instead of racing readiness."""
        self.check_active()
        require(self.server is not None)
        try:
            accepted = required(self.server).accept()
            client = accepted[0]
        except BlockingIOError:
            return
        self.client = client
        self.checkpoint = "negative-injection-request"
        self.service_end = min(
            self.commands.control.body_end, time.monotonic() + INJECTION_SERVICE_SECONDS
        )
        self.requests += 1
        first: BaseException | None = None
        try:
            require(self.requests <= MAX_NATIVE_COMMANDS)
            raw_peer = client.getsockopt(socket.SOL_SOCKET, 17, struct.calcsize("3i"))
            pid, uid, gid = struct.unpack("3i", raw_peer)
            require(pid > 0 and uid == gid == 0 and self.leader_pid is not None)
            client.settimeout(max(0.001, required(self.service_end) - time.monotonic()))
            request_value = exact(
                injection.receive_json(client, required(self.service_end)),
                {"schemaVersion", "nonce", "identifier"},
            )
            require(type(request_value["schemaVersion"]) is int)
            require(request_value["schemaVersion"] == 1)
            require(request_value["nonce"] == self.binding.token)
            identifier = digest(request_value["identifier"])
            self.check_active()
            require(self.peer_parent(self.peer_parent(pid)) == self.leader_pid)
            self.check_active()
            receipt_sha = self.deliver(identifier)
            acknowledged = exact(
                injection.receive_json(client, required(self.service_end)),
                {"action", "nonce", "receiptSha256"},
            )
            require(
                acknowledged
                == {
                    "action": "received",
                    "nonce": self.binding.token,
                    "receiptSha256": receipt_sha,
                }
            )
            self.awaiting_ack = False
            if receipt_sha is not None:
                self.checkpoint = "negative-injection-acknowledged"
            self.check_active()
            injection.send_json(
                client,
                {
                    "action": "complete",
                    "nonce": self.binding.token,
                    "receiptSha256": receipt_sha,
                },
                required(self.service_end),
            )
            self.check_active()
        except BaseException as error:  # noqa: BLE001 -- Preserve service through close.
            first = error
        finally:
            first = ops_guest.settle((client.close,), first)
            self.client = None
            self.service_end = None
            self.awaiting_ack = False
        if first is not None:
            raise first
        self.commands.control.check()

    def check(self) -> None:
        """Lend only this existing service/body guard to the fixed original-reader interface."""
        self.check_active()

    def check_rollback(self, identifier: str) -> None:
        """Admit only the recreated current selected app while preserving both original peers."""
        self.check_active()
        require(self.stopped and self.identifier is not None and identifier != self.identifier)
        require(
            self.binding.rollback_image is not None
            and self.binding.rollback_image != self.binding.image
        )
        require(self.binding.selection is not None and self.binding.dependencies is not None)
        require(self.binding.protected is not None)
        required(self.binding.protected).check()
        self.check_active()
        require(selection_snapshot(cast("Control", cast("object", self))) == self.binding.selection)
        self.check_active()
        attempt = active_attempt(self.binding.before, self.commands.control)
        require(
            read_record(PUBLIC / "release-state.json", self.commands.control)
            == {
                "schemaVersion": 1,
                "attempt": str(attempt),
                "phase": "replace_application",
                "finalized": False,
            }
        )
        self.check_active()
        selected = self.commands.compose("ps", "--status", "running", "--quiet", "simplestchat")
        require(digest(selected.text()) == identifier)
        owned = fixture.inspect_owned(self.commands, identifier, self.binding.token)
        labels = cast("dict[str, object]", owned["labels"])
        state = cast("dict[str, object]", owned["state"])
        require(owned["id"] == identifier and owned["image"] == self.binding.rollback_image)
        require(labels.get("com.docker.compose.project") == "simplestchat-public")
        require(labels.get(render.LABEL) == self.binding.token)
        require(labels.get("com.docker.compose.service") == "simplestchat")
        require(state.get("Running") is True and state.get("OOMKilled") is False)
        for service in ("postgres", "caddy"):
            expected = cast("list[object]", required(self.binding.dependencies)[service])
            require(type(expected) is list and len(expected) == DEPENDENCY_FIELDS)
            dependency = fixture.inspect_owned(
                self.commands, digest(expected[0]), self.binding.token
            )
            dep_labels = cast("dict[str, object]", dependency["labels"])
            dep_state = cast("dict[str, object]", dependency["state"])
            require(dep_labels.get("com.docker.compose.service") == service)
            require(dep_state.get("Running") is True and dep_state.get("OOMKilled") is False)
            require(
                [
                    *reference.stable_container(dependency),
                    digest(dep_labels.get("com.docker.compose.config-hash")),
                ]
                == expected
            )
        require(selection_snapshot(cast("Control", cast("object", self))) == self.binding.selection)
        required(self.binding.protected).check()
        self.check_active()

    def deliver(self, identifier: str) -> str | None:
        """Forward only original real output after the exact stop/receipt boundary."""
        self.check_active()
        client, deadline = required(self.client), required(self.service_end)
        if identifier in self.binding.baseline or self.stopped:
            if identifier not in self.binding.baseline:
                self.checkpoint = "negative-injection-rollback"
                self.check_rollback(identifier)
            self.checkpoint = "negative-injection-pass-through"
            self.awaiting_ack = True
            injection.send_json(client, {"action": "pass"}, deadline)
            return None
        require(self.identifier is None and identifier != self.binding.old)
        self.checkpoint = "negative-injection-inspect"
        attempt = active_attempt(self.binding.before, self.commands.control)
        journal = read_record(PUBLIC / "release-state.json", self.commands.control)
        require(
            journal
            == {
                "schemaVersion": 1,
                "attempt": str(attempt),
                "phase": "replace_application",
                "finalized": False,
            }
        )
        selected = self.commands.compose("ps", "--status", "running", "--quiet", "simplestchat")
        require(digest(selected.text()) == identifier)
        owned = fixture.inspect_owned(self.commands, identifier, self.binding.token)
        labels = cast("dict[str, object]", owned["labels"])
        require(owned["image"] == self.binding.image)
        require(labels.get("com.docker.compose.service") == "simplestchat")
        response = self.commands.docker("inspect", "--format", injection.NATIVE_FORMAT, identifier)
        value = exact(record(response.data), {"id", "image", "state", "restarts", "configHash"})
        state = cast("dict[str, object]", value["state"])
        require(
            response.code == 0
            and value["id"] == identifier
            and value["image"] == self.binding.image
            and state.get("Running") is True
            and state.get("OOMKilled") is False
        )
        stderr = retain(
            response.error,
            MAX_STREAM,
            PRIVATE_MODE,
            self.commands.control,
            self.commands.originals,
        ).data(MAX_STREAM)
        before_finished = finished_at(state.get("FinishedAt"))
        self.identifier = identifier
        self.commands.known.add(identifier)
        self.check_active()
        self.checkpoint = "negative-injection-stop"
        stopped = self.commands.docker("stop", "--time", "0", identifier, timeout=10)
        after = fixture.inspect_owned(self.commands, identifier, self.binding.token)
        after_state = cast("dict[str, object]", after["state"])
        require(
            stopped.code == 0
            and after["id"] == identifier
            and after["image"] == self.binding.image
            and after_state.get("Running") is False
        )
        after_finished = finished_at(after_state.get("FinishedAt"))
        require(after_finished not in {before_finished, DOCKER_ZERO_TIME})
        self.stopped = True
        receipt = {
            "schemaVersion": 1,
            "scope": "HostDeployInspectionBarrier",
            "nonce": self.binding.token,
            "identifier": identifier,
            "image": self.binding.image,
            "responseRawExit": response.code,
            "stdoutBytes": len(response.data),
            "stdoutSha256": hashlib.sha256(response.data).hexdigest(),
            "stderrBytes": len(stderr),
            "stderrSha256": hashlib.sha256(stderr).hexdigest(),
            "runningBefore": True,
            "runningAfter": False,
            "stopRawExit": stopped.code,
            "finishedChanged": True,
        }
        self.checkpoint = "negative-injection-receipt"
        original = self.write_owned(injection.RECEIPT, encoded(receipt))
        self.check_active()
        self.checkpoint = "negative-injection-delivery"
        self.awaiting_ack = True
        injection.send_json(
            client,
            {
                "action": "deliver",
                "stdoutBytes": len(response.data),
                "stderrBytes": len(stderr),
                "receiptSha256": original.sha256,
            },
            deadline,
        )
        self.check_active()
        injection.write_all(client, response.data, deadline)
        self.check_active()
        injection.write_all(client, stderr, deadline)
        self.check_active()
        return original.sha256

    def close(self) -> None:
        """Fence cleanup, close all sockets, and retire only the known live namespace leaf."""
        if self.closed:
            return
        self.closed = True
        body_error = ops_guest.settle((self.commands.control.check,))
        actions = [value.close for value in (self.client, self.server) if value is not None]
        first = ops_guest.settle(actions, body_error)
        if self.socket_stamp is not None and body_error is None:
            try:
                self.commands.control.check()
                require(
                    ops_guest.stamp(injection.SOCKET.lstat()) == ops_guest.stamp(self.socket_stamp)
                )
                injection.SOCKET.unlink()
                self.commands.control.check()
            except BaseException as error:  # noqa: BLE001 -- Earlier close remains first.
                first = first or error
        first = ops_guest.settle((self.commands.control.check,), first)
        if first is not None:
            raise first


def finished_at(value: object) -> str:
    """Admit the bounded Docker timestamp shape without ordering or clock authority."""
    require(isinstance(value, str))
    text = cast("str", value)
    require(
        0 < len(text) <= MAX_DOCKER_TIMESTAMP
        and re.fullmatch(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}"
            + r"(?:\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2})",
            text,
        )
    )
    return text


def active_attempt(before: frozenset[str], control: Control) -> Path:
    """Bind one exact new native attempt without interpreting its output as authority."""
    control.check()
    created = [path for path in (PUBLIC / "results").iterdir() if path.name not in before]
    require(len(created) == 1)
    attempt = created[0]
    metadata = attempt.lstat()
    require(
        re.fullmatch(r"release\.[a-f0-9]{32}", attempt.name)
        and stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == metadata.st_gid == 0
        and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
    )
    control.check()
    return attempt


@final
class Fixture:
    """Borrow mature render/TLS/SQL rules; never invoke its derivative or deploy paths."""

    def __init__(self, value: Mapping[str, object], control: Control, wire: Wire) -> None:
        """Retain this fixed role and its original owners before checking effects."""
        self.value, self.control, self.wire = value, control, wire
        self.checkpoint = "prepare"
        self.originals: list[stage.Original] = []
        self.commands = Commands(control, wire, self.originals)
        self.harness: fixture.Harness | None = None
        self.installed: stage.Original | None = None
        self.current: artifact.Manifest | None = None
        self.previous: artifact.Manifest | None = None
        self.images: dict[str, str] = {}
        self.graphs: dict[str, tuple[set[str], dict[str, object]]] = {}
        self.baseline: dict[str, object] = {}
        self.seed_snapshot = ""
        self.selected: dict[str, bytes] = {}
        self.phase_seconds: dict[str, float] = {}
        self.actors: set[str] = set()
        self.protected: ConfigInputs | None = None
        self.trials: list[dict[str, object]] = []
        self.ca: stage.Original | None = None
        self.selection_class = "unproven"
        self.journal_class = "unproven"
        self.native_log_tail = unknown_native_logs()

    def system_tools(self) -> None:
        """Retain fixed package originals with exact per-role modes, never a generic mask."""
        for role, path, mode in PREPARE_TOOLS:
            self.checkpoint = "prepare-tool-" + role
            _ = retain(Path(path), 320 * 1024**2, mode, self.control, self.originals)

    def payload(
        self, role: str, revision: str
    ) -> tuple[artifact.Manifest, set[str], dict[str, object]]:
        """Bind exact transferred bytes and identify only a closed current/previous stage."""
        require(role in ("current", "previous"))
        self.checkpoint = "prepare-" + role + "-payload"
        value = cast("dict[str, object]", self.value[role])
        directory = PUBLIC / "releases" / revision
        for name, maximum in (("release.json", 2**20), ("image.tar", MAX_IMAGE)):
            original = retain(directory / name, maximum, 0o600, self.control, self.originals)
            pin = cast("dict[str, object]", value[name])
            require(original.sha256 == pin["sha256"] and original.before.st_size == pin["bytes"])
        self.checkpoint = "prepare-" + role + "-manifest"
        self.control.check()
        manifest = artifact.validate_manifest(directory / "release.json")
        require(manifest["revision"] == revision)
        identities, configuration = stage.archive_graph(directory / "image.tar", manifest)
        self.control.check()
        for original in self.originals:
            original.check()
        return manifest, identities, configuration

    def sources(self) -> None:
        """Retain source, tools, installed binary and both signed payloads."""
        self.control.check()
        require(os.geteuid() == 0 and sys.platform == "linux")
        self.checkpoint = "prepare-marker"
        marker, _ = ops_guest.read_fixed(Path("/etc/simplestchat-vm-fixture"), 128, 0o600)
        require(marker == (str(self.value["runId"]) + "\n").encode())
        self.checkpoint = "prepare-guest-source"
        source = retain(
            ROOT / "security_vm_deploy_guest.py", 2**20, 0o600, self.control, self.originals
        )
        require(source.sha256 == self.value["guestSha256"])
        self.checkpoint = "prepare-projected-source"
        for name, sha in cast("dict[str, object]", self.value["sourcePins"]).items():
            path = (
                ROOT / Path(name).name
                if name
                in {"build/security_vm_stage_guest.py", "build/security_vm_deploy_injection.py"}
                else PROJECT / name
            )
            mode = 0o755 if name == "build/security_vm_deploy_injection.py" else 0o600
            original = retain(path, 2**20, mode, self.control, self.originals)
            require(original.sha256 == sha)
        self.checkpoint = "prepare-helper"
        for name, sha in cast("dict[str, object]", self.value["helperPins"]).items():
            path = ROOT / name if name == "security_vm_ops_guest.py" else LIBRARY / name
            original = retain(
                path,
                2**20,
                0o600 if name == "security_vm_ops_guest.py" else 0o644,
                self.control,
                self.originals,
            )
            require(original.sha256 == sha)
        self.checkpoint = "prepare-installed-native"
        self.installed = retain(
            INSTALLED, ops_guest.MAX_BINARY, 0o755, self.control, self.originals
        )
        binary = cast("dict[str, object]", self.value["binary"])
        require(
            required(self.installed).sha256 == binary["sha256"]
            and self.installed.before.st_size == binary["bytes"]
        )
        self.commands.native = self.installed
        self.system_tools()
        self.checkpoint = "prepare-socket"
        self.control.check()
        socket_path = Path("/var/run/docker.sock").resolve(strict=True)
        socket_stamp = socket_path.lstat()
        require(stat.S_ISSOCK(socket_stamp.st_mode) and socket_stamp.st_uid == 0)
        self.commands.socket = socket_path, socket_stamp
        for role, revision in (
            ("current", str(self.value["revision"])),
            ("previous", str(self.value["previousRevision"])),
        ):
            manifest, identities, config = self.payload(role, revision)
            if role == "current":
                self.current = manifest
            else:
                self.previous = manifest
            self.graphs[role] = identities, config
        self.checkpoint = "prepare-schema"
        require(
            self.current is not None
            and self.previous is not None
            and required(self.current)["migrations"] == required(self.previous)["migrations"]
        )

    def prepare(self) -> dict[str, object]:  # noqa: PLR0915 -- Ordered fixture setup.
        """Retain the fixed prepare role contract."""
        started = time.monotonic()
        self.sources()
        require(not self.commands.docker("ps", "--all", "--quiet").text())
        require(
            not self.commands.docker(
                "network",
                "ls",
                "--quiet",
                "--filter",
                "label=com.docker.compose.project=simplestchat-public",
            ).text()
        )
        require(self.previous is not None and self.current is not None)
        for path in (PUBLIC, PUBLIC / "releases", PUBLIC / "results", CONFIG):
            self.control.check()
            metadata = path.lstat()
            require(
                stat.S_ISDIR(metadata.st_mode)
                and metadata.st_uid == metadata.st_gid == 0
                and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
            )
        require(
            not (PUBLIC / "release-state.json").exists() and not (WORK / "current.json").exists()
        )
        require({p.name for p in CONFIG.iterdir()} == {"images.json"})
        canary = stage.Original(
            CONFIG / "images.json", 64, 0o600, cast("stage.Control", cast("object", self.control))
        )
        first: BaseException | None = None
        try:
            require(canary.data() == b"{}\n")
            canary.check()
        except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
            first = error
        first = ops_guest.settle((canary.close,), first)
        if first is not None:
            raise first
        self.control.check()
        require(ops_guest.stamp((CONFIG / "images.json").lstat()) == ops_guest.stamp(canary.before))
        (CONFIG / "images.json").unlink()
        for name, uid, mode in (
            ("postgres", 999, 0o700),
            ("postgres-socket", 999, 0o755),
            ("caddy-data", 10001, 0o700),
            ("caddy-config", 10001, 0o700),
        ):
            self.control.check()
            fixture.create_data_directory(PUBLIC / name, uid, mode)
            self.control.check()
        for name in ("deploy-private", "deploy-private/private"):
            self.control.check()
            (ROOT / name).mkdir(mode=0o700)
        old = required(self.previous)
        archive = PUBLIC / "releases" / old["revision"] / "image.tar"
        _ = self.commands.docker("image", "load", "--input", str(archive), timeout=300)
        observed = self.commands.docker("image", "inspect", old["imageTag"]).data
        identities, config = self.graphs["previous"]
        old_image = stage.image_profile(
            cast("object", json.loads(observed)), identities, config, old["revision"]
        )
        self.images["previous"] = old_image
        require(
            not self.commands.docker(
                "image",
                "ls",
                "--quiet",
                "--filter",
                "reference=" + required(self.current)["imageTag"],
            ).text()
        )
        require(not (PUBLIC / "releases" / old["revision"] / "staged.json").exists())
        render.render_fixture(
            CONFIG,
            PUBLIC,
            render.FixtureIdentity(
                old_image,
                old["revision"],
                str(self.value["runId"]),
                fixture.POSTGRES,
                fixture.CADDY,
            ),
        )
        self.control.check()
        _ = assets.initialize(PUBLIC / "browser-assets")
        _ = assets.select_version(PUBLIC / "browser-assets", old["revision"])
        for name in ("postgres-admin-password", "pg_hba.conf", "init-database.sql"):
            self.control.check()
            os.chown(CONFIG / name, 999, 999)
        harness = self.harness = fixture.Harness(ROOT / "deploy-private", old_image)
        harness.token = str(self.value["runId"])
        harness.commands = self.commands
        harness.certificates()
        # Match public.yml and the maintenance transition: container UID10001
        # reads only this explicitly mounted nonsecret operational file.
        self.control.check()
        os.chown(CONFIG / "Caddyfile", 0, 0)
        self.control.check()
        (CONFIG / "Caddyfile").chmod(CADDY_MODE)
        self.control.check()
        require(harness.ca_path is not None)
        self.ca = retain(required(harness.ca_path), 2**20, 0o600, self.control, self.originals)
        for image in (fixture.POSTGRES, fixture.CADDY):
            _ = self.commands.docker("pull", image, timeout=90)
        _ = self.commands.compose("config", "--quiet")
        _ = self.commands.compose(
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--wait",
            "--wait-timeout",
            "120",
            "postgres",
            timeout=135,
        )
        _ = self.commands.compose(
            "--profile",
            "maintenance",
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--no-deps",
            "migrate",
        )
        migration = cast("str", harness.service("migrate")["id"])
        self.actors.add(migration)
        harness.ready(migration=migration)
        _ = self.commands.compose(
            "--profile", "maintenance", "stop", "--timeout", "30", "migrate", timeout=45
        )
        fixture.assert_healthy_exit(fixture.inspect_owned(self.commands, migration, harness.token))
        _ = self.commands.docker("rm", migration)
        _ = harness.sql(input_path=CONFIG / "runtime-grants.sql")
        _ = harness.sql("CREATE TABLE public.release_fixture_marker(value text NOT NULL)")
        _ = harness.sql(
            "INSERT INTO public.release_fixture_marker VALUES ('"  # noqa: S608 -- Hex fixture ID.
            + harness.token
            + "')"
        )
        _ = harness.sql(
            "CREATE TABLE public.release_fixture_seed(id integer PRIMARY KEY,value text NOT NULL)"
        )
        _ = harness.sql(
            "INSERT INTO public.release_fixture_seed VALUES(1,'alpha'),(2,'beta'),(3,'gamma')"
        )
        _ = self.commands.compose(
            "up", "--detach", "--no-build", "--pull", "never", "simplestchat", "caddy", timeout=60
        )
        harness.ready()
        self.baseline = self.dependencies()
        self.seed_snapshot = self.seed()
        require(self.ledger() == old["migrations"])
        require(
            harness.sql("SELECT value FROM public.release_fixture_marker").text() == harness.token
        )
        self.selected = harness.selection()
        self.protected = ConfigInputs(self.control)
        self.phase_seconds["prepare"] = time.monotonic() - started
        return {
            "schemaVersion": 1,
            "kind": "prepared",
            "runId": self.value["runId"],
            "revision": self.value["revision"],
            "previousRevision": self.value["previousRevision"],
            "passed": True,
            "binarySha256": required(self.installed).sha256 if self.installed else "",
            "firstImage": old_image,
            "dependencySha256": dependency_digest(self.baseline),
            "migrationSha256": hashlib.sha256(
                json.dumps(old["migrations"], sort_keys=True).encode()
            ).hexdigest(),
        }

    def dependencies(self) -> dict[str, object]:
        """Capture actual dependency IDs, starts, restarts and config hashes."""
        require(self.harness is not None)
        result: dict[str, object] = {}
        for name in ("postgres", "caddy"):
            value = required(self.harness).service(name)
            labels = cast("dict[str, object]", value["labels"])
            config_hash = digest(labels.get("com.docker.compose.config-hash"))
            result[name] = [*reference.stable_container(value), config_hash]
        return result

    def ledger(self) -> dict[str, str]:
        """Read the real successful version and SHA384 migration ledger."""
        require(self.harness is not None)
        data = (
            required(self.harness)
            .sql(
                "SELECT version, success, encode(checksum, 'hex') "
                + "FROM _sqlx_migrations ORDER BY version"
            )
            .text()
        )
        result: dict[str, str] = {}
        for row in data.splitlines():
            fields = row.split("|")
            require(
                len(fields) == LEDGER_FIELDS
                and re.fullmatch(r"[1-9][0-9]*", fields[0])
                and fields[1] == "t"
                and re.fullmatch(r"[a-f0-9]{96}", fields[2])
                and fields[0] not in result
            )
            result[fields[0]] = fields[2]
        require(result)
        return result

    def seed(self) -> str:
        """Observe all fixed seed rows through a deterministic database aggregate."""
        value = (
            required(self.harness)
            .sql("SELECT count(*),sum(id),array_agg(value ORDER BY id)FROM release_fixture_seed")
            .text()
        )
        require(value == "3|6|{alpha,beta,gamma}")
        return value

    def continuity(self, revision: str) -> dict[str, object]:
        """Check current effects independently; no native JSON authorizes these facts."""
        require(self.harness is not None and self.current is not None)
        self.commands.check()
        require(self.dependencies() == self.baseline)
        require(self.ledger() == required(self.current)["migrations"])
        require(self.seed() == self.seed_snapshot)
        require(
            required(self.harness).sql("SELECT value FROM public.release_fixture_marker").text()
            == required(self.harness).token
        )
        required(self.harness).ready(seconds=15)
        app = required(self.harness).service("simplestchat")
        role = "current" if revision == self.value["revision"] else "previous"
        require(app["image"] == self.images[role])
        selected = read_record(CONFIG / "images.json", self.control)
        require(
            selected.get("revision") == revision and selected.get("serverImage") == app["image"]
        )
        version = read_record(
            PUBLIC / "browser-assets/public/app-version.json", self.control, 0o644
        )
        require(version == {"revision": revision})
        self.control.check()
        generations = assets.read_store(PUBLIC / "browser-assets", 0)
        self.control.check()
        require(any(item["revision"] == revision for item in generations))
        require(self.protected is not None)
        required(self.protected).check()
        for original in self.originals:
            original.check()
        return {"revision": revision, "image": app["image"], "appId": app["id"]}

    def backup(
        self, attempt: Path, report: Mapping[str, object], revision: str
    ) -> dict[str, object]:
        """Retain the real custom dump; a successful list is not restore proof."""
        require(self.harness is not None)
        dump = retain(
            attempt / "database-before.dump", MAX_DUMP, 0o600, self.control, self.originals
        )
        receipt = retain(
            attempt / "database-before.receipt.json",
            MAX_RECORD,
            0o600,
            self.control,
            self.originals,
        )
        value = exact(
            record(receipt.data()),
            {"schemaVersion", "dump", "bytes", "sha256", "revision", "completedAt"},
        )
        require(
            value["schemaVersion"] == 1
            and value["dump"] == "database-before.dump"
            and value["bytes"] == dump.before.st_size
            and integer(value["bytes"], 1, MAX_DUMP) > 0
            and value["sha256"] == dump.sha256 == report["backupSha256"]
            and value["revision"] == revision
            and isinstance(value["completedAt"], str)
        )
        database = digest(required(self.harness).service("postgres")["id"])
        listed = self.commands.docker(
            "exec",
            "--interactive",
            "--user",
            "999:999",
            database,
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "15s",
            "pg_restore",
            "--list",
            input_file=dump.file,
            timeout=20,
        )
        require(
            listed.code == 0
            and listed.data
            and b"TABLE DATA" in listed.data
            and b"release_fixture_marker" in listed.data
        )
        dump.check()
        receipt.check()
        require(not (attempt / "database-before.dump.partial").exists())
        return {
            "bytes": dump.before.st_size,
            "sha256": dump.sha256,
            "listed": True,
            "restoreRehearsed": False,
        }

    def recovery_diagnostic(self, before: frozenset[str]) -> None:
        """Classify two existing private records without a new query, phase or authority."""
        self.selection_class = "unproven"
        self.journal_class = "unproven"
        self.native_log_tail = unknown_native_logs()
        try:
            attempt = active_attempt(before, self.control)
            self.native_log_tail = native_log_tail(attempt, self.control)
        except BaseException:  # noqa: BLE001 -- New metadata never replaces the native refusal.
            self.native_log_tail = unknown_native_logs()
        try:
            self.control.check()
            selected = read_record(CONFIG / "images.json", self.control)
            _ = exact(selected, {"revision", "serverImage", "postgresImage", "caddyImage"})
            revision = digest(selected["revision"], 40)
            image = image_id(selected["serverImage"])
            require(
                selected["postgresImage"] == fixture.POSTGRES
                and selected["caddyImage"] == fixture.CADDY
            )
            classification = "other"
            for role, key in (("current", "revision"), ("previous", "previousRevision")):
                if revision == self.value[key] and image == self.images.get(role):
                    classification = role
            self.control.check()
            self.selection_class = classification
        except FileNotFoundError:
            self.selection_class = (
                "absent" if ops_guest.settle((self.control.check,)) is None else "unproven"
            )
        except BaseException:  # noqa: BLE001 -- Best-effort metadata never replaces the body failure.
            self.selection_class = "unproven"
        try:
            self.control.check()
            attempt = active_attempt(before, self.control)
            journal = read_record(PUBLIC / "release-state.json", self.control)
            _ = exact(journal, {"schemaVersion", "attempt", "phase", "finalized"})
            require(type(journal["schemaVersion"]) is int and journal["schemaVersion"] == 1)
            require(journal["attempt"] == str(attempt) and type(journal["finalized"]) is bool)
            require(type(journal["phase"]) is str)
            self.control.check()
            self.journal_class = {
                ("rolled_back", True): "rolled-back",
                ("replace_application", False): "replace-unfinished",
                ("complete", True): "complete",
            }.get((cast("str", journal["phase"]), cast("bool", journal["finalized"])), "other")
        except BaseException:  # noqa: BLE001 -- Late, malformed or unavailable metadata is unknown.
            self.journal_class = "unproven"

    def native_report(
        self, data: bytes, revision: str, *, negative: bool
    ) -> tuple[dict[str, object], Path]:
        """Accept actual whole pretty native JSON; final process exit remains independent."""
        prefix = "negative" if negative else "positive"
        self.checkpoint = prefix + "-report-schema"
        names = {
            "action",
            "revision",
            "startedAt",
            "passed",
            "phase",
            "finishedAt",
            "evidence",
            "backupMigrations",
            "browserAssets",
            "quietWaitRequestedSeconds",
            "quietWaitSeconds",
            "roomsActiveAtQuietCheck",
            "backupSha256",
            "roomsActiveBeforeStop",
            "roomsObservedBeforeStopAt",
            "interruptionStartedAt",
        }
        names |= (
            {"rollbackAttempted", "rollbackPassed", "failure"}
            if negative
            else {"interruptionFinishedAt"}
        )
        value = exact(record(data), names)
        self.checkpoint = prefix + "-report-fields"
        require(
            value["action"] == "deploy"
            and value["revision"] == revision
            and value["passed"] is (not negative)
            and value["phase"] == ("replace_application" if negative else "complete")
            and value["quietWaitRequestedSeconds"] == value["quietWaitSeconds"] == 0
        )
        if negative:
            self.checkpoint = prefix + "-report-rollback"
            require(
                value["rollbackAttempted"] is True
                and value["rollbackPassed"] is True
                and value["failure"] == "Application readiness deadline exceeded"
            )
        self.checkpoint = prefix + "-report-timestamps"
        for key in (
            "startedAt",
            "finishedAt",
            "interruptionStartedAt",
            "roomsObservedBeforeStopAt",
        ):
            require(
                isinstance(value[key], str)
                and re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", cast("str", value[key]))
            )
        self.checkpoint = prefix + "-report-backup"
        _ = digest(value["backupSha256"])
        self.checkpoint = prefix + "-report-evidence"
        require(isinstance(value["evidence"], str))
        attempt = Path(cast("str", value["evidence"]))
        require(
            attempt.parent == PUBLIC / "results"
            and re.fullmatch(r"release\.[a-f0-9]{32}", attempt.name)
        )
        self.checkpoint = prefix + "-report-directory"
        metadata = attempt.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == metadata.st_gid == 0
            and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
        )
        return value, attempt

    def observe_report(
        self, data: bytes, revision: str, *, negative: bool, before: frozenset[str]
    ) -> tuple[dict[str, object], Path]:
        """Keep report refusal primary while collecting only passive existing-record classes."""
        try:
            return self.native_report(data, revision, negative=negative)
        except BaseException:
            self.recovery_diagnostic(before)
            raise

    def native(  # noqa: PLR0915 -- One native operation and independent observations.
        self, revision: str, *, negative: bool
    ) -> dict[str, object]:
        """Run the actual installed FD, then bind passive records to independent effects."""
        require(
            self.installed is not None
            and self.harness is not None
            and self.current is not None
            and self.previous is not None
        )
        self.control.check()
        require(self.control.body_end - time.monotonic() >= NATIVE_SECONDS + 40)
        before = {path.name for path in (PUBLIC / "results").iterdir()}
        selected = selection_snapshot(self.control)
        started = time.monotonic()
        since = stage.utc()
        injector: Injector | None = None
        if negative:
            old = digest(required(self.harness).service("simplestchat")["id"])
            baseline = frozenset(
                {
                    old,
                    *(
                        str(cast("list[object]", self.baseline[name])[0])
                        for name in ("postgres", "caddy")
                    ),
                }
            )
            injector = Injector(
                self.commands,
                InjectionBinding(
                    self.images["previous"],
                    old,
                    required(self.harness).token,
                    baseline,
                    frozenset(before),
                    rollback_image=self.images["current"],
                    selection=selected,
                    dependencies=self.baseline,
                    protected=self.protected,
                ),
            )
            self.commands.injector = injector
        first: BaseException | None = None
        result: Command | None = None
        prefix = "negative" if negative else "positive"
        self.checkpoint = prefix + "-native"
        try:
            result = self.commands.run(
                [
                    "simplestchat-ops",
                    "release",
                    "deploy",
                    "--revision",
                    revision,
                    "--quiet-seconds",
                    "0",
                    "--timeout-seconds",
                    str(NATIVE_SECONDS),
                    "--finalization-seconds",
                    str(NATIVE_FINALIZATION),
                ],
                timeout=NATIVE_SECONDS + 10,
                success=not negative,
            )
        except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
            first = error
            if injector is not None:
                self.checkpoint = injector.checkpoint
        finally:
            if injector is not None:
                before_close = first
                first = ops_guest.settle((injector.close,), first)
                if before_close is None and first is not None:
                    self.checkpoint = "negative-injection-close"
            self.commands.injector = None
        if first is not None:
            raise first
        require(result is not None)
        result = cast("Command", result)
        until = stage.utc()
        self.checkpoint = (
            injector.checkpoint
            if injector is not None and not injector.stopped
            else prefix + "-report"
        )
        report, attempt = self.observe_report(
            result.data, revision, negative=negative, before=frozenset(before)
        )
        self.checkpoint = prefix + "-report-process"
        require((result.code != 0) if negative else result.code == 0)
        self.checkpoint = prefix + "-report-attempts"
        require({path.name for path in (PUBLIC / "results").iterdir()} - before == {attempt.name})
        self.checkpoint = prefix + "-outcome"
        outcome_original = retain(
            attempt / "outcome.json", MAX_RECORD, 0o600, self.control, self.originals
        )
        expected = dict(report)
        del expected["evidence"]
        require(record(outcome_original.data()) == expected)
        self.checkpoint = prefix + "-journal"
        journal = read_record(PUBLIC / "release-state.json", self.control)
        require(
            journal
            == {
                "schemaVersion": 1,
                "attempt": str(attempt),
                "finalized": True,
                "phase": "rolled_back" if negative else "complete",
            }
        )
        role = "previous" if negative else "current"
        manifest = required(self.previous if negative else self.current)
        self.checkpoint = prefix + "-staged"
        staged = retain(
            PUBLIC / "releases" / revision / "staged.json",
            MAX_RECORD,
            0o600,
            self.control,
            self.originals,
        )
        value = exact(
            record(staged.data()),
            {"schemaVersion", "revision", "serverImage", "manifestSha256", "stagedAt"},
        )
        require(
            value["schemaVersion"] == 1
            and value["revision"] == revision
            and value["serverImage"] in self.graphs[role][0]
            and value["manifestSha256"] == manifest_hash(self.originals, revision)
        )
        image = stage.image_profile(
            cast(
                "object",
                json.loads(self.commands.docker("image", "inspect", manifest["imageTag"]).data),
            ),
            *self.graphs[role],
            revision,
        )
        require(image == value["serverImage"])
        self.images[role] = image
        self.checkpoint = prefix + "-backup"
        retained_backup = self.backup(attempt, report, revision)
        self.checkpoint = prefix + "-selection"
        for name in reference.SELECTION:
            original = retain(
                attempt / ("before-" + name), 2**20, 0o600, self.control, self.originals
            )
            require(original.data(2**20) == selected[name])
        self.checkpoint = prefix + "-events"
        events = self.commands.docker(
            "events", "--since", since, "--until", until, "--format", "{{json .}}", timeout=15
        ).data
        postgres = cast("list[object]", self.baseline["postgres"])
        binding = ActorBinding(
            self.graphs, digest(postgres[0]), image_id(postgres[1]), required(self.harness).token
        )
        self.checkpoint = prefix + "-actors"
        history = actor_history(events, attempt, binding, self.control, self.originals)
        self.actors.update(cast("list[str]", history["actorIds"]))
        self.checkpoint = prefix + "-continuity"
        if negative:
            require(injector is not None and injector.stopped and injector.identifier is not None)
            injected_sequence(
                events,
                required(required(injector).identifier),
                self.images["previous"],
                required(self.harness).token,
            )
            require(required(self.harness).selection() == selected)
            observed = self.continuity(str(self.value["revision"]))
        else:
            observed = self.continuity(revision)
        self.checkpoint = prefix + "-retired"
        known = self.commands.docker("ps", "--all", "--quiet", "--no-trunc").text().splitlines()
        require(self.actors.isdisjoint(known))
        for original in self.originals:
            original.check()
        seconds = time.monotonic() - started
        return {
            "case": "owned-candidate-stop-rollback" if negative else "first-A-to-B",
            "passed": True,
            "exitStatus": result.code,
            "rollbackAttempted": negative,
            "rollbackPassed": negative,
            "retainedBackup": retained_backup,
            "actorHistory": history,
            "selected": observed,
            "injectedCandidateId": None if injector is None else injector.identifier,
            "manualStopObserved": False if injector is None else injector.stopped,
            "dependencyProfile": "original-docker-cli"
            if injector is None
            else "negative-inspection-response-barrier-v1",
            "seconds": seconds,
        }

    def exercise(self) -> dict[str, object]:
        """One positive and one real failed native transaction under the original owner."""
        self.control.advance()
        self.commands.check()
        require(self.control.body - time.monotonic() >= CONTINUE_MINIMUM)
        positive = self.native(str(self.value["revision"]), negative=False)
        self.trials.append(positive)
        self.phase_seconds["positive"] = float(cast("float", positive["seconds"]))
        require(self.control.body - time.monotonic() >= NATIVE_SECONDS + 40)
        negative = self.native(str(self.value["previousRevision"]), negative=True)
        self.trials.append(negative)
        self.phase_seconds["negative"] = float(cast("float", negative["seconds"]))
        observed = self.continuity(str(self.value["revision"]))
        self.checkpoint = "separate-probe"
        probe = MigrationProbe(self.commands)
        separate = stage.separate_probe(
            cast("stage.Runner", cast("object", probe)),
            self.images["current"],
            required(self.current)["migrations"],
            str(self.value["runId"]),
        )
        self.checkpoint = "final-state"
        _ = self.continuity(str(self.value["revision"]))
        require(self.protected is not None and self.installed is not None)
        required(self.protected).check()
        self.control.check()
        descriptor = os.open(WORK / "workload.lock", os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        first: BaseException | None = None
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
            first = error
        first = ops_guest.settle((lambda: os.close(descriptor),), first)
        if first is not None:
            raise first
        return {
            "schemaVersion": 1,
            "kind": "result",
            "runId": self.value["runId"],
            "revision": self.value["revision"],
            "previousRevision": self.value["previousRevision"],
            "passed": True,
            "cleanupPassed": False,
            "binarySha256": required(self.installed).sha256,
            "firstImage": self.images["previous"],
            "currentImage": observed["image"],
            "trials": self.trials,
            "dependenciesUnchanged": True,
            "markerUnchanged": True,
            "seedAggregateUnchanged": True,
            "schemaUnchanged": True,
            "currentSelected": True,
            "actorHistoryComplete": True,
            "ownedInjectorStoppedCandidate": True,
            "nativeValidatorIsolationIndependentlyInspected": False,
            "phaseSeconds": self.phase_seconds,
            "separateProbe": separate,
        }

    def finish(self, first: BaseException | None) -> BaseException | None:
        """Settle exact fixture resources, then every admitted original, preserving first."""
        if not self.control.settling:
            first = ops_guest.settle((self.control.check,), first)
        self.control.settling = True
        self.commands.injector = None
        actions: list[Callable[[], None]] = []
        if self.harness is not None:
            actions.append(self.retire_containers)
        if self.protected is not None:
            actions.extend((self.protected.check, self.protected.close))
        if self.ca is not None:
            actions.append(self.retire_ca)
        actions.extend(
            action
            for original in self.originals
            if original is not self.ca
            for action in (original.check, original.close)
        )
        actions.append(self.control.check)
        return ops_guest.settle(actions, first)

    def retire_ca(self) -> None:
        """Remove only the original generated fixture CA and settle its peer."""
        require(self.ca is not None)
        first: BaseException | None = None
        removed = False
        try:
            required(self.ca).check()
            self.control.check()
            required(self.ca).path.unlink()
            removed = True
        except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
            first = error
        self.originals.remove(required(self.ca))
        first = ops_guest.settle((required(self.ca).close,), first)
        if removed:

            def update() -> None:
                """Retain the fixed update role contract."""
                _ = self.commands.run(["/usr/sbin/update-ca-certificates"], timeout=15)

            first = ops_guest.settle((update,), first)
        if first is not None:
            raise first

    def retire_containers(self) -> None:
        """Remove only full IDs whose labels bind them to this exact disposable fixture."""
        self.control.check()
        require(self.harness is not None)
        observed = (
            self.commands.docker(
                "ps",
                "--all",
                "--quiet",
                "--no-trunc",
                "--filter",
                "label=" + render.LABEL + "=" + required(self.harness).token,
            )
            .text()
            .splitlines()
        )
        first: BaseException | None = None
        for item in observed:
            try:
                identifier = digest(item)
                _ = fixture.inspect_owned(self.commands, identifier, required(self.harness).token)
                stopped = self.commands.docker(
                    "stop", "--time", "5", identifier, timeout=10, success=False
                )
                require(stopped.code == 0)
                _ = self.commands.docker("rm", identifier)
            except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
                first = first or error
        if first is not None:
            raise first
        require(
            not self.commands.docker(
                "ps",
                "--all",
                "--quiet",
                "--filter",
                "label=" + render.LABEL + "=" + required(self.harness).token,
            ).data.strip()
        )


def read_record(path: Path, control: Control, mode: int = 0o600) -> dict[str, object]:
    """Read passive output through a temporary checked owner, never as authentication."""
    control.check()
    original = stage.Original(
        path, MAX_RECORD, mode, cast("stage.Control", cast("object", control))
    )
    value: dict[str, object] = {}
    first: BaseException | None = None
    try:
        value = record(original.data())
    except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
        first = error
    first = ops_guest.settle((original.close,), first)
    if first is not None:
        raise first
    return value


def manifest_hash(originals: Sequence[stage.Original], revision: str) -> str:
    """Select the one original authenticated manifest identity."""
    selected = [
        original
        for original in originals
        if original.path == PUBLIC / "releases" / revision / "release.json"
    ]
    require(len(selected) == 1)
    return selected[0].sha256


def native_ids(attempt: Path, control: Control, originals: list[stage.Original]) -> set[str]:
    """Inventory always-created stderr; scan only present capture stdout for actual IDs."""
    result: set[str] = set()
    captures: dict[str, dict[int, Path]] = {"stdout": {}, "stderr": {}}
    for path in sorted(attempt.iterdir()):
        match = re.fullmatch(r"([0-9]{3})\.(stdout|stderr)", path.name)
        if match is None:
            continue
        ordinal = int(match[1])
        require(0 < ordinal <= MAX_NATIVE_COMMANDS)
        captures[match[2]][ordinal] = path
    errors = sorted(captures["stderr"])
    require(
        errors
        and errors == list(range(1, len(errors) + 1))
        and captures["stdout"].keys() <= captures["stderr"].keys()
    )
    for stream, files in captures.items():
        for path in files.values():
            original = retain(path, MAX_STREAM, 0o600, control, originals)
            if stream == "stdout":
                data = original.data(MAX_STREAM).strip()
                if re.fullmatch(rb"[a-f0-9]{64}", data):
                    result.add(data.decode("ascii"))
    return result


def service_event(
    event: Mapping[str, object],
    attributes: Mapping[str, object],
    identifier: str,
    binding: ActorBinding,
) -> None:
    """Admit app lifecycle or only exact preheld PostgreSQL exec observations."""
    require(
        attributes.get("com.docker.compose.project") == "simplestchat-public"
        and attributes.get(render.LABEL) == binding.token
    )
    service = attributes.get("com.docker.compose.service")
    if service == "postgres":
        action = event.get("Action")
        require(
            identifier == binding.postgres_id
            and isinstance(action, str)
            and str(action).split(":", 1)[0] in {"exec_create", "exec_start", "exec_die"}
            and attributes.get("image") in {fixture.POSTGRES, binding.postgres_image}
        )
    else:
        require(service == "simplestchat")


def actor_history(
    data: bytes,
    attempt: Path,
    binding: ActorBinding,
    control: Control,
    originals: list[stage.Original],
) -> dict[str, object]:
    """Bind all native transient actors without guessing ordinal positions."""
    require(len(data) <= MAX_STREAM and 0 < len(data.splitlines()) < HISTORY_LIMIT)
    candidates = native_ids(attempt, control, originals)
    actors: dict[str, dict[str, object]] = {}
    order: list[str] = []
    identities = binding.graphs["current"][0] | binding.graphs["previous"][0]
    for line in data.splitlines():
        event = record(line)
        if event.get("Type") != "container":
            continue
        actor = cast("dict[str, object]", event.get("Actor"))
        attributes = cast("dict[str, object]", actor.get("Attributes"))
        name = attributes.get("name")
        require(isinstance(name, str))
        identifier = digest(actor.get("ID"))
        if not str(name).startswith(("scpub-release-validate-", "scpub-release-assets-")):
            service_event(event, attributes, identifier, binding)
            continue
        require(
            re.fullmatch(r"scpub-release-(validate|assets)-[a-f0-9]{32}", str(name))
            and identifier in candidates
            and attributes.get("image") in identities
        )
        kind = "validator" if str(name).startswith("scpub-release-validate-") else "assets"
        if identifier not in actors:
            actors[identifier] = {
                "name": name,
                "image": attributes["image"],
                "kind": kind,
                "actions": [],
            }
        current = actors[identifier]
        require(current["name"] == name and current["image"] == attributes["image"])
        action = event.get("Action")
        if action in {"create", "start", "die", "destroy"}:
            if action == "create":
                order.append(identifier)
            if action == "die":
                require(attributes.get("exitCode") == "0")
            cast("list[str]", current["actions"]).append(cast("str", action))
    require(len(actors) == ACTOR_COUNT and len(order) == ACTOR_COUNT)
    require(
        [actors[item]["kind"] for item in order] == ["validator", "validator", "assets", "assets"]
    )
    for actor in actors.values():
        require(
            actor["actions"]
            == (
                ["create", "start", "die", "destroy"]
                if actor["kind"] == "validator"
                else ["create", "destroy"]
            )
        )
    for filename, kind in (
        ("validation-name.txt", "validator"),
        ("asset-container-name.txt", "assets"),
    ):
        original = retain(attempt / filename, 128, 0o600, control, originals)
        name = original.data().decode("ascii").strip()
        last = [actors[item]["name"] for item in order if actors[item]["kind"] == kind][-1]
        require(name == last)
    return {
        "actorIds": order,
        "validatorCount": 2,
        "assetCaptureCount": 2,
        "allCreatedStartedAsRequiredExitedZeroAndRemoved": True,
        "historyRows": len(data.splitlines()),
        "nativeIsolationInspected": False,
    }


def injected_sequence(data: bytes, identifier: str, image: str, token: str) -> None:
    """Require actual start/stop/death of the exact manually stopped owned candidate."""
    actions: list[str] = []
    for line in data.splitlines():
        event = record(line)
        if event.get("Type") != "container":
            continue
        actor = cast("dict[str, object]", event.get("Actor"))
        if actor.get("ID") != identifier:
            continue
        attributes = cast("dict[str, object]", actor.get("Attributes"))
        require(attributes.get("image") == image and attributes.get(render.LABEL) == token)
        action = event.get("Action")
        if action in {"create", "start", "stop", "die", "destroy"}:
            actions.append(cast("str", action))
    require(
        actions.count("create")
        == actions.count("start")
        == actions.count("die")
        == actions.count("destroy")
        == 1
        and actions.count("stop") >= 1
        and actions.index("create")
        < actions.index("start")
        < actions.index("die")
        < actions.index("destroy")
    )


def image_id(value: object) -> str:
    """Require one full content-addressed Docker identity."""
    require(isinstance(value, str) and re.fullmatch(r"sha256:[a-f0-9]{64}", value))
    return cast("str", value)


def duration(value: object, maximum: float) -> float:
    """Accept only finite nonnegative measured phase durations."""
    require(type(value) in (int, float))
    result = float(cast("float", value))
    require(math.isfinite(result) and 0 <= result < maximum)
    return result


def public_trial(value: object, revision: str, *, negative: bool) -> dict[str, object]:
    """Never forward unknown candidate/guest fields or arbitrary error strings."""
    row = exact(
        value,
        {
            "case",
            "passed",
            "exitStatus",
            "rollbackAttempted",
            "rollbackPassed",
            "retainedBackup",
            "actorHistory",
            "selected",
            "injectedCandidateId",
            "manualStopObserved",
            "dependencyProfile",
            "seconds",
        },
    )
    require(
        row["case"] == ("owned-candidate-stop-rollback" if negative else "first-A-to-B")
        and row["passed"] is True
        and type(row["exitStatus"]) is int
        and row["exitStatus"] == (1 if negative else 0)
        and row["rollbackAttempted"] is negative
        and row["rollbackPassed"] is negative
        and row["manualStopObserved"] is negative
        and row["dependencyProfile"]
        == ("negative-inspection-response-barrier-v1" if negative else "original-docker-cli")
    )
    if negative:
        _ = digest(row["injectedCandidateId"])
    else:
        require(row["injectedCandidateId"] is None)
    backup = exact(row["retainedBackup"], {"bytes", "sha256", "listed", "restoreRehearsed"})
    _ = integer(backup["bytes"], 1, MAX_DUMP)
    _ = digest(backup["sha256"])
    require(backup["listed"] is True and backup["restoreRehearsed"] is False)
    history = exact(
        row["actorHistory"],
        {
            "actorIds",
            "validatorCount",
            "assetCaptureCount",
            "allCreatedStartedAsRequiredExitedZeroAndRemoved",
            "historyRows",
            "nativeIsolationInspected",
        },
    )
    ids = history["actorIds"]
    require(isinstance(ids, list) and len(cast("list[object]", ids)) == ACTOR_COUNT)
    validated = [digest(item) for item in cast("list[object]", ids)]
    require(
        len(set(validated)) == ACTOR_COUNT
        and type(history["validatorCount"]) is int
        and history["validatorCount"] == VALIDATOR_COUNT
        and type(history["assetCaptureCount"]) is int
        and history["assetCaptureCount"] == VALIDATOR_COUNT
        and history["allCreatedStartedAsRequiredExitedZeroAndRemoved"] is True
        and history["nativeIsolationInspected"] is False
    )
    _ = integer(history["historyRows"], 1, HISTORY_LIMIT - 1)
    selected = exact(row["selected"], {"revision", "image", "appId"})
    require(selected["revision"] == revision)
    _ = image_id(selected["image"])
    _ = digest(selected["appId"])
    _ = duration(row["seconds"], NATIVE_SECONDS + 120)
    return dict(row)


def public_result(
    value: Mapping[str, object], expected: Mapping[str, object], prepared: Mapping[str, object]
) -> dict[str, object]:
    """Project a closed completed nonsecret result, never raw guest dictionaries."""
    result = exact(value, RESULT_FIELDS)
    require(
        type(result["schemaVersion"]) is int
        and result["schemaVersion"] == 1
        and result["kind"] == "result"
        and result["passed"] is True
        and result["cleanupPassed"] is True
    )
    for key in ("runId", "revision", "previousRevision"):
        require(result[key] == expected[key])
    binary = exact(expected["binary"], {"sha256", "bytes"})
    require(
        result["binarySha256"] == binary["sha256"]
        and result["firstImage"] == prepared["firstImage"]
    )
    _ = image_id(result["firstImage"])
    _ = image_id(result["currentImage"])
    require(result["firstImage"] != result["currentImage"])
    for name in (
        "dependenciesUnchanged",
        "markerUnchanged",
        "seedAggregateUnchanged",
        "schemaUnchanged",
        "currentSelected",
        "actorHistoryComplete",
        "ownedInjectorStoppedCandidate",
    ):
        require(result[name] is True)
    require(result["nativeValidatorIsolationIndependentlyInspected"] is False)
    trials = result["trials"]
    require(isinstance(trials, list) and len(cast("list[object]", trials)) == TRIAL_COUNT)
    for index, row in enumerate(cast("list[object]", trials)):
        observed = public_trial(row, str(expected["revision"]), negative=index == 1)
        require(cast("dict[str, object]", observed["selected"])["image"] == result["currentImage"])
    phases = exact(result["phaseSeconds"], {"prepare", "positive", "negative"})
    _ = duration(phases["prepare"], SETUP_SECONDS)
    for key in ("positive", "negative"):
        _ = duration(phases[key], NATIVE_SECONDS + 120)
    probe = exact(
        result["separateProbe"],
        {
            "passed",
            "isolationIndependentlyInspected",
            "migrationMapMatched",
            "exitStatus",
            "containerId",
        },
    )
    require(
        probe["passed"] is True
        and probe["isolationIndependentlyInspected"] is True
        and probe["migrationMapMatched"] is True
        and type(probe["exitStatus"]) is int
        and probe["exitStatus"] == 0
    )
    _ = digest(probe["containerId"])
    return dict(result)


def private_evidence(run_id: str) -> bytes:
    """Read only a closed bounded diagnostic allowlist; never dump credentials or DB bytes."""
    require(os.geteuid() == 0 and re.fullmatch(r"[a-f0-9]{32}", run_id))
    marker, _ = ops_guest.read_fixed(Path("/etc/simplestchat-vm-fixture"), 128, 0o600)
    require(marker == (run_id + "\n").encode())
    paths: list[tuple[str, int, str, Path]] = []
    for number in range(1, MAX_COMMANDS + 1):
        for suffix in ("stdout", "stderr"):
            path = ROOT / f"deploy-observer-{number:03d}.{suffix}"
            if path.exists() or path.is_symlink():
                paths.append(("observer", number, suffix, path))
    attempts = sorted((PUBLIC / "results").glob("release.*"))
    require(len(attempts) <= TRIAL_COUNT)
    for ordinal, attempt in enumerate(attempts, 1):
        require(re.fullmatch(r"release\.[a-f0-9]{32}", attempt.name))
        paths.extend(
            ("native", ordinal, path.name, path)
            for path in sorted(attempt.iterdir())
            if path.name
            in {
                "outcome.json",
                "validation-name.txt",
                "asset-container-name.txt",
            }
            or re.fullmatch(r"[0-9]{3}\.(stdout|stderr)", path.name)
        )
    diagnostic = ROOT / "deploy-observer-diagnostic.json"
    if diagnostic.exists():
        paths.append(("diagnostic", 0, "fixed", diagnostic))
    require(len(paths) <= EVIDENCE_FILES)
    rows: list[dict[str, object]] = []
    for role, number, name, path in paths:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        original = stage.adopt(descriptor, "rb")
        first: BaseException | None = None
        prefix = b""
        before: os.stat_result | None = None
        try:
            before = os.fstat(original.fileno())
            require(
                stat.S_ISREG(before.st_mode)
                and before.st_uid == before.st_gid == 0
                and stat.S_IMODE(before.st_mode) == PRIVATE_MODE
                and before.st_nlink == 1
                and 0 <= before.st_size <= MAX_STREAM
            )
            prefix = original.read(512)
            require(
                ops_guest.stamp(before) == ops_guest.stamp(os.fstat(original.fileno()))
                and ops_guest.stamp(before) == ops_guest.stamp(path.lstat())
            )
        except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
            first = error
        first = ops_guest.settle((original.close,), first)
        if first is not None:
            raise first
        require(before is not None)
        before = cast("os.stat_result", before)
        rows.append(
            {
                "role": role,
                "ordinal": number,
                "stream": name,
                "bytes": before.st_size,
                "prefixBase64": base64.b64encode(prefix).decode("ascii"),
                "truncated": before.st_size > len(prefix),
            }
        )
    data = (
        json.dumps({"schemaVersion": 1, "scope": "HostDeployVmPrivateEvidence", "files": rows})
        + "\n"
    ).encode()
    require(len(data) <= MAX_STREAM)
    return data


def execute(control: Control, wire: Wire) -> dict[str, object]:
    """Keep prepared, continued operation and all finalizers under one owner."""
    fixture_owner: Fixture | None = None
    result: dict[str, object] = {}
    first: BaseException | None = None
    phase = "request"
    try:
        value = request(wire.read())
        control.configure(value)
        fixture_owner = Fixture(value, control, wire)
        phase = "prepare"
        prepared = fixture_owner.prepare()
        data = encoded(prepared)
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        phase = "continue"
        continuation = exact(wire.read(), {"schemaVersion", "kind", "runId"})
        require(
            type(continuation["schemaVersion"]) is int
            and continuation["schemaVersion"] == 1
            and continuation["kind"] == "continue"
            and continuation["runId"] == value["runId"]
        )
        phase = "exercise"
        result = fixture_owner.exercise()
    except BaseException as error:  # noqa: BLE001 -- Preserve first; settle all peers.
        first = error
    diagnostic = failure_snapshot(phase, fixture_owner, first, control)
    first = ops_guest.settle((control.check,), first)
    control.settling = True
    if diagnostic["firstFailureKind"] is None and first is not None:
        kind = type(first).__name__
        diagnostic["firstFailureKind"] = kind if kind in FAILURE_KINDS else "OtherError"
        diagnostic["checkpoint"] = "body-completion"
    if fixture_owner is not None:
        first = fixture_owner.finish(first)
    first = ops_guest.settle((wire.close, control.check), first)
    if diagnostic["firstFailureKind"] is None and first is not None:
        kind = type(first).__name__
        diagnostic["firstFailureKind"] = kind if kind in FAILURE_KINDS else "OtherError"
        diagnostic["checkpoint"] = "cleanup"

    def save_diagnostic() -> None:
        """Retain the fixed save diagnostic role contract."""
        control.check()
        _ = ops_guest.write_new(
            ROOT / "deploy-observer-diagnostic.json", (json.dumps(diagnostic) + "\n").encode()
        )
        control.check()

    first = ops_guest.settle((save_diagnostic,), first)
    if first is not None:
        raise first
    result["cleanupPassed"] = True
    return result


def emit_readonly_record() -> int:
    """Serve one of the two fixed passive records; never select an arbitrary file or action."""
    try:
        data = (
            failure_diagnostic(sys.argv[2])
            if sys.argv[1] == "--failure-diagnostic"
            else private_evidence(sys.argv[2])
        )
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
    except (OSError, ValueError, RuntimeError):
        return 1
    return 0


def main() -> int:
    """Earlier frames remain passive through final restores, cancellation and stdout checks."""
    if len(sys.argv) == EVIDENCE_ARGC and sys.argv[1] in {
        "--failure-diagnostic",
        "--private-evidence",
    }:
        return emit_readonly_record()
    require(len(sys.argv) == 1)
    control = Control(time.monotonic())
    handlers: dict[signal.Signals, object] = {}
    mask = os.umask(0o077)
    status = 1
    wire: Wire | None = None
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        wire = Wire(control)
        result = execute(control, wire)
        data = encoded(result)
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        status = 0
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        pass
    finally:
        control.settling = True
        actions: list[Callable[[], None]] = []
        if wire is not None:
            actions.append(wire.close)
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                """Retain the fixed restore role contract."""
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_mask() -> None:
            """Retain the fixed restore mask role contract."""
            _ = os.umask(mask)

        actions.extend((restore_mask, control.check))
        if ops_guest.settle(actions) is not None:
            status = 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
