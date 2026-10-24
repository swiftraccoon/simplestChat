"""Prove the actual native release-fetch client in one disposable free KVM guest.

This deliberate VM keeper accepts only local source-built inputs. It creates
the sole generated loopback guest, installs the unchanged receiver, runs cached
then fresh native clients, probes real publication and retires every owned peer.
No application, service, provider or production host operation is available.
Only summary.json is suitable for upload; every raw stream remains private.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pwd
import re
import shlex
import shutil
import signal
import socket
import stat
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, cast, override

import security_vm as vm
import security_vm_fetch_guest as probe
from security_context import ROOT, executable
from security_tools import ToolError, bounded_file, require, write_private
from security_vm_fetch_hosts import KnownHost

# isort: split
import bounded_process
from release_json import decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from types import FrameType
    from typing import BinaryIO

REPOSITORY = "swiftraccoon/simplestChat"
WORKFLOW = "Native release fetch KVM runtime"
SECONDS = 1800
VM_FINALIZATION = 120
SUPERVISOR_TAIL = 20
NATIVE_MAX = 1080
NATIVE_FINALIZATION = 60
# Fixture-only admission: M60 + SSH30 + one existing10s local settlement
# and20s positive work. This does not change the product's accepted N/M domain.
NATIVE_MINIMUM = NATIVE_FINALIZATION + 30 + 10 + 20
MAX_BINARY = 320 * 1024**2
MAX_RECEIPT = 131072
TRIALS = ("cached-local-first", "local-download-repeat")
TOOL_ROLES = ("gh", "curl", "ssh")
DIRECTORY_MODE = 0o700
SOURCE_INPUTS = (
    "build/security_vm_fetch.py",
    "build/security_vm_fetch_guest.py",
    "build/security_vm_fetch_hosts.py",
    "security/vm/fetch.yml",
    "security/vm/debian-cloud.json",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "Cargo.lock",
    "rust-toolchain.toml",
)


def settle(
    actions: Sequence[Callable[[], None]], first: BaseException | None = None
) -> BaseException | None:
    """Attempt every admitted finalizer, preserving the original failure."""
    for action in actions:
        try:
            action()
        except BaseException as error:  # noqa: BLE001 -- Always settle every peer.
            first = first or error
    return first


@dataclass
class Control:
    """Keep the original 1800-second clock and first signal through cleanup."""

    started: float = field(default_factory=time.monotonic)
    status: int = 0
    settling: bool = False

    @property
    def end(self) -> float:
        """Keep all body/finalization ceilings derived from the entry clock."""
        return self.started + SECONDS

    @property
    def body_end(self) -> float:
        """Reserve VM/input/known-host retirement before admitting another command."""
        return self.end - VM_FINALIZATION

    def signal(self, number: int, _frame: object) -> None:
        """Latch once without throwing into acquired resources."""
        self.status = self.status or (130 if number == signal.SIGINT else 143)

    def check(self) -> None:
        """Refuse late/cancelled effects while still permitting bounded finalizer attempts."""
        now = time.monotonic()
        ceiling = self.end if self.settling else self.body_end
        require(math.isfinite(now) and self.started <= now < ceiling, "fetch_vm_deadline")
        require(not self.status or self.settling, "fetch_vm_cancelled")


def ci_home(environment: Mapping[str, str]) -> Path:
    """Confine the real passwd-home mutation to the dedicated disposable Linux CI leaf."""
    require(
        sys.platform == "linux"
        and environment.get("GITHUB_ACTIONS") == "true"
        and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and environment.get("GITHUB_REPOSITORY") == REPOSITORY
        and environment.get("GITHUB_REF") == "refs/heads/main"
        and environment.get("GITHUB_WORKFLOW") == WORKFLOW,
        "fetch_vm_disposable_ci",
    )
    return Path(pwd.getpwuid(os.getuid()).pw_dir)


@dataclass
class Held:
    """Retain one original local source inode through native and VM settlement."""

    path: Path
    stream: BinaryIO
    stamp: tuple[int, ...]
    sha256: str
    size: int

    @classmethod
    def open(cls, path: Path, maximum: int, control: Control, *, system: bool = False) -> Held:
        """Admit a canonical owned regular file before reading or hashing it."""
        control.check()
        require(path.is_absolute() and path.resolve() == path, "fetch_vm_input_path")
        control.check()
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
        stream = os.fdopen(descriptor, "rb")
        try:
            metadata = os.fstat(stream.fileno())
            require(
                stat.S_ISREG(metadata.st_mode)
                and metadata.st_uid in ((os.getuid(), 0) if system else (os.getuid(),))
                and not metadata.st_mode & 0o022
                and 0 < metadata.st_size <= maximum,
                "fetch_vm_input_file",
            )
            held = cls(path, stream, probe.stamp(metadata), "", metadata.st_size)
            held.sha256 = held.digest(control)
            held.check(control)
        except BaseException:
            stream.close()
            raise
        return held

    def digest(self, control: Control) -> str:
        """Hash bounded chunks of the same original without allocating its whole payload."""
        control.check()
        _ = self.stream.seek(0)
        control.check()
        digest, count = hashlib.sha256(), 0
        while True:
            control.check()
            chunk = self.stream.read(min(65536, self.size + 1 - count))
            control.check()
            if not chunk:
                break
            count += len(chunk)
            require(count <= self.size, "fetch_vm_input_changed")
            digest.update(chunk)
        require(count == self.size, "fetch_vm_input_changed")
        _ = self.stream.seek(0)
        control.check()
        return digest.hexdigest()

    def check(self, control: Control) -> None:
        """Check original full metadata, namespace and exact bytes after every native trial."""
        control.check()
        require(
            probe.stamp(os.fstat(self.stream.fileno())) == self.stamp
            and probe.stamp(self.path.lstat()) == self.stamp
            and self.digest(control) == self.sha256,
            "fetch_vm_input_changed",
        )
        require(probe.stamp(os.fstat(self.stream.fileno())) == self.stamp, "fetch_vm_input_changed")
        control.check()

    def close(self) -> None:
        """Explicitly close this original even if a later peer fails."""
        self.stream.close()


def directory_stamp(value: os.stat_result) -> tuple[int, ...]:
    """Allow owned directory contents to grow while binding its identity and permissions."""
    return value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid


@dataclass
class Work:
    """Retain the original work and parent directories until checked recursive retirement.

    Descriptor-relative rmtree supplies the mature traversal. The preceding
    identity checks refuse observed substitution; they cannot remove the
    same-UID check/read race between validation and a pathname-based library.
    """

    path: Path
    parent_fd: int | None
    file_fd: int | None = None
    parent_stamp: tuple[int, ...] | None = None
    stamp: tuple[int, ...] | None = None

    @classmethod
    def create(cls, parent: Path) -> Work:
        """Admit the original parent before making one fixed private work directory."""
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
        parent_fd = os.open(parent, flags)
        owned = cls(parent, parent_fd)
        try:
            first = os.fstat(parent_fd)
            require(
                stat.S_ISDIR(first.st_mode)
                and first.st_uid == os.getuid()
                and stat.S_IMODE(first.st_mode) == DIRECTORY_MODE
                and directory_stamp(parent.lstat()) == directory_stamp(first),
                "fetch_vm_work_parent",
            )
            owned.parent_stamp = directory_stamp(first)
            owned.path = Path(tempfile.mkdtemp(prefix="fetch-vm-private-", dir=parent))
            owned.file_fd = os.open(owned.path.name, flags, dir_fd=parent_fd)
            child = os.fstat(owned.file_fd)
            require(
                stat.S_ISDIR(child.st_mode)
                and child.st_uid == os.getuid()
                and stat.S_IMODE(child.st_mode) == DIRECTORY_MODE,
                "fetch_vm_work_directory",
            )
            owned.stamp = directory_stamp(child)
            owned.check()
        except BaseException as first_error:
            # A creation that never established the original child identity is
            # retained on refusal. It cannot authorize deleting a later entry.
            try:
                owned.finish()
            except BaseException as cleanup_error:
                raise first_error from cleanup_error
            raise
        return owned

    def check(self) -> None:
        """Compare anchored current entries with both still-open original directories."""
        parent, child = self.parent_fd, self.file_fd
        require(
            parent is not None
            and child is not None
            and self.stamp is not None
            and self.parent_stamp is not None,
            "fetch_vm_work_owner",
        )
        if parent is None or child is None:
            message = "fetch_vm_work_owner"
            raise ToolError(message)
        require(
            directory_stamp(os.fstat(parent)) == self.parent_stamp
            and directory_stamp(self.path.parent.lstat()) == self.parent_stamp
            and directory_stamp(os.fstat(child)) == self.stamp
            and directory_stamp(os.stat(self.path.name, dir_fd=parent, follow_symlinks=False))
            == self.stamp,
            "fetch_vm_work_changed",
        )

    def finish(self) -> None:
        """Refuse foreign removal while still attempting every original descriptor close."""
        first: BaseException | None = None
        parent, child = self.parent_fd, self.file_fd
        try:
            self.check()
            require(parent is not None, "fetch_vm_work_owner")
            shutil.rmtree(self.path.name, dir_fd=parent)
        except BaseException as error:  # noqa: BLE001 -- Preserve refusal through both closes.
            first = error
        finally:
            self.parent_fd, self.file_fd = None, None
            actions = tuple(lambda fd=fd: os.close(fd) for fd in (child, parent) if fd is not None)
            first = settle(actions, first)
        if first is not None:
            raise first


@dataclass
class NativeTools:
    """Expose only three checked symlinks to the retained canonical native tool originals.

    These are filesystem aliases, not wrappers or observer scripts. Their
    validation detects observed mutation; same-UID pathname races remain a
    limitation, as in the native client's own executable-alias witnesses.
    """

    directory: Path
    stamp: tuple[int, ...]
    work: Work
    links: dict[str, tuple[tuple[int, ...], Held]] = field(default_factory=dict)

    @classmethod
    def create(cls, work: Work, originals: Mapping[str, Held], control: Control) -> NativeTools:
        """Build a closed native PATH without re-resolving an ambient alias during a trial."""
        control.check()
        work.check()
        directory = work.path / "native-bin"
        directory.mkdir(mode=DIRECTORY_MODE)
        metadata = directory.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == os.getuid()
            and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE,
            "fetch_vm_native_tools_changed",
        )
        tools = cls(directory, directory_stamp(metadata), work)
        for name in TOOL_ROLES:
            control.check()
            original = originals[name]
            original.check(control)
            require(
                directory_stamp(directory.lstat()) == tools.stamp, "fetch_vm_native_tools_changed"
            )
            path = directory / name
            path.symlink_to(original.path)
            metadata = path.lstat()
            require(
                stat.S_ISLNK(metadata.st_mode) and metadata.st_uid == os.getuid(),
                "fetch_vm_native_tool_link",
            )
            tools.links[name] = (probe.stamp(metadata), original)
        tools.check(control)
        return tools

    def check(self, control: Control) -> None:
        """Bind link identity/text, canonical target namespace and original bytes at each fence."""
        control.check()
        self.work.check()
        require(
            directory_stamp(self.directory.lstat()) == self.stamp
            and set(self.links) == set(TOOL_ROLES),
            "fetch_vm_native_tools_changed",
        )
        names: set[str] = set()
        with os.scandir(self.directory) as entries:
            for entry in entries:
                control.check()
                require(
                    entry.name in TOOL_ROLES and entry.name not in names,
                    "fetch_vm_native_tools_changed",
                )
                names.add(entry.name)
        require(names == set(TOOL_ROLES), "fetch_vm_native_tools_changed")
        for name, (stamp, original) in self.links.items():
            control.check()
            path = self.directory / name
            require(
                probe.stamp(path.lstat()) == stamp
                and path.readlink() == original.path
                and path.resolve(strict=True) == original.path,
                "fetch_vm_native_tools_changed",
            )
            original.check(control)


class Host(vm.Host):
    """Reuse mature VM ownership with distinct credential-free and native-client environments."""

    def __init__(self, output: Path, control: Control) -> None:
        """VM commands cannot spend the independently reserved finalization window."""
        self.control: Control = control
        self.guest: vm.Guest | None = None
        self.fixed_tools: dict[str, Held] = {}
        super().__init__(output, control.body_end)

    def healthy(self) -> bool:
        """Observe original cancellation and the exact QEMU while child streams are quiet."""
        try:
            self.control.check()
            if self.guest is not None:
                self.guest.require_alive()
        except (OSError, ValueError, ToolError):
            return False
        return True

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        timeout: float = 120,
        accepted: tuple[int, ...] = (0,),
    ) -> tuple[int, bytes]:
        """Use the same bounded owner for preparation/probes without inheriting API credentials."""
        return self.command(name, argv, self.env, timeout, accepted)

    def command(
        self,
        name: str,
        argv: Sequence[str],
        environment: Mapping[str, str],
        timeout: float,
        accepted: tuple[int, ...],
    ) -> tuple[int, bytes]:
        """Drain private bounded streams and close every original before classifying status."""
        require(
            re.fullmatch(r"[a-z0-9-]{1,64}", name) and len(self.checks) < vm.MAX_COMMANDS,
            "fetch_vm_command_role",
        )
        self.control.check()
        phase_end = min(time.monotonic() + timeout, self.deadline)
        role = {
            "download-cloud": "curl",
            "ssh-ready": "ssh",
            "cloud-init": "ssh",
            "probe-1": "ssh",
            "probe-2": "ssh",
            "fetch-stage": "ssh",
        }.get(name)
        program = self.fixed_tools.get(role) if role is not None else None
        command, child_environment = list(argv), dict(environment)
        if role is not None:
            require(program is not None, "fetch_vm_tool_owner")
            if program is not None:
                program.check(self.control)
                if name == "fetch-stage":
                    child_environment["ANSIBLE_SSH_EXECUTABLE"] = str(program.path)
                else:
                    require(command, "fetch_vm_command_role")
                    command[0] = str(program.path)
        streams: list[BinaryIO] = []
        first: BaseException | None = None
        status, data = 1, b""
        prefix = self.output / f"{len(self.checks):02d}-{name}"
        try:
            for extension in (".stdout", ".stderr"):
                self.control.check()
                descriptor = os.open(
                    prefix.with_suffix(extension),
                    os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                    0o600,
                )
                stream = os.fdopen(descriptor, "w+b")
                streams.append(stream)
                os.fchmod(stream.fileno(), 0o600)
            self.control.check()
            remaining = phase_end - time.monotonic()
            require(remaining > 0, "fetch_vm_deadline")
            status, _, _ = bounded_process.run(
                command,
                cwd=ROOT,
                env=child_environment,
                output=streams[0],
                error=streams[1],
                limits=bounded_process.Limits(
                    timeout=remaining,
                    stdout=vm.MAX_STREAM,
                    stderr=vm.MAX_STREAM,
                    healthy=self.healthy,
                ),
            )
            for stream in streams:
                stream.flush()
            _ = streams[0].seek(0)
            data = streams[0].read(vm.MAX_STREAM + 1)
            require(len(data) <= vm.MAX_STREAM, "fetch_vm_capture_bound")
            self.control.check()
            if program is not None:
                program.check(self.control)
        except BaseException as error:  # noqa: BLE001 -- Close both originals after cancellation.
            first = error
        finally:
            first = settle(tuple(stream.close for stream in streams), first)
        self.checks.append({"name": name, "exitStatus": status})
        if first is not None:
            raise first
        require(status in accepted, "fetch_vm_command_failed")
        return status, data


def native_budget(control: Control) -> int:
    """Clip the existing caller1080 cap to the SAME VM clock, never restarting a trial."""
    control.check()
    seconds = min(NATIVE_MAX, math.floor(control.body_end - time.monotonic() - SUPERVISOR_TAIL))
    require(seconds >= NATIVE_MINIMUM, "fetch_vm_no_trial_room")
    return seconds


def selection(path: Path, revision: str) -> dict[str, object]:
    """Require only the same-head public selectors from existing preacquisition."""
    value = object_value(decode_json(bounded_file(path, 16384, private=True)))
    require(
        set(value) == {"revision", "ciRunId", "artifactId"}
        and value["revision"] == revision
        and all(
            type(value[key]) is int and 0 < cast("int", value[key]) <= probe.SAFE
            for key in ("ciRunId", "artifactId")
        ),
        "fetch_vm_selection",
    )
    return dict(value)


def inventory(work: Path, port: int, run_id: str, revision: str, ssh: Path) -> Path:
    """No application/provider variables or caller-selected destinations enter Ansible."""
    value = {
        "benchmark_hosts": {
            "hosts": {
                "vmfixture": {
                    "ansible_host": "127.0.0.1",
                    "ansible_port": port,
                    "ansible_user": "fixture",
                    "ansible_python_interpreter": "/usr/bin/python3",
                    "ansible_become": True,
                    "ansible_ssh_private_key_file": str(work / "client"),
                    "ansible_ssh_executable": str(ssh),
                    "ansible_ssh_args": shlex.join(vm.ssh_options(work)),
                    "vm_fixture_run_id": run_id,
                    "vm_fixture_revision": revision,
                }
            }
        }
    }
    path = work / "inventory.json"
    write_private(path, json.dumps(value).encode(), mode=0o600)
    return path


def native_arguments(  # noqa: PLR0913, PLR0917 -- Fixed generated VM roles, no arbitrary target.
    binary: Path,
    work: Path,
    port: int,
    selected: Mapping[str, object],
    acquired: Path,
    output: Path,
    seconds: int,
    *,
    cached: bool,
) -> list[str]:
    """Keep the real product's fixed SSH policy and only the generated loopback target."""
    values = {
        "repository": REPOSITORY,
        "revision": str(selected["revision"]),
        "artifact-id": str(selected["artifactId"]),
        "ci-run": str(selected["ciRunId"]),
        "host": "127.0.0.1",
        "port": str(port),
        "user": "fixture",
        "identity": str(work / "client"),
        "output-parent": str(output),
        "timeout-seconds": str(seconds),
        "finalization-seconds": str(NATIVE_FINALIZATION),
    }
    if cached:
        values["verified-directory"] = str(acquired / "artifact")
    return [
        str(binary),
        "release",
        "fetch",
        *[item for name, value in values.items() for item in ("--" + name, value)],
    ]


