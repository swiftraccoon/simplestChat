"""Own one disposable KVM Caddy prototype; never export guest materials or raw diagnostics.

Run from a trusted checkout with `python3 -B build/caddy_probe_vm.py`.
The required N/M/G values are fixture budgets, not production defaults. The
guest receives a remaining duration after staging; handoff overhead remains
inside the authoritative original controller deadline. Kernel I/O is cooperative.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shlex
import signal
import socket
import sys
import tarfile
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, cast, override

import security_vm as vm
import security_vm_fetch as fetch
import security_vm_receive as receive
from release_json import decode_json, object_value
from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from types import FrameType
    from typing import BinaryIO

SOURCE_CLOSURE = "20e5d12552abd99ef774a3b30f597ea38d9f39ff64ce11f8c4975298d9e29205"
CADDYFILE_SHA256 = "cb48cb809b5f80cb4187f3976be868c1d71a8411ad893ba0ad2b4df03ffd6039"
MAX_RECORD = 131_072
MAX_SOURCE = 2**20
MAX_SOURCE_FILES = 128
MAX_TOOL = 320 * 1024**2
# Existing bounded_process default TERM5 + consuming reap5, inside the original body window.
COMMAND_SETTLEMENT = 10.0
GUEST_SCOPE = "CaddyExactSourceDisposableGuest"
GUEST_IMAGE = (
    "ghcr.io/swiftraccoon/simplestchat-caddy:"
    + "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071"
    + "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
)
QUALIFICATION = "TLS-only; no application or maintenance success claim"
GUEST_KEYS = frozenset(
    {
        "schemaVersion",
        "scope",
        "passed",
        "cleanupPassed",
        "failure",
        "image",
        "sourceClosure",
        "materials",
        "observations",
        "containment",
        "qualification",
        "checkpoint",
        "exception",
        "cleanupException",
        "ioCheckpoint",
        "errnoCategory",
    }
)
EXPERIMENT = "caddy"
GUEST_PHASES = frozenset(
    (
        "admission",
        "guest-prerequisites",
        "source-setup",
        "image",
        "network",
        "containment",
        "module-graph",
        "natural-tls",
        "trust-install",
        "fresh-process",
        "negative-tls",
        "validator",
        "negative-copies",
        "final-observations",
        "body-completion",
        "cleanup",
        "restore",
        "final-fence",
        "complete",
    )
)
# Reviewed literal policy refusals only; never export arbitrary exception text.
GUEST_REFUSAL_CODES = frozenset(
    {
        "acmez_version",
        "binary_member",
        "binary_members",
        "binary_size",
        "caddy_version",
        "caddyfile_source",
        "certmagic_version",
        "command_capture",
        "command_count",
        "command_role",
        "command_status",
        "container_id",
        "container_image",
        "container_mount_types",
        "container_mounts",
        "container_profile",
        "container_runtime_profile",
        "container_unknown",
        "control_peer",
        "evidence_short_write",
        "external_contact_observed",
        "firewall_identity",
        "fixture_budget",
        "fixture_identity",
        "fixture_marker",
        "fixture_root",
        "foreign_container",
        "fresh_tls_changed",
        "guard_capture_missing",
        "guard_marker_short",
        "guard_not_effective",
        "guard_observation",
        "guard_retained_for_live_or_unknown_container",
        "guard_retention_unproved",
        "guest_cancelled",
        "guest_deadline",
        "guest_root_copy",
        "guest_root_copy_changed",
        "held_served_chain",
        "held_tls_chain",
        "image_digest",
        "image_platform",
        "image_storage_environment",
        "input_changed",
        "input_profile",
        "interface_identity",
        "internal_module_missing",
        "material_changed",
        "material_parent_profile",
        "material_profile",
        "material_short_read",
        "material_spelling",
        "metadata_array",
        "metadata_bound",
        "metadata_object",
        "metadata_output_short",
        "metadata_string",
        "missing_key_not_observed",
        "natural_ca_not_ready",
        "negative_material_accepted",
        "negative_tls_accepted",
        "negative_tls_status",
        "network_unknown",
        "physical_interface",
        "python_public_chain_api_missing",
        "tls_chain",
        "tls_not_ready",
        "tls_only_response",
        "tls_served_chain",
        "tool_changed",
        "tool_missing",
        "tool_profile",
        "tool_short_read",
        "unowned_tool",
        "validator_failed",
        "wire_capture_bound",
        "wire_capture_exit",
        "wire_capture_not_ready",
        "wire_capture_stopped",
        "wire_error_bound",
    }
)

GUEST_IO_CHECKPOINTS = frozenset(
    {
        "containment-route",
        "containment-rule-file",
        "containment-guard-install",
        "containment-capture-start",
        "containment-counter-before",
        "containment-marker-open",
        "containment-marker-send",
        "containment-marker-close",
        "containment-counter-after",
        "containment-counter-validate",
        "containment-capture-check",
    }
)
GUEST_ERRNO_CATEGORIES = frozenset(
    {
        "EPERM",
        "EACCES",
        "ENOENT",
        "EBADF",
        "ENOSPC",
        "EIO",
        "EINTR",
        "EPIPE",
        "EAGAIN",
        "UNKNOWN",
        "OTHER",
    }
)

FAILED_CASES: frozenset[str] = frozenset()
CHECKPOINTS = frozenset(
    {
        "admission",
        "boot",
        "stage",
        "guest",
        "inputs",
        "body-completion",
        "cleanup",
        "restore",
        "summary-output",
        "final-fence",
    }
)

EXCEPTION_KINDS = frozenset({"policy", "io", "timeout", "signal", "decode", "unexpected"})
MAX_FAILURE_RECORD = 4096
MAX_SIGNED_STATUS = 255
OBSERVED_STATUSES = tuple(range(-MAX_SIGNED_STATUS, MAX_SIGNED_STATUS + 1))


def exception_kind(error: BaseException | None) -> str | None:
    """Classify a fixed family without exposing any error text, class name or arguments."""
    if error is None:
        return None
    catalog = (
        (TimeoutError, "timeout"),
        (KeyboardInterrupt, "signal"),
        (ToolError, "policy"),
        ((ValueError, UnicodeError), "decode"),
        (OSError, "io"),
    )
    for family, kind in catalog:
        if isinstance(error, family):
            return kind
    return "unexpected"


def observe_failure(state: dict[str, object], error: BaseException, checkpoint: str) -> None:
    """Latch this actual first refusal before cleanup or later diagnostic output can fail."""
    if state.get("_first") is None:
        state["_first"] = error
        state["exception"] = exception_kind(error)
        state["failedCheckpoint"] = checkpoint


def first_error(state: dict[str, object], outer: BaseException | None) -> BaseException | None:
    """Keep a same-process exception owner; no guest JSON can grant this private state."""
    value = state.get("_first")
    return value if isinstance(value, BaseException) else outer


def enum_value(value: object, allowed: frozenset[str]) -> str | None:
    """Project only an explicitly cataloged scalar."""
    return value if isinstance(value, str) and value in allowed else None


def signed_status(value: object) -> int | None:
    """Keep an actually observed signed status distinct from bool, unknown and SDK defaults."""
    return (
        value if type(value) is int and -MAX_SIGNED_STATUS <= value <= MAX_SIGNED_STATUS else None
    )


def failure_summary(
    state: dict[str, object],
    report: dict[str, object] | None,
    outer: BaseException | None,
    *,
    restored: bool,
) -> dict[str, object]:
    """Build one bounded negative projection without copying raw/nested guest or error data."""
    row_hash = state.get("rowSha256")
    row_bytes = state.get("rowBytes")
    checkpoint = state.get("failedCheckpoint", state.get("checkpoint"))
    return {
        "schemaVersion": 1,
        "experiment": EXPERIMENT,
        "passed": False,
        "metadataOnly": True,
        "controllerCheckpoint": enum_value(checkpoint, CHECKPOINTS),
        "exception": enum_value(state.get("exception"), EXCEPTION_KINDS),
        "outerException": exception_kind(outer),
        "observedSshStatus": signed_status(state.get("sshStatus")),
        "guestCheckpoint": enum_value(state.get("guestCheckpoint"), GUEST_PHASES),
        "guestFailedCase": enum_value(state.get("guestFailedCase"), FAILED_CASES),
        "guestException": enum_value(state.get("guestException"), EXCEPTION_KINDS),
        "guestIoCheckpoint": enum_value(state.get("guestIoCheckpoint"), GUEST_IO_CHECKPOINTS),
        "guestErrnoCategory": enum_value(state.get("guestErrnoCategory"), GUEST_ERRNO_CATEGORIES),
        "guestRefusalCode": enum_value(state.get("guestRefusalCode"), GUEST_REFUSAL_CODES),
        "guestCleanupException": enum_value(state.get("guestCleanupException"), EXCEPTION_KINDS),
        "guestCleanupPassed": state.get("guestCleanupPassed")
        if type(state.get("guestCleanupPassed")) is bool
        else None,
        "guestRowAccepted": state.get("guestRowAccepted") is True,
        "rowSha256": row_hash
        if isinstance(row_hash, str) and re.fullmatch(r"[a-f0-9]{64}", row_hash)
        else None,
        "rowBytes": row_bytes
        if type(row_bytes) is int and 0 <= row_bytes <= vm.MAX_STREAM
        else None,
        "cleanupPassed": report.get("cleanupPassed")
        if report is not None and type(report.get("cleanupPassed")) is bool
        else state.get("cleanupPassed")
        if type(state.get("cleanupPassed")) is bool
        else None,
        "cleanupException": enum_value(state.get("cleanupException"), EXCEPTION_KINDS),
        "restorationPassed": restored,
        "finalFencePassed": state.get("finalFencePassed") is True,
        "controllerSignalStatus": signed_status(state.get("signalStatus")),
    }


def retain_failure(
    destination: tuple[Path, tuple[int, ...] | None, float],
    metadata: dict[str, object],
    primary: BaseException | None,
) -> BaseException | None:
    """Attempt only fixed closed metadata output; publication never changes the negative verdict."""
    raw = json.dumps(metadata, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    require(len(raw) <= MAX_FAILURE_RECORD, "failure-summary-bound")
    output, original, end = destination

    def write() -> None:
        if original is not None:
            require(time.monotonic() < end, "failure-output-deadline")
            require(fetch.directory_stamp(output.lstat()) == original, "failure-output-changed")
            write_private(output / "failure-summary.json", raw, mode=0o600)

    def console() -> None:
        text = "maintenance-prerequisite-failure " + raw.decode()
        require(sys.stderr.write(text) == len(text), "failure-console-short")
        _ = sys.stderr.flush()

    return fetch.settle((write, console), primary)


TRANSFER_SOURCES = {
    "caddy_probe_guest.py": "build/caddy_probe_guest.py",
    "caddy_materials.py": "build/caddy_materials.py",
    "bounded_process.py": "ops/ansible/files/bounded_process.py",
    "Caddyfile": "Caddyfile",
}
TRANSFER_NAMES = tuple(TRANSFER_SOURCES)
SOURCE_NAMES = (
    "build/security_vm.py",
    "build/security_vm_fetch.py",
    "build/security_vm_receive.py",
    "build/security_vm_fetch_guest.py",
    "build/security_vm_fetch_hosts.py",
    "build/security_vm_qmp.py",
    "build/security_context.py",
    "build/security_tools.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_json.py",
    "security/vm/debian-cloud.json",
)
TOOL_NAMES = (
    "ssh",
    "curl",
    "ssh-keygen",
    "cloud-localds",
    "qemu-img",
    "timeout",
    "prlimit",
    "qemu-system-x86_64",
)


@dataclass
class Control(fetch.Control):
    """Keep the original entry clock, body reserve, and latched signals through final output."""

    whole_seconds: float = 0
    finalization_seconds: float = 0

    @property
    @override
    def end(self) -> float:
        """Clip every phase to the explicit original N."""
        return self.started + self.whole_seconds

    @property
    @override
    def body_end(self) -> float:
        """Keep explicit M for VM, input, work, and outer restoration."""
        return self.end - self.finalization_seconds


class Host(receive.Host):
    """Reuse the original bounded process/VM owner and original regular-file stdin loan."""

    def __init__(self, output: Path, control: Control) -> None:
        """Reserve existing child settlement before the independently reserved VM tail."""
        control.check()
        require(control.body_end - time.monotonic() > COMMAND_SETTLEMENT, "caddy_vm_body_budget")
        super().__init__(output, control)
        self.deadline: float = control.body_end - COMMAND_SETTLEMENT
        self.diagnostic: dict[str, object] = {}

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
        input_file: BinaryIO | None = None,
    ) -> tuple[int, bytes]:
        """Dispatch only retained canonical fixed tools, including every bootstrap SSH use."""
        require(bool(argv), "caddy_vm_command")
        role = Path(argv[0]).name
        original = self.fixed_tools.get(role)
        if original is None:
            original = next(
                (held for held in self.fixed_tools.values() if str(held.path) == argv[0]), None
            )
        require(original is not None, "caddy_vm_command")
        if original is None:
            message = "caddy_vm_command"
            raise ToolError(message)
        original.check(self.control)
        command = [str(original.path), *argv[1:]]
        result = super().run(
            name, command, timeout=timeout, accepted=accepted, input_file=input_file
        )
        original.check(self.control)
        return result


def positive(value: str) -> float:
    """Reject unbounded, ambiguous or nonpositive fixture intervals."""
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        message = "expected a finite positive duration"
        raise argparse.ArgumentTypeError(message)
    return number


@dataclass
class Options(argparse.Namespace):
    """Select only original source and a fresh private evidence directory."""

    project_root: Path = Path()
    output: Path = Path()
    timeout_seconds: float = 0
    finalization_seconds: float = 0
    guest_finalization_seconds: float = 0
    output_stamp: tuple[int, ...] | None = None
    diagnostic: dict[str, object] = field(default_factory=dict)


def options(arguments: Sequence[str] | None) -> Options:
    """Require all budgets explicitly before admitting files or a VM."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    for name in ("project-root", "output"):
        _ = parser.add_argument("--" + name, required=True, type=Path)
    for name in ("timeout-seconds", "finalization-seconds", "guest-finalization-seconds"):
        _ = parser.add_argument("--" + name, required=True, type=positive)
    selected = parser.parse_args(arguments, namespace=Options())
    if not (
        selected.finalization_seconds < selected.timeout_seconds <= vm.VM_SECONDS
        and selected.guest_finalization_seconds
        < selected.timeout_seconds - selected.finalization_seconds
    ):
        parser.error("N must be <=1800, 0<M<N, and 0<G<N-M")
    return selected


