"""Own a signed-musl host receiver proof inside one disposable free Debian VM.

Current authentication commands are owned zero-status children, never inferred
from a saved observation. Their original payloads stay pinned through install,
real SSH trials, independent guest checks and every VM/input finalizer. Only the
closed summary may be uploaded. No host, image, command or root is selectable.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import pwd
import re
import selectors
import shlex
import signal
import socket
import ssl
import stat
import subprocess
import sys
import tarfile
import time
import tomllib
import uuid
from http import HTTPStatus
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, cast, final, override

import security_vm as vm
import security_vm_fetch as fetch_vm
import security_vm_ops as ops_vm
import security_vm_ops_guest as ops_guest
from security_context import ROOT, executable
from security_tools import ToolError, bounded_file, require, write_private
from security_vm_fetch_hosts import KnownHost

# isort: split
import bounded_process
import release_fetch_controller as reference
from release_json import decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

WORKFLOW = "Signed musl release receive KVM runtime"
MAX_RECORD = 131072
FRAME_BYTES = 16384
SAFE = 9007199254740991
PRIVATE_DIRECTORY = 0o700
SIGNED_OPS_FILES = 15
EVIDENCE_MEMBERS = 16
HOST_TIMEOUT = 300
HOST_FINALIZATION = 30
SOURCE_INPUTS = tuple(
    dict.fromkeys(
        (
            *fetch_vm.SOURCE_INPUTS,
            "build/security_vm_ops.py",
            "build/security_vm_ops_guest.py",
            "build/security_vm_vps_guest.py",
            "build/vps_ops_bootstrap.py",
            "build/security_vm.py",
            "build/security_vm_qmp.py",
            "build/security_context.py",
            "build/security_tools.py",
            "build/python-requirements.txt",
            "security/vm/ops-stage.yml",
            "security/vm/debian-cloud.json",
            "ops/ansible/ansible.cfg",
            "ops/ansible/requirements.txt",
            "ops/ansible/files/bounded_process.py",
            "ops/ansible/files/release_artifact.py",
            "ops/ansible/files/release_json.py",
            "ops/ansible/files/release_preflight.py",
            "ops/ansible/files/release-public.py",
            "ops/ansible/files/release_public.py",
            "ops/ansible/files/release_assets.py",
            "ops/ansible/files/browser_assets.py",
            "ops/ansible/files/runtime_profile.py",
            "ops/ansible/files/reboot-public.py",
            "ops/ansible/files/reboot_public.py",
            "ops/ansible/files/release_trust.py",
            "ops/ansible/files/fetch-release.py",
            "ops/ansible/files/release_fetch_receiver.py",
            "ops/ansible/files/force_release.py",
            "Cargo.toml",
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
        )
    )
)


def encoded(value: object) -> bytes:
    """Write stable, finite private JSON without changing the source values."""
    return json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def private_record(path: Path) -> dict[str, object]:
    """Treat retained JSON as passive data, not authentication authority."""
    return cast(
        "dict[str, object]", object_value(decode_json(bounded_file(path, MAX_RECORD, private=True)))
    )


def admission(environment: Mapping[str, str]) -> Path:
    """Confine real passwd-home changes to this exact disposable CI leaf."""
    require(
        sys.platform == "linux"
        and environment.get("GITHUB_ACTIONS") == "true"
        and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and environment.get("GITHUB_REPOSITORY") == fetch_vm.REPOSITORY
        and environment.get("GITHUB_REF") == "refs/heads/main"
        and environment.get("GITHUB_WORKFLOW") == WORKFLOW,
        "receive_vm_disposable_ci",
    )
    return Path(pwd.getpwuid(os.getuid()).pw_dir)


class Options(argparse.Namespace):
    """Fixed public artifact selectors and one fresh private output."""

    run_id: int = 0
    run_attempt: int = 0
    artifact_id: int = 0
    output: Path = Path()


def fail() -> NoReturn:
    """Make impossible local state negative without external error text."""
    message = "receive_vm_unproven"
    raise ToolError(message)


class Host(fetch_vm.Host):
    """Keep existing VM custody and add only its original regular stdin loan."""

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
        """Never expose API credentials to Ansible, SSH or the guest."""
        if input_file is None:
            return super().run(name, argv, timeout=timeout, accepted=accepted)
        command = list(argv)
        ssh = self.fixed_tools.get("ssh")
        require(ssh is not None, "receive_vm_ssh_original")
        if ssh is None:
            fail()
        ssh.check(self.control)
        command[0] = str(ssh.path)
        self.control.check()
        phase_end = min(time.monotonic() + timeout, self.deadline)
        streams: list[BinaryIO] = []
        primary: BaseException | None = None
        status, data = 1, b""
        try:
            for suffix in ("stdout", "stderr"):
                path = self.output / f"{len(self.checks):02d}-{name}.{suffix}"
                descriptor = os.open(
                    path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
                )
                streams.append(os.fdopen(descriptor, "w+b"))
            remaining = phase_end - time.monotonic()
            require(remaining > 0, "receive_vm_deadline")
            status, _, _ = bounded_process.run(
                command,
                cwd=ROOT,
                env=self.env,
                input_file=input_file,
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
            require(len(data) <= vm.MAX_STREAM, "receive_vm_stream_bound")
            self.control.check()
            ssh.check(self.control)
        except BaseException as error:  # noqa: BLE001 -- Settle both original logs after every refusal.
            primary = error
        finally:
            primary = fetch_vm.settle(tuple(stream.close for stream in streams), primary)
        self.checks.append({"name": name, "exitStatus": status})
        if primary is not None:
            raise primary
        require(status in accepted, "receive_vm_command_failed")
        return status, data


def credential_environment(host: Host, tools: fetch_vm.NativeTools) -> dict[str, str]:
    """Bind every authenticated child to original GH/curl plus explicit system utilities."""
    tools.check(host.control)
    return dict(
        host.env,
        PATH=os.pathsep.join((str(tools.directory), "/usr/bin", "/bin")),
        GH_TOKEN=os.environ["GH_TOKEN"],
    )


@final
class OpsTree:
    """One already-retained flat payload namespace through fresh verification and execution."""

    def __init__(self, path: Path, control: fetch_vm.Control) -> None:
        """Retain the original directory before admitting any payload inode."""
        self.path, self.control = path, control
        control.check()
        self.descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            control.check()
            self.original = os.fstat(self.descriptor)
            require(
                path.resolve() == path
                and stat.S_ISDIR(self.original.st_mode)
                and self.original.st_uid == os.getuid()
                and stat.S_IMODE(self.original.st_mode) == PRIVATE_DIRECTORY,
                "receive_vm_ops_namespace",
            )
            self.names = tuple(sorted(os.listdir(self.descriptor)))  # noqa: PTH208 -- Inspect only the original directory FD.
            require(len(self.names) == SIGNED_OPS_FILES, "receive_vm_ops_members")
            self.check()
        except BaseException as error:
            failure = fetch_vm.settle((lambda: os.close(self.descriptor),), error)
            self.descriptor = -1
            if failure is not None:
                raise failure from None
            raise

    def check(self) -> None:
        """Compare original FD, visible namespace, member set and complete mutation stamp."""
        self.control.check()
        require(
            self.descriptor >= 0
            and ops_guest.stamp(os.fstat(self.descriptor)) == ops_guest.stamp(self.original)
            and ops_guest.stamp(self.path.lstat()) == ops_guest.stamp(self.original)
            and tuple(sorted(os.listdir(self.descriptor))) == self.names,  # noqa: PTH208 -- Original FD.
            "receive_vm_ops_namespace_changed",
        )
        self.control.check()

    def finish(self) -> None:
        """Attempt the original close even after a failed namespace check."""
        if self.descriptor < 0:
            return
        descriptor = self.descriptor
        primary: BaseException | None = None
        try:
            self.check()
        except BaseException as error:  # noqa: BLE001 -- Always attempt the original close.
            primary = error
        finally:
            primary = fetch_vm.settle((lambda: os.close(descriptor),), primary)
            self.descriptor = -1
        if primary is not None:
            raise primary


def ops_payloads(tree: OpsTree, owners: fetch_vm.Owners) -> dict[str, fetch_vm.Held]:
    """Retain all fifteen current payloads before any new verifier invocation."""
    result: dict[str, fetch_vm.Held] = {}
    for name in tree.names:
        tree.check()
        if name == "simplestchat-ops":
            maximum = ops_guest.MAX_BINARY
        elif name == "ops-attestation.jsonl":
            maximum = 8 * 1024**2
        elif name in {
            "ops-release.json",
            "ops-evidence.json",
            "ops-predicate.json",
            "ops-preparation.json",
        }:
            maximum = 65536
        else:
            require(
                re.fullmatch(r"ops-evidence-[0-9]{2}\.bin", name)
                and int(name.removeprefix("ops-evidence-").removesuffix(".bin")) < EVIDENCE_MEMBERS,
                "receive_vm_ops_member",
            )
            maximum = 1024**2
        held = fetch_vm.Held.open(tree.path / name, maximum, tree.control)
        owners.inputs.append(held)
        result[name] = held
    require(
        {
            "simplestchat-ops",
            "ops-release.json",
            "ops-evidence.json",
            "ops-predicate.json",
            "ops-preparation.json",
            "ops-attestation.jsonl",
        }
        <= set(result),
        "receive_vm_ops_members",
    )
    tree.check()
    return result


def check_ops(tree: OpsTree, payloads: Mapping[str, fetch_vm.Held]) -> None:
    """Recheck the complete retained namespace and every original before execution."""
    tree.check()
    require(tuple(sorted(payloads)) == tree.names, "receive_vm_ops_members")
    for original in payloads.values():
        original.check(tree.control)
    tree.check()


def current_ops_verification(  # noqa: PLR0913, PLR0917 -- Fixed original tree and authentication role.
    host: Host,
    tree: OpsTree,
    payloads: Mapping[str, fetch_vm.Held],
    controller: Path,
    selection: Mapping[str, object],
    credential: Mapping[str, str],
) -> None:
    """Require actual retained3 GH verification while these same originals remain held."""
    check_ops(tree, payloads)
    value = {
        "schemaVersion": 1,
        "selection": selection,
        "directory": str(tree.path),
        "output": str(host.output / "ops-reverified"),
        "authentication": "gh-token",
    }
    data = encoded(value)
    path = host.output / "ops-reverify-request.json"
    write_private(path, data, mode=0o600)
    request = fetch_vm.Held.open(path, FRAME_BYTES, host.control)
    primary: BaseException | None = None
    try:
        _ = host.command(
            "authenticate-retained-ops",
            [
                str(controller),
                "ops-artifact",
                "verify-retained",
                "--request",
                str(path),
                "--request-sha256",
                hashlib.sha256(data).hexdigest(),
            ],
            credential,
            min(600, host.control.body_end - time.monotonic()),
            (0,),
        )
        request.check(host.control)
        check_ops(tree, payloads)
    except BaseException as error:  # noqa: BLE001 -- Preserve body while closing the original request.
        primary = error
    finally:
        primary = fetch_vm.settle((request.close,), primary)
    if primary is not None:
        raise primary


def server_attempt(value: Mapping[str, object], run_id: int, run_attempt: int) -> None:
    """Bind the original producer's runId spelling, not the Envelope's buildRunId."""
    require(
        type(value.get("runId")) is int
        and value.get("runId") == run_id
        and type(value.get("runAttempt")) is int
        and value.get("runAttempt") == run_attempt,
        "receive_vm_same_attempt",
    )


def fresh_grant(host: Host, selection: reference.ReleaseSelection) -> str:
    """Use only the original GH token tool and a fixed API request, with no secret logs."""
    original = host.fixed_tools["gh"]
    original.check(host.control)
    remaining = host.control.body_end - time.monotonic()
    require(remaining > 0, "receive_vm_deadline")
    environment = dict(host.env, PATH="/usr/bin:/bin", GH_TOKEN=os.environ["GH_TOKEN"])
    status, token_bytes, error = bounded_process.run(
        [str(original.path), "auth", "token", "--hostname", "github.com"],
        cwd=ROOT,
        env=environment,
        limits=bounded_process.Limits(
            timeout=min(20, remaining),
            stdout=reference.MAX_TOKEN_BYTES,
            stderr=reference.MAX_TOKEN_BYTES,
            healthy=host.healthy,
        ),
    )
    original.check(host.control)
    host.control.check()
    require(status == 0 and not error, "receive_vm_token_unproven")
    token = token_bytes.decode("ascii").strip()
    require(
        0 < len(token) <= reference.MAX_TOKEN_BYTES
        and all(
            reference.FIRST_PRINTABLE < ord(item) < reference.DELETE_CHARACTER for item in token
        ),
        "receive_vm_token_unproven",
    )
    remaining = host.control.body_end - time.monotonic()
    require(remaining > 0, "receive_vm_deadline")
    connection = http.client.HTTPSConnection(
        "api.github.com", timeout=min(20, remaining), context=ssl.create_default_context()
    )
    primary: BaseException | None = None
    result = ""
    response: http.client.HTTPResponse | None = None
    try:
        connection.request(
            "GET",
            f"/repos/{selection.repository}/actions/artifacts/{selection.artifact_id}/zip",
            headers={
                "Authorization": "Bearer " + token,
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
                "User-Agent": "simplestchat-release-fetch",
            },
        )
        response = connection.getresponse()
        locations = [value for name, value in response.getheaders() if name.lower() == "location"]
        host.control.check()
        require(
            response.status == HTTPStatus.FOUND and len(locations) == 1, "receive_vm_grant_unproven"
        )
        result = reference.storage_url(locations[0])
    except BaseException as error:  # noqa: BLE001 -- Preserve first cause while closing the socket.
        primary = error
    finally:
        token = ""
        token_bytes = b""
        finalizers: list[Callable[[], None]] = []
        if response is not None:
            finalizers.append(response.close)
        finalizers.append(connection.close)
        primary = fetch_vm.settle(finalizers, primary)
    if primary is not None:
        raise primary
    original.check(host.control)
    host.control.check()
    return result


@final
class Session:
    """One fixed negative SSH transaction; private frames never become argv/logs."""

    def __init__(
        self, argv: Sequence[str], environment: Mapping[str, str], control: fetch_vm.Control
    ) -> None:
        """Register the sole child before any pipe/selector admission can fail."""
        self.control = control
        self.child: subprocess.Popen[bytes] | None = None
        self.selector = selectors.DefaultSelector()
        self.streams: list[BinaryIO] = []
        self.pending = b""
        self.buffer = bytearray()
        self.stdout_bytes = 0
        self.stderr_bytes = 0
        self.eof: set[str] = set()
        self.closed = False
        try:
            control.check()
            self.child = subprocess.Popen(  # noqa: S603 -- Fixed generated-loopback SSH argv only.
                list(argv),
                env=dict(environment),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            pipes = (self.child.stdin, self.child.stdout, self.child.stderr)
            self.streams.extend(cast("BinaryIO", stream) for stream in pipes if stream is not None)
            require(
                len(self.streams) == len(("stdin", "stdout", "stderr")), "receive_vm_pipe_missing"
            )
            for stream, label in zip(self.streams, ("stdin", "stdout", "stderr"), strict=True):
                os.set_blocking(stream.fileno(), False)
                if label != "stdin":
                    _ = self.selector.register(stream, selectors.EVENT_READ, label)
        except BaseException as error:
            failure = fetch_vm.settle((self.close,), error)
            if failure is not None:
                raise failure from None
            raise

    def step(self, end: float) -> None:
        """Poll cancellation, bounded concurrent pipes and the SAME original deadline."""
        self.control.check()
        remaining = min(end, self.control.body_end) - time.monotonic()
        require(remaining > 0, "receive_vm_session_deadline")
        for key, _ in self.selector.select(min(remaining, 0.1)):
            event = cast("object", key.data)
            if event == "stdin":
                sent = os.write(key.fd, self.pending[:65536])
                require(sent > 0, "receive_vm_input_incomplete")
                self.pending = self.pending[sent:]
                if not self.pending:
                    _ = self.selector.unregister(key.fd)
                continue
            chunk = os.read(key.fd, 65536)
            label = "stdout" if event == "stdout" else "stderr"
            if not chunk:
                self.eof.add(label)
                _ = self.selector.unregister(key.fd)
            elif label == "stdout":
                self.stdout_bytes += len(chunk)
                require(self.stdout_bytes <= MAX_RECORD, "receive_vm_stdout_bound")
                self.buffer.extend(chunk)
            else:
                self.stderr_bytes += len(chunk)
                require(self.stderr_bytes <= MAX_RECORD, "receive_vm_stderr_bound")
        self.control.check()

    def send(self, value: object) -> None:
        """Keep stdin open after each one-frame write, including the second frame."""
        require(not self.pending and self.child is not None, "receive_vm_session_state")
        data = encoded(value)
        require(len(data) <= FRAME_BYTES, "receive_vm_frame_bound")
        self.pending = data
        _ = self.selector.register(self.streams[0], selectors.EVENT_WRITE, "stdin")
        end = min(self.control.body_end, time.monotonic() + 30)
        while self.pending:
            self.step(end)

    def line(self, seconds: float) -> dict[str, object]:
        """Read exactly one bounded complete line; EOF does not invent a receipt."""
        end = min(self.control.body_end, time.monotonic() + seconds)
        while b"\n" not in self.buffer:
            require("stdout" not in self.eof, "receive_vm_missing_record")
            self.step(end)
        line, _, tail = self.buffer.partition(b"\n")
        self.buffer = bytearray(tail)
        return cast("dict[str, object]", object_value(decode_json(bytes(line))))

    def finish(self) -> int:
        """Close input, drain both original pipes, retire the reserved group, then reap."""
        require(self.child is not None and not self.pending, "receive_vm_session_state")
        if self.child is None:
            fail()
        self.streams[0].close()
        end = min(self.control.body_end, time.monotonic() + 20)
        while self.eof != {"stdout", "stderr"} or not bounded_process.leader_exited(self.child.pid):
            self.step(end)
        require(not self.buffer and self.stderr_bytes == 0, "receive_vm_extra_output")
        # Keep the terminal leader unreaped until the existing group finalizer
        # has signalled all remaining descendants. No post-reap PID adoption.
        bounded_process.stop(self.child, grace=1)
        status = self.child.returncode
        require(type(status) is int, "receive_vm_unreaped")
        self.close()
        self.control.check()
        return cast("int", status)

    def close(self) -> None:
        """Attempt every pipe/selector/group peer even after a previous error."""
        if self.closed:
            return
        self.closed = True
        actions: list[Callable[[], None]] = []
        child = self.child
        if child is not None and child.returncode is None:
            actions.append(lambda: bounded_process.stop(child, grace=1))
        actions.extend(stream.close for stream in self.streams)
        actions.append(self.selector.close)
        failure = fetch_vm.settle(actions)
        if failure is not None:
            raise failure


def call_guest(  # noqa: PLR0913 -- Fixed phase input plus independent original proof pins.
    host: Host,
    work: Path,
    port: int,
    phase: str,
    value: object = None,
    *,
    expected: Mapping[str, object] | None = None,
) -> dict[str, object]:
    """Send only a fixed phase to the source-bound guest assertion."""
    require(
        phase
        in {
            "install-route",
            "lock-negative",
            "journal-negative",
            "arm-conflict",
            "restore-conflict",
            "inspect",
        },
        "receive_vm_phase",
    )
    path = host.output / ("guest-" + phase + ".json")
    write_private(path, encoded({} if value is None else value), mode=0o600)
    ssh = host.fixed_tools["ssh"]
    command = [
        "/usr/bin/sudo",
        "-n",
        "/usr/bin/python3",
        "-I",
        "-B",
        "/root/simplestchat-vm-fixture/security_vm_receive_guest.py",
        phase,
    ]
    argv = [
        str(ssh.path),
        *vm.ssh_options(work),
        "-i",
        str(work / "client"),
        "-p",
        str(port),
        "fixture@127.0.0.1",
        shlex.join(command),
    ]
    original = fetch_vm.Held.open(path, MAX_RECORD, host.control)
    data = b""
    primary: BaseException | None = None
    try:
        _, data = host.run("receive-" + phase, argv, input_file=original.stream, timeout=120)
        original.check(host.control)
    except BaseException as error:  # noqa: BLE001 -- Preserve body while closing the original input.
        primary = error
    finally:
        primary = fetch_vm.settle((original.close,), primary)
    if primary is not None:
        raise primary
    observed = cast("dict[str, object]", object_value(decode_json(data)))
    bound = cast("Mapping[str, object]", value) if phase == "install-route" else expected
    return guest_observation(phase, observed, bound)


def write_transfer(
    path: Path, held: Mapping[str, fetch_vm.Held], control: fetch_vm.Control
) -> None:
    """Own the fixed three-member USTAR writer without replacing a known source failure."""
    descriptor = -1
    raw: BinaryIO | None = None
    builder: tarfile.TarFile | None = None
    primary: BaseException | None = None
    try:
        descriptor = os.open(
            path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600
        )
        raw = os.fdopen(descriptor, "wb")
        descriptor = -1
        builder = tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT)  # noqa: SIM115 -- Preserve first body error across both closes.
        try:
            for name in ops_guest.NAMES:
                original = held[name]
                original.check(control)
                _ = original.stream.seek(0)
                member = tarfile.TarInfo(name)
                member.size, member.mode, member.uid, member.gid = original.size, 0o600, 0, 0
                builder.addfile(member, original.stream)
                original.check(control)
        except BaseException as error:  # noqa: BLE001 -- Keep the source/write failure through TAR close.
            primary = error
        finally:
            primary = fetch_vm.settle((builder.close,), primary)
        if primary is None:
            raw.flush()
            os.fsync(raw.fileno())
    except BaseException as error:  # noqa: BLE001 -- Attempt writer/descriptor close after every refusal.
        primary = primary or error
    finally:
        finalizers: list[Callable[[], None]] = []
        if raw is not None:
            finalizers.append(raw.close)
        if descriptor >= 0:
            finalizers.append(lambda: os.close(descriptor))
        primary = fetch_vm.settle(finalizers, primary)
    if primary is not None:
        raise primary


def transfer(  # noqa: PLR0913, PLR0917 -- Independent retained transfer inputs.
    host: Host,
    owners: fetch_vm.Owners,
    verified: Mapping[str, fetch_vm.Held],
    selection: Mapping[str, object],
    binary: Path,
    output: Path,
    run_id: str,
    artifact_id: int,
) -> tuple[ops_vm.Input, BinaryIO]:
    """Use owned authentication success, then pin/measure the three transferred originals."""
    held = {
        "ops-release.json": verified["ops-release.json"],
        "simplestchat-ops": verified["simplestchat-ops"],
    }
    selected = encoded(selection).rstrip(b"\n")
    path = output / "ops-selection.json"
    write_private(path, selected, mode=0o600)
    selected_original = fetch_vm.Held.open(path, 16384, host.control)
    owners.inputs.append(selected_original)
    held["ops-selection.json"] = selected_original
    for original in held.values():
        original.check(host.control)
    _, measured = host.run(
        "measure-signed-executable",
        [
            str(binary),
            "distribution",
            "inspect-executable",
            "--path",
            str(held["simplestchat-ops"].path),
        ],
        timeout=120,
    )
    executable_identity = ops_guest.executable(
        measured, held["simplestchat-ops"].sha256, held["simplestchat-ops"].size
    )
    install: dict[str, object] = {"schemaVersion": 1}
    for key, name in zip(("manifest", "selection", "binary"), ops_guest.NAMES, strict=True):
        original = held[name]
        install[key] = {
            "path": str(ops_guest.INPUT / name),
            "sha256": original.sha256,
            "bytes": original.size,
        }
    install_bytes = encoded(install).rstrip(b"\n")
    archive_path = output / "ops-transfer.tar"
    write_transfer(archive_path, held, host.control)
    transfer_pin = fetch_vm.Held.open(archive_path, ops_guest.MAX_TRANSFER, host.control)
    owners.inputs.append(transfer_pin)
    values = {
        "run-id": run_id,
        "selection-json": selected.decode(),
        "artifact-id": str(artifact_id),
        "transfer-sha256": transfer_pin.sha256,
        "transfer-bytes": str(transfer_pin.size),
        "install-request-json": install_bytes.decode(),
        "install-request-sha256": hashlib.sha256(install_bytes).hexdigest(),
        "executable-json": json.dumps(executable_identity, separators=(",", ":")),
    }
    for name, prefix in zip(ops_guest.NAMES, ("manifest", "selection", "binary"), strict=True):
        values[prefix + "-sha256"] = held[name].sha256
        values[prefix + "-bytes"] = str(held[name].size)
    options = ops_guest.Options.parse(
        [part for key, value in values.items() for part in ("--" + key, value)]
    )
    loan = archive_path.open("rb")
    owned: ops_vm.Input | None = None
    try:
        owned = ops_vm.Input(loan.fileno(), options)
        owned.admit(cast("ops_guest.Control", cast("object", host.control)))
    except BaseException as error:
        loan_finalizers: list[Callable[[], None]] = []
        if owned is not None:
            loan_finalizers.append(owned.close)
        loan_finalizers.append(loan.close)
        failure = fetch_vm.settle(loan_finalizers, error)
        if failure is not None:
            raise failure from None
        raise
    return owned, loan


def prepared_inventory(  # noqa: PLR0913, PLR0917 -- Original owner plus fixed generated identity.
    owners: fetch_vm.Owners,
    work: Path,
    port: int,
    run_id: str,
    revision: str,
    control: fetch_vm.Control,
) -> Path:
    """Reuse the original Ops inventory instead of recreating its exclusive leaf."""
    original = fetch_vm.Held.open(work / "inventory.json", 16384, control)
    owners.inputs.append(original)
    value = private_record(original.path)
    hosts = cast("dict[str, object]", value.get("benchmark_hosts"))
    targets = cast("dict[str, object]", hosts.get("hosts"))
    require(set(targets) == {"vmfixture"}, "receive_vm_inventory")
    target = cast("dict[str, object]", targets["vmfixture"])
    require(
        target.get("ansible_host") == "127.0.0.1"
        and target.get("ansible_port") == port
        and target.get("ansible_user") == "fixture"
        and target.get("ansible_ssh_private_key_file") == str(work / "client")
        and target.get("ansible_ssh_args") == shlex.join(vm.ssh_options(work))
        and target.get("vm_fixture_run_id") == run_id
        and target.get("vm_fixture_revision") == revision,
        "receive_vm_inventory",
    )
    original.check(control)
    return original.path


def route_observation(value: Mapping[str, object], expected: Mapping[str, object]) -> None:
    """Require the declared adapter and authenticated installed pin before client effects."""
    require(
        set(value)
        == {
            "installedRoute",
            "binarySha256",
            "routeSha256",
            "timeoutSeconds",
            "finalizationSeconds",
            "hostCurl",
        }
        and value.get("installedRoute") is True
        and value.get("binarySha256") == expected["binarySha256"]
        and value.get("routeSha256") == expected["routeSha256"]
        and type(value.get("timeoutSeconds")) is int
        and value.get("timeoutSeconds") == HOST_TIMEOUT
        and type(value.get("finalizationSeconds")) is int
        and value.get("finalizationSeconds") == HOST_FINALIZATION,
        "receive_vm_route_record",
    )
    raw = value.get("hostCurl")
    require(isinstance(raw, dict), "receive_vm_route_record")
    curl = cast("dict[str, object]", raw)
    require(
        set(curl) == {"sha256", "bytes", "version", "opensslAdvertised", "caSha256", "caBytes"}
        and curl.get("opensslAdvertised") is True
        and isinstance(curl.get("version"), str)
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", str(curl["version"]))
        and all(
            isinstance(curl.get(key), str) and re.fullmatch("[a-f0-9]{64}", str(curl[key]))
            for key in ("sha256", "caSha256")
        )
        and all(
            type(curl.get(key)) is int and cast("int", curl[key]) > 0
            for key in ("bytes", "caBytes")
        ),
        "receive_vm_route_record",
    )


def final_observation(
    value: Mapping[str, object], run_id: str, revision: str, published: Mapping[str, object]
) -> None:
    """Close the exact independent attempt census and original publication binding."""
    require(
        set(value)
        == {
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "classes",
            "published",
            "lockReleased",
            "passed",
        }
        and type(value.get("schemaVersion")) is int
        and value.get("schemaVersion") == 1
        and value.get("scope") == "SignedMuslReceiveGuestProbe"
        and value.get("runId") == run_id
        and value.get("revision") == revision
        and value.get("classes")
        == {"complete": 2, "retained_artifact_differs": 1, "zip_checksum_mismatch": 1}
        and value.get("published") == published
        and value.get("lockReleased") is True
        and value.get("passed") is True,
        "receive_vm_final_probe",
    )


def guest_observation(
    phase: str, value: Mapping[str, object], expected: Mapping[str, object] | None = None
) -> dict[str, object]:
    """Project only exact public phase facts; unknown guest fields never reach summary."""
    if phase == "install-route":
        require(expected is not None, "receive_vm_route_record")
        if expected is None:
            fail()
        route_observation(value, expected)
        return {
            key: value[key]
            for key in (
                "installedRoute",
                "binarySha256",
                "routeSha256",
                "timeoutSeconds",
                "finalizationSeconds",
                "hostCurl",
            )
        }
    if phase in ("lock-negative", "journal-negative"):
        require(
            set(value)
            == {
                "case",
                "nativeExitStatus",
                "noReady",
                "noNewAttempt",
                "publicationUnchanged",
                "lockReleased",
            }
            and value.get("case") == phase
            and type(value.get("nativeExitStatus")) is int
            and value.get("nativeExitStatus") == 1
            and all(
                value.get(key) is True
                for key in ("noReady", "noNewAttempt", "publicationUnchanged", "lockReleased")
            ),
            "receive_vm_negative_record",
        )
        return {
            "case": phase,
            "nativeExitStatus": 1,
            "noReady": True,
            "noNewAttempt": True,
            "publicationUnchanged": True,
            "lockReleased": True,
        }
    if phase in ("arm-conflict", "restore-conflict"):
        restored = phase == "restore-conflict"
        require(
            set(value) == {"conflictRestored", "fixtureOnly"}
            and value.get("conflictRestored") is restored
            and value.get("fixtureOnly") is True,
            "receive_vm_conflict_record",
        )
        return {"conflictRestored": restored, "fixtureOnly": True}
    require(phase == "inspect" and expected is not None, "receive_vm_probe_record")
    if expected is None:
        fail()
    run_id, revision = str(expected["runId"]), str(expected["revision"])
    published = cast("dict[str, object]", expected["published"])
    final_observation(value, run_id, revision, published)
    classes = cast("dict[str, object]", value["classes"])
    require(all(type(item) is int for item in classes.values()), "receive_vm_probe_record")
    return {
        "schemaVersion": 1,
        "scope": "SignedMuslReceiveGuestProbe",
        "runId": run_id,
        "revision": revision,
        "published": published,
        "classes": {"complete": 2, "retained_artifact_differs": 1, "zip_checksum_mismatch": 1},
        "lockReleased": True,
        "passed": True,
    }


def checksum_negative(
    host: Host, work: Path, port: int, selected: Mapping[str, object], envelope: dict[str, object]
) -> dict[str, object]:
    """Obtain a new real target AFTER Ready; retain the genuine complete ZIP on mismatch."""
    ssh = host.fixed_tools["ssh"]
    values = reference.FetchOptions(
        repository=fetch_vm.REPOSITORY,
        revision=str(selected["revision"]),
        artifact_id=cast("int", selected["artifactId"]),
        ci_run=cast("int", selected["ciRunId"]),
        host="127.0.0.1",
        port=port,
        user="fixture",
        identity=str(work / "client"),
        output_parent=str(host.output),
    )
    argv = reference.ssh_arguments(values)
    argv[0] = str(ssh.path)
    session = Session(argv, reference.ssh_environment(), host.control)
    primary: BaseException | None = None
    result: dict[str, object] = {}
    try:
        wrong = "0" * 64 if envelope["zipSha256"] != "0" * 64 else "1" * 64
        session.send(dict(envelope, zipSha256=wrong))
        ready = session.line(45)
        require(
            ready == {"schemaVersion": 1, "status": "ready"}
            and type(ready["schemaVersion"]) is int,
            "receive_vm_not_ready",
        )
        host.control.check()
        grant = fresh_grant(host, values)
        host.control.check()
        session.send({"url": grant})
        grant = ""
        receipt = session.line(315)
        status = session.finish()
        require(
            status == 1
            and receipt.get("passed") is False
            and receipt.get("settled") is True
            and receipt.get("finalized") is False
            and receipt.get("phase") == "download"
            and receipt.get("failureClass") == "zip_checksum_mismatch"
            and receipt.get("zipSha256") == wrong
            and receipt.get("downloadedBytes") == envelope["artifactZipBytes"],
            "receive_vm_checksum_negative",
        )
        result = {
            "case": "retained-download-checksum",
            "nativeExitStatus": status,
            "readyBeforeNewGrant": True,
            "fullRetainedZip": True,
            "passed": True,
        }
    except BaseException as error:  # noqa: BLE001 -- Preserve body while retiring every session peer.
        primary = error
    finally:
        primary = fetch_vm.settle((session.close,), primary)
    if primary is not None:
        raise primary
    return result


def run(  # noqa: C901, PLR0912, PLR0915 -- Keep ordered ownership and finalizers in one transaction.
    options: Options, control: fetch_vm.Control
) -> dict[str, object]:
    """Keep all authentication, guest effects, original inputs and finalizers in one owner."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslReleaseReceiveKvm",
        "passed": False,
        "cleanupPassed": False,
        "failure": "guard",
        "trials": [],
        "authenticatedOpsDistribution": False,
        "hostTarget": "x86_64-unknown-linux-musl",
        "clientTarget": "x86_64-unknown-linux-gnu",
        "route": "fixture-only-pinned-exec-wrapper-product-command-unchanged",
        "partialTransferClaim": False,
    }
    owners = fetch_vm.Owners()
    native_tools: fetch_vm.NativeTools | None = None
    ops_tree: OpsTree | None = None
    archive: ops_vm.Input | None = None
    loan: BinaryIO | None = None
    first: BaseException | None = None
    try:
        home = admission(os.environ)
        output = options.output
        require(
            output.is_absolute() and output.resolve() == output and not output.exists(),
            "receive_vm_output",
        )
        require(os.environ.get("GH_TOKEN"), "receive_vm_token_missing")
        revision = os.environ.get("GITHUB_SHA", "")
        require(re.fullmatch("[a-f0-9]{40}", revision), "receive_vm_revision")
        vm.preflight(output)
        output.mkdir(mode=0o700)
        host = Host(output, control)
        source_hashes: dict[str, str] = {}
        source_originals: dict[str, fetch_vm.Held] = {}
        for name in SOURCE_INPUTS:
            original = fetch_vm.Held.open(ROOT / name, 2**20, control)
            owners.inputs.append(original)
            source_hashes[name] = original.sha256
            source_originals[name] = original
        report["sourceSha256"] = source_hashes
        binary = ROOT / "target/x86_64-unknown-linux-gnu/release/simplestchat-ops"
        controller = ROOT / "target/debug/xtask"
        programs: list[Path] = []
        for path in (binary, controller):
            original = fetch_vm.Held.open(path.resolve(), fetch_vm.MAX_BINARY, control)
            owners.inputs.append(original)
            programs.append(original.path)
        binary, controller = programs
        original_tools: dict[str, fetch_vm.Held] = {}
        for name in fetch_vm.TOOL_ROLES:
            tool = fetch_vm.Held.open(
                Path(executable(name)).resolve(), fetch_vm.MAX_BINARY, control, system=True
            )
            owners.inputs.append(tool)
            original_tools[name] = tool
        host.fixed_tools = original_tools
        host.env["ANSIBLE_SSH_EXECUTABLE"] = str(original_tools["ssh"].path)
        owners.work = fetch_vm.Work.create(output)
        work = owners.work.path
        native_tools = fetch_vm.NativeTools.create(owners.work, original_tools, control)
        credential = credential_environment(host, native_tools)
        cargo = source_originals["Cargo.toml"]
        cargo.check(control)
        cargo_data = cargo.stream.read(2**20 + 1)
        require(len(cargo_data) == cargo.size, "receive_vm_source_manifest")
        package = cast(
            "object", tomllib.loads(cargo_data.decode())["workspace"]["package"]["version"]
        )
        require(isinstance(package, str), "receive_vm_source_manifest")
        cargo.check(control)
        selected_ops = {
            "repository": fetch_vm.REPOSITORY,
            "revision": revision,
            "packageVersion": package,
            "target": "x86_64-unknown-linux-musl",
            "protocolVersion": 1,
            "buildRunId": options.run_id,
            "runAttempt": options.run_attempt,
        }
        request = {
            "schemaVersion": 1,
            "selection": selected_ops,
            "artifactId": options.artifact_id,
            "output": str(output / "ops-acquired"),
            "authentication": "gh-token",
        }
        request_data = encoded(request)
        request_path = output / "ops-request.json"
        write_private(request_path, request_data, mode=0o600)
        report["failure"] = "ops-authentication"
        native_tools.check(control)
        _ = host.command(
            "authenticate-ops",
            [
                str(controller),
                "ops-artifact",
                "acquire",
                "--request",
                str(request_path),
                "--request-sha256",
                hashlib.sha256(request_data).hexdigest(),
            ],
            credential,
            min(1200, control.body_end - time.monotonic()),
            (0,),
        )
        # Authority is the current owned successful acquisition invocation, not
        # acquisition-observation.json; pins below are retained payload facts.
        native_tools.check(control)
        ops_tree = OpsTree(output / "ops-acquired/input", control)
        retained = ops_payloads(ops_tree, owners)
        report["failure"] = "ops-retained-authentication"
        native_tools.check(control)
        current_ops_verification(host, ops_tree, retained, controller, selected_ops, credential)
        native_tools.check(control)
        report["authenticatedOpsDistribution"] = True
        report["opsVerifierCalls"] = 6
        report["opsSelection"] = selected_ops
        report["opsArtifactId"] = options.artifact_id
        report["failure"] = "server-authentication"
        native_tools.check(control)
        acquired = output / "server-acquired"
        _ = host.command(
            "authenticate-server",
            [
                sys.executable,
                "-B",
                str(ROOT / "build/security_vm_release.py"),
                "--repository",
                fetch_vm.REPOSITORY,
                "--output",
                str(acquired),
            ],
            credential,
            min(1200, control.body_end - time.monotonic()),
            (0,),
        )
        native_tools.check(control)
        selected = fetch_vm.selection(acquired / "selection.json", revision)
        require(selected["ciRunId"] == options.run_id, "receive_vm_same_head_run")
        pins: dict[str, fetch_vm.Held] = {}
        for name, path, maximum in (
            ("selection", acquired / "selection.json", 16384),
            ("image.tar", acquired / "artifact/image.tar", positive_limit()),
            ("release.json", acquired / "artifact/release.json", 2**20),
            ("artifact.zip", acquired / "artifact/artifact.zip", positive_limit()),
        ):
            original = fetch_vm.Held.open(path, maximum, control)
            owners.inputs.append(original)
            pins[name] = original
        for name in (
            "outcome.json",
            "source.json",
            "sbom.spdx.json",
            "image-security.json",
            "runtime-proof.json",
            "vex.openvex.json",
            "release-predicate.json",
            "release-attestation.jsonl",
        ):
            original = fetch_vm.Held.open(acquired / "artifact" / name, 64 * 1024**2, control)
            owners.inputs.append(original)
            pins[name] = original
        predicate = private_record(acquired / "artifact/release-predicate.json")
        server_attempt(predicate, options.run_id, options.run_attempt)
        report.update(
            {
                "revision": revision,
                "serverArtifactId": selected["artifactId"],
                "serverCiRunId": selected["ciRunId"],
                "zipSha256": pins["artifact.zip"].sha256,
            }
        )
        run_id = uuid.uuid4().hex
        report["runId"] = run_id
        check_ops(ops_tree, retained)
        archive, loan = transfer(
            host, owners, retained, selected_ops, binary, output, run_id, options.artifact_id
        )
        pin = vm.CloudImage.read()
        report["cloudImageSha512"] = pin.sha512
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", listener.getsockname())[1]
        report["failure"] = "boot"
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, run_id)
        guest = owners.guest = vm.Guest(
            host, vm.qemu_command(work, port, run_id), qmp_path=work / "qmp.sock"
        )
        host.guest = guest
        vm.boot(host, guest, work, port)
        report["failure"] = "authenticated-install"
        ops_vm.stage(cast("ops_vm.Host", cast("object", host)), guest, work, port, archive.options)
        check_ops(ops_tree, retained)
        observed = ops_vm.exercise(
            cast("ops_vm.Host", cast("object", host)), guest, work, port, archive
        )
        require(observed["passed"] is True, "receive_vm_install_unproven")
        report["install"], report["opsCases"] = observed["install"], observed["cases"]
        path = prepared_inventory(owners, work, port, run_id, revision, control)
        _, stage = host.run(
            "receive-stage",
            [
                executable("ansible-playbook"),
                "-i",
                str(path),
                str(ROOT / "security/vm/receive.yml"),
            ],
            timeout=180,
        )
        recaps = list(vm.RECAP.finditer(stage.decode()))
        require(
            len(recaps) == 1 and int(recaps[0][3]) == 0 and int(recaps[0][4]) == 0,
            "receive_vm_stage",
        )
        route_value = {
            "schemaVersion": 1,
            "runId": run_id,
            "revision": revision,
            "binarySha256": archive.options.pins["simplestchat-ops"][0],
            "binaryBytes": archive.options.pins["simplestchat-ops"][1],
            "routeSha256": source_hashes["build/security_vm_receive_route.py"],
            "originalRouteSha256": source_hashes["ops/ansible/files/fetch-release.py"],
            "artifactId": selected["artifactId"],
            "zipSha256": pins["artifact.zip"].sha256,
            "zipBytes": pins["artifact.zip"].size,
            "imageSha256": pins["image.tar"].sha256,
            "manifestSha256": pins["release.json"].sha256,
            "memberPins": {
                name: {"sha256": original.sha256, "bytes": original.size}
                for name, original in pins.items()
                if name not in ("selection", "artifact.zip")
            },
        }
        route_result = call_guest(host, work, port, "install-route", route_value)
        route_observation(route_result, route_value)
        report["routeInstalled"] = route_result
        owners.known = KnownHost(home, port, bounded_file(work / "host.pub", 4096).decode().strip())
        environment = dict(
            host.env, PATH=str(native_tools.directory), GH_TOKEN=os.environ["GH_TOKEN"]
        )
        previous: dict[str, object] | None = None
        envelope: dict[str, object] | None = None
        for count, label in enumerate(("cached-local-first", "local-download-repeat"), 1):
            report["failure"] = label
            owners.known.check()
            native_tools.check(control)
            for original in owners.inputs:
                original.check(control)
            _, data = host.command(
                label,
                fetch_vm.native_arguments(
                    binary,
                    work,
                    port,
                    selected,
                    acquired,
                    output / label,
                    fetch_vm.native_budget(control),
                    cached=count == 1,
                ),
                environment,
                min(1100, control.body_end - time.monotonic()),
                (0,),
            )
            owners.known.check()
            native_tools.check(control)
            receipt = fetch_vm.native_receipt(data, selected, pins)
            observation = fetch_vm.observe_guest(host, work, port, run_id, selected, pins, count)
            current = cast("dict[str, object]", observation["published"])
            require(previous is None or current == previous, "receive_vm_repeat_changed")
            previous = current
            cast("list[object]", report["trials"]).append(
                {"name": label, **receipt, "independentProbe": True}
            )
            if count == 1:
                candidates = list((output / label).glob("release-fetch.*/artifact.json"))
                require(len(candidates) == 1, "receive_vm_envelope_original")
                envelope = private_record(candidates[0])
        require(envelope is not None, "receive_vm_envelope_missing")
        if envelope is None:
            fail()
        for phase in ("lock-negative", "journal-negative"):
            report["failure"] = phase
            cast("list[object]", report["trials"]).append(
                call_guest(host, work, port, phase, {"envelope": envelope})
            )
        report["failure"] = "publication-conflict"
        _ = call_guest(host, work, port, "arm-conflict")
        conflict_error: BaseException | None = None
        status = 1
        try:
            status, data = host.command(
                "publication-conflict",
                fetch_vm.native_arguments(
                    binary,
                    work,
                    port,
                    selected,
                    acquired,
                    output / "publication-conflict",
                    fetch_vm.native_budget(control),
                    cached=True,
                ),
                environment,
                min(1100, control.body_end - time.monotonic()),
                (1,),
            )
            result = cast("dict[str, object]", object_value(decode_json(data)))
            raw_remote: object = result.get("remote")
            require(isinstance(raw_remote, dict), "receive_vm_remote_shape")
            remote = cast("dict[str, object]", raw_remote)
            require(
                status == 1
                and result.get("passed") is False
                and result.get("remoteSettled") is True
                and remote.get("passed") is False
                and remote.get("finalized") is False
                and remote.get("phase") == "publish"
                and remote.get("failureClass") == "retained_artifact_differs",
                "receive_vm_conflict_unproven",
            )
        except BaseException as error:  # noqa: BLE001 -- Keep body failure while always attempting restore.
            conflict_error = error
        finally:

            def restore_conflict() -> None:
                _ = call_guest(host, work, port, "restore-conflict")

            conflict_error = fetch_vm.settle((restore_conflict,), conflict_error)
        if conflict_error is not None:
            raise conflict_error  # noqa: TRY301 -- Outer owner must receive the original body cause.
        cast("list[object]", report["trials"]).append(
            {"name": "publication-conflict", "passed": True, "nativeExitStatus": status}
        )
        owners.known.check()
        native_tools.check(control)
        report["failure"] = "retained-download"
        cast("list[object]", report["trials"]).append(
            checksum_negative(host, work, port, selected, envelope)
        )
        owners.known.check()
        native_tools.check(control)
        final = call_guest(
            host,
            work,
            port,
            "inspect",
            expected={"runId": run_id, "revision": revision, "published": previous},
        )
        require(previous is not None, "receive_vm_publication_missing")
        final_observation(final, run_id, revision, cast("dict[str, object]", previous))
        report["independentProbe"] = final
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- All original resources remain owned through every refusal.
        first = error
    finally:
        control.settling = True
        extra: list[Callable[[], None]] = []
        if native_tools is not None:
            extra.append(lambda tools=native_tools: tools.check(control))
        if archive is not None:
            extra.extend(
                (
                    lambda: archive.check(cast("ops_guest.Control", cast("object", control))),
                    archive.close,
                )
            )
        if loan is not None:
            extra.append(loan.close)
        if ops_tree is not None:
            extra.append(ops_tree.finish)
        cleanup = fetch_vm.settle(extra)
        owned_cleanup = owners.finish(control, report)
        cleanup = cleanup or owned_cleanup
        first = first or cleanup
        report["cleanupPassed"] = cleanup is None
        if report.get("failure") is None and first is not None:
            report["failure"] = "unproven"
        if cleanup is not None and report["failure"] is None:
            report["failure"] = "cleanup"
        report["passed"] = first is None and cleanup is None and not control.status
    return report