def native_receipt(
    data: bytes, selected: Mapping[str, object], pins: Mapping[str, Held]
) -> dict[str, object]:
    """Parse passive facts, separately requiring current zero native status at the callsite."""
    require(
        0 < len(data) <= MAX_RECEIPT and data.endswith(b"\n") and data.count(b"\n") == 1,
        "fetch_vm_native_record",
    )
    value = object_value(decode_json(data))
    remote = object_value(value.get("remote"))
    require(
        value.get("schemaVersion") == 1
        and type(value.get("schemaVersion")) is int
        and value.get("operation") == "fetch_release_controller"
        and value.get("passed") is True
        and value.get("phase") == "complete"
        and value.get("remoteSettled") is True
        and value.get("failureClass") is None
        and remote.get("passed") is True
        and remote.get("settled") is True
        and remote.get("finalized") is True
        and remote.get("status") == "complete"
        and remote.get("phase") == "complete"
        and remote.get("failureClass") is None
        and remote.get("revision") == selected["revision"]
        and type(remote.get("artifactId")) is int
        and remote["artifactId"] == selected["artifactId"]
        and remote.get("zipSha256") == pins["artifact.zip"].sha256
        and remote.get("artifactZipBytes") == pins["artifact.zip"].size
        and type(remote.get("downloadedBytes")) is int
        and remote["downloadedBytes"] == pins["artifact.zip"].size
        and remote.get("archiveSha256") == pins["image.tar"].sha256
        and remote.get("manifestSha256") == pins["release.json"].sha256,
        "fetch_vm_native_unproven",
    )
    return {
        "passed": True,
        "nativeExitStatus": 0,
        "receiptComplete": True,
        "remoteSettled": True,
        "transportProof": "native-contract-current-ssh-zero-and-natural-eof",
    }