def guest_record(raw: bytes, image: str) -> dict[str, object]:
    """Validate the entire bounded row, projecting none of its nested or unknown data."""
    require(0 < len(raw) <= MAX_RECORD and raw.endswith(b"\n"), "caddy_vm_guest_record")
    require(raw.count(b"\n") == 1, "caddy_vm_guest_record")
    value = object_value(decode_json(raw))
    require(frozenset(value) == GUEST_KEYS, "caddy_vm_guest_record")
    require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["scope"] == GUEST_SCOPE
        and value["sourceClosure"] == SOURCE_CLOSURE
        and value["qualification"] == QUALIFICATION
        and value["image"] == image,
        "caddy_vm_guest_identity",
    )
    require(
        value["passed"] is True
        and value["cleanupPassed"] is True
        and value["failure"] is None
        and isinstance(value["materials"], dict)
        and isinstance(value["observations"], list)
        and isinstance(value["containment"], dict),
        "caddy_vm_guest_refused",
    )
    return {"passed": True, "cleanupPassed": True, "rowSha256": hashlib.sha256(raw).hexdigest()}


def guest_failure_record(raw: bytes, image: str) -> dict[str, object]:
    """Retain only fixed identity/phase/error scalars from a bounded negative guest row."""
    result: dict[str, object] = {
        "rowBytes": len(raw),
        "rowSha256": hashlib.sha256(raw).hexdigest(),
        "guestRowAccepted": False,
    }
    try:
        require(0 < len(raw) <= MAX_RECORD and raw.endswith(b"\n"), "caddy_vm_guest_record")
        require(raw.count(b"\n") == 1, "caddy_vm_guest_record")
        value = object_value(decode_json(raw))
        require(
            frozenset(value) == GUEST_KEYS
            and type(value["schemaVersion"]) is int
            and value["schemaVersion"] == 1
            and value["scope"] == GUEST_SCOPE
            and value["sourceClosure"] == SOURCE_CLOSURE
            and value["image"] == image
            and value["qualification"] == QUALIFICATION,
            "caddy_vm_guest_identity",
        )
        checkpoint = enum_value(value["checkpoint"], GUEST_PHASES)
        error = enum_value(value["exception"], EXCEPTION_KINDS)
        require(
            checkpoint is not None
            and type(value["passed"]) is bool
            and type(value["cleanupPassed"]) is bool
            and (value["exception"] is None or error is not None),
            "caddy_vm_guest_diagnostic",
        )
        result.update(
            {
                "guestRowAccepted": True,
                "guestCheckpoint": checkpoint,
                "guestException": error,
                "guestIoCheckpoint": enum_value(value["ioCheckpoint"], GUEST_IO_CHECKPOINTS)
                if checkpoint == "containment"
                else None,
                "guestErrnoCategory": enum_value(value["errnoCategory"], GUEST_ERRNO_CATEGORIES),
                "guestRefusalCode": enum_value(value["failure"], GUEST_REFUSAL_CODES),
                "guestCleanupException": enum_value(value["cleanupException"], EXCEPTION_KINDS),
                "guestCleanupPassed": value["cleanupPassed"],
            }
        )
    except (OSError, ValueError, ToolError, RecursionError):
        pass
    return result