def positive_limit() -> int:
    """Retain the host receiver's original two-GiB archive bound."""
    return 2 * 1024**3


class Parser(argparse.ArgumentParser):
    """Keep arbitrary input spellings out of the public console."""

    @override
    def error(self, message: str) -> NoReturn:
        """Return one fixed syntax error, without echoing argv."""
        _ = message
        self.exit(2, "Receiver VM artifact selectors are invalid.\n")


def main() -> int:  # noqa: C901 -- Restore all handlers/umask before one final status fence.
    """Fence positive status AFTER output, handler restoration and original umask."""
    control = fetch_vm.Control()
    parser = Parser(allow_abbrev=False)
    for name in ("run-id", "run-attempt", "artifact-id"):
        _ = parser.add_argument("--" + name, required=True, type=int)
    _ = parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args(namespace=Options())
    if not all(
        0 < getattr(options, name) <= SAFE for name in ("run_id", "run_attempt", "artifact_id")
    ):
        return 2
    previous_umask = os.umask(0o077)
    handlers: dict[signal.Signals, object] = {}
    status = 1
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        report = run(options, control)
        data = encoded(report)
        require(len(data) <= MAX_RECORD, "receive_vm_summary_bound")
        if options.output.is_dir():
            write_private(options.output / "summary.json", data, mode=0o600)
        require(sys.stdout.buffer.write(data) == len(data), "receive_vm_stdout_incomplete")
        _ = sys.stdout.buffer.flush()
        status = control.status or (0 if report["passed"] is True else 1)
    except (OSError, ValueError, ToolError, RuntimeError):
        _ = sys.stderr.write("Signed musl receiver VM proof is unproven.\n")
        status = control.status or 1
    finally:
        actions: list[Callable[[], None]] = []
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_umask() -> None:
            _ = os.umask(previous_umask)

        actions.append(restore_umask)
        if fetch_vm.settle(actions) is not None:
            status = control.status or 1
    try:
        control.check()
    except (OSError, ValueError, ToolError):
        status = control.status or 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