def observe_guest(  # noqa: PLR0913, PLR0917 -- Bind independent original public pins.
    host: Host,
    work: Path,
    port: int,
    run_id: str,
    selected: Mapping[str, object],
    pins: Mapping[str, Held],
    count: int,
) -> dict[str, object]:
    """Run the fixed read-only assertion in the already-authenticated owned guest."""
    values = {
        "run-id": run_id,
        "revision": str(selected["revision"]),
        "artifact-id": str(selected["artifactId"]),
        "zip-sha256": pins["artifact.zip"].sha256,
        "zip-bytes": str(pins["artifact.zip"].size),
        "image-sha256": pins["image.tar"].sha256,
        "manifest-sha256": pins["release.json"].sha256,
        "attempts": str(count),
    }
    command = [
        "sudo",
        "-n",
        "/usr/bin/python3",
        "-I",
        "-B",
        "/root/simplestchat-vm-fixture/security_vm_fetch_guest.py",
        *[item for name, value in values.items() for item in ("--" + name, value)],
    ]
    ssh = host.fixed_tools.get("ssh")
    require(ssh is not None and vm.MIN_PORT <= port <= vm.MAX_PORT, "fetch_vm_tool_owner")
    if ssh is None:
        message = "fetch_vm_tool_owner"
        raise ToolError(message)
    # Reuse mature fixture flags and its fixed target without another ambient
    # executable lookup. Product release-fetch argv remains unchanged.
    arguments = [
        str(ssh.path),
        *vm.ssh_options(work),
        "-i",
        str(work / "client"),
        "-p",
        str(port),
        "fixture@127.0.0.1",
        shlex.join(command),
    ]
    _, data = host.run("probe-" + str(count), arguments, timeout=120)
    value = probe.object_json(data)
    require(
        set(value)
        == {"schemaVersion", "scope", "runId", "revision", "attempts", "published", "passed"}
        and type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["scope"] == "ReleaseFetchGuestProbe"
        and value["runId"] == run_id
        and value["revision"] == selected["revision"]
        and type(value["attempts"]) is int
        and value["attempts"] == count
        and value["passed"] is True,
        "fetch_vm_probe_record",
    )
    published_raw = value["published"]
    require(isinstance(published_raw, dict), "fetch_vm_probe_record")
    published = cast("dict[str, object]", published_raw)
    require(
        set(published) == {"image.tar", "release.json"},
        "fetch_vm_probe_record",
    )
    for name, raw in published.items():
        require(isinstance(raw, dict), "fetch_vm_probe_record")
        row = cast("dict[str, object]", raw)
        require(
            set(row) == {"sha256", "bytes", "device", "inode"}
            and row["sha256"] == pins[name].sha256
            and type(row["bytes"]) is int
            and row["bytes"] == pins[name].size
            and all(
                type(row[key]) is int and cast("int", row[key]) >= 0 for key in ("device", "inode")
            ),
            "fetch_vm_probe_record",
        )
    return value