def ssh(host: Host, work: Path, port: int, command: Sequence[str]) -> list[str]:
    """Retain the existing generated-host pin and fixed loopback destination."""
    return [
        str(host.fixed_tools["ssh"].path),
        *vm.ssh_options(work),
        "-i",
        str(work / "client"),
        "-p",
        str(port),
        "fixture@127.0.0.1",
        shlex.join(command),
    ]


def retain_sources(
    selected: Options, control: Control, owners: fetch.Owners
) -> dict[str, fetch.Held]:
    """Bind actual imported helpers and the four transfer originals before any VM effect."""
    require(
        selected.project_root.is_absolute()
        and selected.project_root.resolve() == selected.project_root
        and selected.project_root == ROOT,
        "caddy_vm_project_root",
    )
    paths = {name: selected.project_root / name for name in SOURCE_NAMES}
    for name in SOURCE_NAMES:
        module_name = Path(name).stem
        module = sys.modules.get(module_name)
        if module is not None:
            require(
                isinstance(module.__file__, str)
                and Path(module.__file__).resolve() == selected.project_root / name,
                "caddy_vm_import_source",
            )
    for module in tuple(sys.modules.values()):
        filename = getattr(module, "__file__", None)
        if not isinstance(filename, str):
            continue
        path = Path(filename).resolve()
        if not path.is_relative_to(selected.project_root):
            continue
        relative = path.relative_to(selected.project_root)
        if path.suffix == ".py" and (
            relative.parts[0] == "build" or relative.parts[:3] == ("ops", "ansible", "files")
        ):
            paths[str(relative)] = path
    require(len(paths) <= MAX_SOURCE_FILES, "caddy_vm_source_count")
    paths.update(
        {
            "build/caddy_probe_vm.py": Path(__file__).resolve(),
            "build/caddy_probe_guest.py": selected.project_root / "build/caddy_probe_guest.py",
            "build/caddy_materials.py": selected.project_root / "build/caddy_materials.py",
            "Caddyfile": selected.project_root / "Caddyfile",
        }
    )
    result: dict[str, fetch.Held] = {}
    for name, path in paths.items():
        original = fetch.Held.open(path, MAX_SOURCE, control)
        owners.inputs.append(original)
        result[name] = original
    require(result["Caddyfile"].sha256 == CADDYFILE_SHA256, "caddy_vm_caddyfile")
    return result


def verify_transfer(
    host: Host, work: Path, port: int, remote: str, originals: Mapping[str, fetch.Held]
) -> None:
    """Compare fixed remote source names before executing the source-bound guest script."""
    _, raw = host.run(
        "guest-source-pins",
        ssh(
            host,
            work,
            port,
            ["sudo", "-n", "sha256sum", "--", *(remote + "/" + name for name in TRANSFER_NAMES)],
        ),
    )
    expected = b"".join(
        (originals[TRANSFER_SOURCES[name]].sha256 + "  " + remote + "/" + name + "\n").encode()
        for name in TRANSFER_NAMES
    )
    require(raw == expected, "caddy_vm_guest_source_changed")


def transfer_archive(destination: Path, held: Mapping[str, fetch.Held], control: Control) -> None:
    """Write exactly four source members with the mature USTAR writer and checked closes."""
    descriptor = -1
    stream: BinaryIO | None = None
    archive: tarfile.TarFile | None = None
    first: BaseException | None = None
    try:
        descriptor = os.open(
            destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600
        )
        stream = os.fdopen(descriptor, "wb")
        descriptor = -1
        archive = tarfile.open(  # noqa: SIM115 -- Preserve the first error across both closes.
            fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT
        )
        for name in TRANSFER_NAMES:
            original = held[TRANSFER_SOURCES[name]]
            original.check(control)
            _ = original.stream.seek(0)
            member = tarfile.TarInfo(name)
            member.size, member.mode, member.uid, member.gid = original.size, 0o600, 0, 0
            archive.addfile(member, original.stream)
            original.check(control)
        closing = archive
        archive = None
        closing.close()
        stream.flush()
        os.fsync(stream.fileno())
        control.check()
    except BaseException as error:  # noqa: BLE001 -- Attempt every original close.
        first = error
    finally:
        actions: list[Callable[[], None]] = []
        if archive is not None:
            actions.append(archive.close)
        if stream is not None:
            actions.append(stream.close)
        if descriptor >= 0:
            actions.append(lambda: os.close(descriptor))
        first = fetch.settle(actions, first)
    if first is not None:
        raise first