@dataclass
class Owners:
    """Keep actual VM, real SSH-home entry, originals and private work until all settle."""

    guest: vm.Guest | None = None
    known: KnownHost | None = None
    inputs: list[Held] = field(default_factory=list)
    work: Work | None = None

    def finish(self, control: Control, report: dict[str, object]) -> BaseException | None:
        """No failed VM close may prevent known-host retirement or input closes."""
        actions: list[Callable[[], None]] = []
        if self.guest is not None:
            guest = self.guest

            def close_guest() -> None:
                returned = False
                try:
                    guest.close()
                    returned = True
                finally:
                    raw = guest.diagnostics()
                    qmp = cast("dict[str, object]", raw.get("qmp", {}))
                    cloud = cast("dict[str, object]", raw.get("cloudInit", {}))
                    facts = {
                        "qemuReaped": type(raw.get("cleanupReturnCode")) is int,
                        "qemuExitedZero": type(raw.get("cleanupReturnCode")) is int
                        and raw["cleanupReturnCode"] == 0,
                        "authenticatedSsh": raw.get("authenticatedSsh") is True,
                        "drainerCompleted": raw.get("drainer") == "completed",
                        "qmpConnected": qmp.get("connected") is True,
                        "qmpGreeting": qmp.get("greetingReceived") is True,
                        "qmpCapabilities": qmp.get("capabilitiesNegotiated") is True,
                        "qmpEof": qmp.get("eofObserved") is True,
                        "qmpPassed": bool(qmp) and qmp.get("failure") is None,
                        "cloudInitPassed": cloud.get("passed") is True,
                        "closeReturned": returned,
                    }
                    report["vmLifecycle"] = {
                        **facts,
                        "qemuReturnCode": raw["cleanupReturnCode"]
                        if type(raw.get("cleanupReturnCode")) is int
                        else None,
                    }
                    require(all(facts.values()) and guest.failure is None, "fetch_vm_cleanup")

            actions.append(close_guest)
        if self.known is not None:
            known = self.known
            actions.append(known.finish)
        for held in self.inputs:
            actions.extend((lambda held=held: held.check(control), held.close))
        if self.work is not None:
            actions.append(self.work.finish)
        actions.append(control.check)
        return settle(actions)