def stage_guest(
    host: Host, work: Path, port: int, run_id: str, originals: Mapping[str, fetch.Held]
) -> str:
    """Install prerequisites before the guest guard; transfer no inventory, keys or credentials."""
    _ = host.run(
        "guest-install",
        ssh(
            host,
            work,
            port,
            [
                "/usr/bin/sudo",
                "-n",
                "/usr/bin/env",
                "DEBIAN_FRONTEND=noninteractive",
                "/bin/sh",
                "-ec",
                "apt-get update && apt-get install -y --no-install-recommends "
                + "docker.io docker-cli nftables tcpdump iproute2 tini python3-cryptography "
                + "openssl ca-certificates",
            ],
        ),
        timeout=420,
    )
    remote = "/opt/simplestchat-caddy-prototype/" + run_id
    _ = host.run(
        "guest-directory",
        ssh(
            host,
            work,
            port,
            ["sudo", "-n", "install", "-d", "-o", "root", "-g", "root", "-m", "0700", remote],
        ),
    )
    archive = work / "sources.tar"
    transfer_archive(archive, originals, cast("Control", host.control))
    source = fetch.Held.open(archive, 8 * MAX_SOURCE, host.control)
    first: BaseException | None = None
    try:
        _ = host.run(
            "guest-transfer",
            ssh(
                host,
                work,
                port,
                [
                    "sudo",
                    "-n",
                    "tar",
                    "--extract",
                    "--file=-",
                    "--no-same-permissions",
                    "--directory",
                    remote,
                ],
            ),
            input_file=source.stream,
        )
        source.check(host.control)
    except BaseException as error:  # noqa: BLE001 -- Close the original after every refusal.
        first = error
    finally:
        first = fetch.settle((source.close,), first)
    if first is not None:
        raise first
    verify_transfer(host, work, port, remote, originals)
    return remote


def invoke_guest(
    host: Host, work: Path, port: int, selected: Options, run_id: str
) -> dict[str, object]:
    """Pass only remaining body duration and explicit G; the original controller end still wins."""
    host.control.check()
    remaining = host.control.body_end - time.monotonic()
    require(remaining > selected.guest_finalization_seconds, "caddy_vm_guest_budget")
    status, raw = host.run(
        "guest-probe",
        ssh(
            host,
            work,
            port,
            [
                "/usr/bin/sudo",
                "-n",
                "/usr/bin/python3",
                "-I",
                "-B",
                "/opt/simplestchat-caddy-prototype/" + run_id + "/caddy_probe_guest.py",
                "--run-id",
                run_id,
                "--seconds",
                str(remaining),
                "--finalization-seconds",
                str(selected.guest_finalization_seconds),
            ],
        ),
        timeout=remaining,
        accepted=OBSERVED_STATUSES,
    )
    host.diagnostic["sshStatus"] = status
    refusal: BaseException | None = None
    if status != 0:
        message = "caddy_vm_guest_status"
        refusal = ToolError(message)
        observe_failure(host.diagnostic, refusal, "guest")
    # Metadata is best effort after the actual status; it cannot replace a known refusal.
    outcome = fetch.settle(
        (lambda: host.diagnostic.update(guest_failure_record(raw, GUEST_IMAGE)),), refusal
    )
    if outcome is not None:
        raise outcome
    return guest_record(raw, GUEST_IMAGE)