def run(  # noqa: C901, PLR0915 -- Ordered whole VM ownership.
    binary: Path, acquired: Path, output: Path, control: Control
) -> dict[str, object]:
    """Own the complete actual proof, keeping every public verdict negative until cleanup."""
    owners = Owners()
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SourceBuiltNativeReleaseFetchKvm",
        "sourceTarget": "x86_64-unknown-linux-gnu",
        "authenticatedOpsDistribution": False,
        "passed": False,
        "cleanupPassed": False,
        "failure": "guard",
        "cleanupUncertain": False,
        "guestExecuted": False,
        "remoteOutcome": "not-started",
        "trials": [],
        "samePublishedInodes": False,
        "vmLifecycle": None,
        "observer": "no-tool-wrappers; native-contract-plus-readonly-guest-probe",
    }
    first: BaseException | None = None
    try:
        home = ci_home(os.environ)
        control.check()
        require(
            output.is_absolute() and output.resolve() == output and not output.exists(),
            "fetch_vm_output",
        )
        require(os.environ.get("GH_TOKEN"), "fetch_vm_token_missing")
        revision = os.environ.get("GITHUB_SHA", "")
        require(re.fullmatch(r"[a-f0-9]{40}", revision), "fetch_vm_source")
        vm.preflight(output)
        pins: dict[str, Held] = {}
        for name, path, maximum in (
            ("binary", binary, MAX_BINARY),
            ("selection", acquired / "selection.json", 16384),
            ("image.tar", acquired / "artifact/image.tar", probe.MAX_IMAGE),
            ("release.json", acquired / "artifact/release.json", probe.MAX_MANIFEST),
            ("artifact.zip", acquired / "artifact/artifact.zip", probe.MAX_IMAGE),
        ):
            held = Held.open(path, maximum, control)
            owners.inputs.append(held)
            pins[name] = held
        selected = selection(acquired / "selection.json", revision)
        pins["selection"].check(control)
        require(os.access(binary, os.X_OK), "fetch_vm_binary_executable")
        manifest = object_value(
            decode_json(bounded_file(pins["release.json"].path, probe.MAX_MANIFEST))
        )
        require(
            manifest.get("revision") == revision
            and manifest.get("archiveSha256") == pins["image.tar"].sha256,
            "fetch_vm_source",
        )
        for original in owners.inputs:
            original.check(control)
        report.update(
            {
                "revision": revision,
                "artifactId": selected["artifactId"],
                "ciRunId": selected["ciRunId"],
                "binarySha256": pins["binary"].sha256,
                "zipSha256": pins["artifact.zip"].sha256,
                "zipBytes": pins["artifact.zip"].size,
            }
        )
        source_hashes: dict[str, str] = {}
        for name in SOURCE_INPUTS:
            control.check()
            data = bounded_file(ROOT / name, 2**20)
            control.check()
            source_hashes[name] = hashlib.sha256(data).hexdigest()
        report["sourceSha256"] = source_hashes
        control.check()
        pin = vm.CloudImage.read()
        control.check()
        report["cloudImageSha512"] = pin.sha512
        output.mkdir(mode=0o700)
        host = Host(output, control)
        report["toolSha256"] = {}
        original_tools: dict[str, Held] = {}
        for name in TOOL_ROLES:
            tool = Held.open(Path(executable(name)).resolve(), MAX_BINARY, control, system=True)
            owners.inputs.append(tool)
            original_tools[name] = tool
            cast("dict[str, object]", report["toolSha256"])[name] = tool.sha256
        host.fixed_tools = original_tools
        host.env["ANSIBLE_SSH_EXECUTABLE"] = str(original_tools["ssh"].path)
        owners.work = Work.create(output)
        control.check()
        work = owners.work.path
        native_tools = NativeTools.create(owners.work, original_tools, control)
        run_id = uuid.uuid4().hex
        report["runId"] = run_id
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", listener.getsockname())[1]
        report["failure"] = "boot"
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, run_id)
        qemu_arguments = vm.qemu_command(work, port, run_id)
        control.check()
        guest = owners.guest = vm.Guest(host, qemu_arguments, qmp_path=work / "qmp.sock")
        control.check()
        host.guest = guest
        report["guestExecuted"] = True
        vm.boot(host, guest, work, port)
        report["failure"] = "stage"
        path = inventory(work, port, run_id, revision, original_tools["ssh"].path)
        _, stage = host.run(
            "fetch-stage",
            [executable("ansible-playbook"), "-i", str(path), str(ROOT / "security/vm/fetch.yml")],
            timeout=180,
        )
        _ = vm.recap(stage, unchanged=False)
        guest.require_alive()
        report["failure"] = "known-hosts"
        control.check()
        owners.known = KnownHost(home, port, bounded_file(work / "host.pub", 4096).decode().strip())
        control.check()
        native_environment = dict(host.env)
        native_environment["PATH"] = str(native_tools.directory)
        native_environment["GH_TOKEN"] = os.environ["GH_TOKEN"]
        previous: dict[str, object] | None = None
        for count, label in enumerate(TRIALS, 1):
            report["failure"] = label
            owners.known.check()
            native_tools.check(control)
            for source in owners.inputs:
                source.check(control)
            guest.require_alive()
            seconds = native_budget(control)
            started = time.monotonic()
            report["remoteOutcome"] = "unknown"
            _, data = host.command(
                label,
                native_arguments(
                    binary,
                    work,
                    port,
                    selected,
                    acquired,
                    output / label,
                    seconds,
                    cached=count == 1,
                ),
                native_environment,
                seconds + 5,
                (0,),
            )
            elapsed = time.monotonic() - started
            receipt = native_receipt(data, selected, pins)
            owners.known.check()
            native_tools.check(control)
            for source in owners.inputs:
                source.check(control)
            guest.require_alive()
            observed = observe_guest(host, work, port, run_id, selected, pins, count)
            current = cast("dict[str, object]", observed["published"])
            if previous is not None:
                require(current == previous, "fetch_vm_publication_changed")
                report["samePublishedInodes"] = True
            previous = current
            report["remoteOutcome"] = "validated"
            cast("list[object]", report["trials"]).append(
                {
                    "name": label,
                    "timeoutSeconds": seconds,
                    "finalizationSeconds": NATIVE_FINALIZATION,
                    "elapsedSeconds": round(elapsed, 3),
                    "attempts": count,
                    "publication": current,
                    **receipt,
                }
            )
        control.check()
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- Preserve first refusal.
        first = error
    finally:
        control.settling = True
        cleanup = owners.finish(control, report)
        first = first or cleanup
        report["cleanupPassed"] = cleanup is None
        report["cleanupUncertain"] = cleanup is not None
        if cleanup is not None and report["failure"] is None:
            report["failure"] = "cleanup"
        report["passed"] = (
            first is None and not control.status and report["samePublishedInodes"] is True
        )
        if control.status and report["failure"] is None:
            report["failure"] = "cancelled"
    return report


@dataclass
class Options(argparse.Namespace):
    """Only original local inputs and a new private evidence directory are selectable."""

    binary: Path = Path()
    acquired: Path = Path()
    output: Path = Path()


def main(arguments: Sequence[str] | None = None) -> int:
    """Publish only the sanitized closed summary, after all owners have completed."""
    control = Control()
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    for name in ("binary", "acquired", "output"):
        _ = parser.add_argument("--" + name, type=Path, required=True)
    options = parser.parse_args(arguments, namespace=Options())
    previous_mask = os.umask(0o077)
    handlers: dict[int, int | Callable[[int, FrameType | None], object] | None] = {}
    status = 1
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        report = run(options.binary, options.acquired, options.output, control)
        data = json.dumps(report, separators=(",", ":")).encode() + b"\n"
        require(len(data) <= MAX_RECEIPT, "fetch_vm_summary_bound")
        if options.output.is_dir():
            write_private(options.output / "summary.json", data, mode=0o600)
        _ = sys.stdout.buffer.write(data)
        _ = sys.stdout.buffer.flush()
        control.check()
        status = 0 if report["passed"] is True and not control.status else 1
    except (OSError, ValueError, ToolError, RecursionError):
        _ = sys.stderr.write("Native release fetch KVM proof is unproven.\n")
        status = 1
    finally:

        def restore_mask() -> None:
            _ = os.umask(previous_mask)

        actions: list[Callable[[], None]] = []
        for number, handler in handlers.items():

            def restore(
                number: int = number,
                handler: int | Callable[[int, FrameType | None], object] | None = handler,
            ) -> None:
                _ = signal.signal(number, handler)

            actions.append(restore)
        actions.append(restore_mask)
        if settle(actions) is not None:
            status = 1
    try:
        control.check()
    except (ValueError, ToolError):
        status = 1
    # Settling permits finalizer work after a signal; final success does not.
    return status or int(bool(control.status))


if __name__ == "__main__":
    raise SystemExit(main())