def hold_tools(host: Host, owners: fetch.Owners) -> None:
    """Retain each actual top-level VM program; no wrapper or tool substitution is installed."""
    for name in TOOL_NAMES:
        original = fetch.Held.open(
            Path(executable(name)).resolve(), MAX_TOOL, host.control, system=True
        )
        owners.inputs.append(original)
        host.fixed_tools[name] = original


def finish_run(
    owners: fetch.Owners,
    control: Control,
    report: dict[str, object],
    first: BaseException | None,
    state: dict[str, object] | None = None,
) -> None:
    """Fence the live body before widening to the original finalization window."""
    state = {} if state is None else state
    before = first
    first = fetch.settle((control.check,), first)
    if first is not None:
        observe_failure(
            state, first, "body-completion" if before is None else str(state.get("checkpoint"))
        )
    if first is not None and report["failure"] is None:
        report["failure"] = "body-completion"
    control.settling = True
    cleanup = owners.finish(control, report)
    report["cleanupPassed"] = cleanup is None
    state["cleanupPassed"] = cleanup is None
    state["cleanupException"] = exception_kind(cleanup)
    if cleanup is not None:
        observe_failure(state, cleanup, "cleanup")
    if report["failure"] is None and cleanup is not None:
        report["failure"] = "cleanup"
    report["passed"] = first is None and cleanup is None and not control.status


def set_checkpoint(report: dict[str, object], state: dict[str, object], value: str) -> None:
    """Keep the existing controller phase and its closed negative metadata in step."""
    report["failure"] = value
    state["checkpoint"] = value


def run(selected: Options, control: Control) -> dict[str, object]:
    """Own the full disposable run and every known finalizer before forming a positive result."""
    owners = fetch.Owners()
    state = selected.diagnostic
    state["checkpoint"] = "admission"
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "CaddyExactSourceDisposableKvm",
        "passed": False,
        "cleanupPassed": False,
        "failure": "admission",
        "sourceClosure": SOURCE_CLOSURE,
        "qualification": QUALIFICATION,
        "guest": None,
        "vmLifecycle": None,
    }
    first: BaseException | None = None
    try:
        require(
            selected.output.is_absolute()
            and selected.output.resolve() == selected.output
            and not selected.output.exists(),
            "caddy_vm_output",
        )
        originals = retain_sources(selected, control, owners)
        vm.preflight(selected.output)
        control.check()
        selected.output.mkdir(mode=0o700)
        selected.output_stamp = fetch.directory_stamp(selected.output.lstat())
        host = Host(selected.output, control)
        host.diagnostic = state
        hold_tools(host, owners)
        report.update(
            {
                "sourceSha256": {name: original.sha256 for name, original in originals.items()},
                "toolSha256": {
                    name: original.sha256 for name, original in host.fixed_tools.items()
                },
            }
        )
        owners.work = fetch.Work.create(selected.output)
        work = owners.work.path
        run_id = uuid.uuid4().hex
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", listener.getsockname())[1]
        set_checkpoint(report, state, "boot")
        vm.prepare_disk(host, work, vm.CloudImage.read())
        vm.create_seed(host, work, port, run_id)
        command = vm.qemu_command(work, port, run_id)
        for index, name in ((0, "timeout"), (4, "prlimit"), (12, "qemu-system-x86_64")):
            require(Path(command[index]).name == name, "caddy_vm_qemu_command")
            host.fixed_tools[name].check(control)
            command[index] = str(host.fixed_tools[name].path)
        control.check()
        owners.guest = vm.Guest(host, command, qmp_path=work / "qmp.sock")
        host.guest = owners.guest
        vm.boot(host, owners.guest, work, port)
        set_checkpoint(report, state, "stage")
        _ = stage_guest(host, work, port, run_id, originals)
        for original in originals.values():
            original.check(control)
        set_checkpoint(report, state, "guest")
        report["guest"] = invoke_guest(host, work, port, selected, run_id)
        state["checkpoint"] = "inputs"
        for original in owners.inputs:
            original.check(control)
        owners.guest.require_alive()
        control.check()
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- Settle all VM and original-input owners first.
        first = error
        observe_failure(state, error, str(state["checkpoint"]))
    finally:
        finish_run(owners, control, report, first, state)
    return report


def restore_outer(
    handlers: dict[int, int | Callable[[int, FrameType | None], object] | None], mask: int
) -> BaseException | None:
    """Attempt every known handler and the original mask before publishing any outcome."""
    actions: list[Callable[[], None]] = []
    for number, handler in handlers.items():

        def restore(
            number: int = number,
            handler: int | Callable[[int, FrameType | None], object] | None = handler,
        ) -> None:
            _ = signal.signal(number, handler)

        actions.append(restore)

    def restore_mask() -> None:
        _ = os.umask(mask)

    actions.append(restore_mask)
    return fetch.settle(actions)


def main(arguments: Sequence[str] | None = None) -> int:
    """Restore every handler/mask before publishing the sanitized final verdict."""
    started = time.monotonic()
    selected = options(arguments)
    control = Control(
        started=started,
        whole_seconds=selected.timeout_seconds,
        finalization_seconds=selected.finalization_seconds,
    )
    previous_mask = os.umask(0o077)
    handlers: dict[int, int | Callable[[int, FrameType | None], object] | None] = {}
    report: dict[str, object] | None = None
    first: BaseException | None = None
    state = selected.diagnostic
    state["checkpoint"] = "admission"
    restored = False
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        report = run(selected, control)
    except BaseException as error:  # noqa: BLE001 -- Never expose raw exceptions or materials.
        first = error
        observe_failure(state, error, str(state.get("checkpoint", "admission")))
    finally:
        restoration = restore_outer(handlers, previous_mask)
        restored = restoration is None
        if restoration is not None:
            observe_failure(state, restoration, "restore")
        first = first or restoration
    checkpoint = "final-fence"
    try:
        control.check()
        require(not control.status, "caddy_vm_cancelled")
        state["finalFencePassed"] = False
        require(report is not None and first is None, "caddy_vm_outer_cleanup")
        assert_report = cast("dict[str, object]", report)
        require(assert_report["passed"] is True, "caddy_vm_negative")
        checkpoint = "summary-output"
        raw = json.dumps(assert_report, separators=(",", ":")).encode() + b"\n"
        require(len(raw) <= MAX_RECORD, "caddy_vm_summary_bound")
        if selected.output_stamp is not None:
            require(
                fetch.directory_stamp(selected.output.lstat()) == selected.output_stamp,
                "caddy_vm_output_changed",
            )
            write_private(selected.output / "summary.json", raw, mode=0o600)
        require(sys.stdout.buffer.write(raw) == len(raw), "caddy_vm_stdout_short")
        _ = sys.stdout.buffer.flush()
        checkpoint = "final-fence"
        control.check()
        require(not control.status, "caddy_vm_cancelled")
        state["finalFencePassed"] = True
    except BaseException as error:  # noqa: BLE001 -- Retain fixed failure metadata after all peers.
        first = first or error
        observe_failure(state, error, checkpoint)
        state["signalStatus"] = control.status
        metadata = failure_summary(state, report, first, restored=restored)
        _ = retain_failure(
            (selected.output, selected.output_stamp, control.end),
            metadata,
            first_error(state, first),
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
