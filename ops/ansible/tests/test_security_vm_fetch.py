"""Inert whole-keeper policy tests; every SSH home and input is test-owned."""

from __future__ import annotations

import hashlib
import io
import json
import os
import pwd
import shutil
import signal
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override
from unittest.mock import patch

from test_support import ROOT, array, obj, objects, string, yaml_value

sys.path.insert(0, str(ROOT / "build"))
import bounded_process
import security_vm as vm
import security_vm_fetch as keeper
from release_json import decode_json
from security_tools import ToolError
from security_vm_fetch_hosts import KnownHost

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from types import FrameType
    from typing import BinaryIO

REVISION = "a" * 40
RUN_ID = "b" * 32
TOKEN = "INERT_VM_KEEPER_TOKEN"  # noqa: S105 -- Fixed non-secret credential-isolation sentinel.
KEY = "ssh-ed25519 " + "A" * 68 + " vm-fixture"
CI = {
    "GITHUB_ACTIONS": "true",
    "RUNNER_ENVIRONMENT": "github-hosted",
    "GITHUB_REPOSITORY": keeper.REPOSITORY,
    "GITHUB_REF": "refs/heads/main",
    "GITHUB_WORKFLOW": keeper.WORKFLOW,
    "GITHUB_SHA": REVISION,
    "GH_TOKEN": TOKEN,
}


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create only fixed inert data, with a closed real original handle."""
    with path.open("xb") as output:
        _ = output.write(data)
    path.chmod(mode)


@final
class FakeGuest:
    """Observe only inert VM boundaries; no QEMU/SSH process is created."""

    def __init__(self, host: keeper.Host, _argv: Sequence[str], *, qmp_path: Path) -> None:
        """Retain the same host and path so cleanup ordering is observable."""
        self.host, self.path = host, qmp_path
        self.failure: str | None = None
        self.closed = False
        self.authentication_observed = False
        self.cloud_init: dict[str, object] = {}

    def authenticated_ssh(self) -> None:
        """Record only inert SSH readiness supplied to the original boot policy."""
        self.authentication_observed = True

    def require_alive(self) -> None:
        """Refuse any observation after the inert VM owner was closed."""
        if self.closed:
            raise AssertionError

    def close(self) -> None:
        """Record the actual fixture close before optionally injecting uncertainty."""
        self.closed = True
        if getattr(self.host, "close_fault", False):
            message = "INERT_PRIVATE_CLEANUP_TEXT"
            raise OSError(message)

    def diagnostics(self) -> dict[str, object]:
        """Provide only documented VM lifetime observations at this inert boundary."""
        return {
            "cleanupReturnCode": cast("FakeHost", self.host).qemu_status,
            "authenticatedSsh": True,
            "drainer": "completed",
            "qmp": {
                "connected": True,
                "greetingReceived": True,
                "capabilitiesNegotiated": True,
                "eofObserved": True,
                "failure": None,
            },
            "cloudInit": {"passed": True},
        }


@final
class FakeHost(keeper.Host):
    """Keep all real keeper/source/known-host operations while adapting external processes."""

    def __init__(self, output: Path, control: keeper.Control) -> None:
        """Use actual Host credential-free environment construction."""
        super().__init__(output, control)
        self.close_fault = False
        self.native_fault = False
        self.inode_fault = False
        self.qemu_status: int | bool | None = 0
        self.native_calls: list[tuple[list[str], dict[str, str]]] = []
        self.acquired = output.parent / "acquired"

    @override
    def command(
        self,
        name: str,
        argv: Sequence[str],
        environment: Mapping[str, str],
        timeout: float,
        accepted: tuple[int, ...],
    ) -> tuple[int, bytes]:
        """Adapt only process outputs, retaining fixed argv and credential-role assertions."""
        self.control.check()
        self.checks.append({"name": name, "exitStatus": 0})
        if name in keeper.TRIALS:
            self.native_calls.append((list(argv), dict(environment)))
            if self.native_fault:
                message = "INERT_PRIVATE_NATIVE_TEXT"
                raise ToolError(message)
            remote: dict[str, object] = {
                "passed": True,
                "settled": True,
                "finalized": True,
                "status": "complete",
                "phase": "complete",
                "failureClass": None,
                "revision": REVISION,
                "artifactId": 123,
                "zipSha256": hashlib.sha256(b"inert zip").hexdigest(),
                "artifactZipBytes": len(b"inert zip"),
                "downloadedBytes": len(b"inert zip"),
                "archiveSha256": hashlib.sha256(b"inert image").hexdigest(),
                "manifestSha256": hashlib.sha256(
                    (self.acquired / "artifact/release.json").read_bytes()
                ).hexdigest(),
            }
            self.assert_native(argv, environment, timeout, accepted)
            return 0, (
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "operation": "fetch_release_controller",
                        "passed": True,
                        "phase": "complete",
                        "remoteSettled": True,
                        "failureClass": None,
                        "remote": remote,
                    }
                )
                + "\n"
            ).encode()
        if "GH_TOKEN" in environment or "GITHUB_TOKEN" in environment:
            raise AssertionError
        if name == "fetch-stage":
            return (
                0,
                b"vmfixture : ok=8 changed=7 unreachable=0 failed=0 "
                + b"skipped=0 rescued=0 ignored=0\n",
            )
        if name.startswith("probe-"):
            count = int(name[-1])
            values: dict[str, object] = {}
            for index, member in enumerate(("image.tar", "release.json"), 1):
                data = (self.acquired / "artifact" / member).read_bytes()
                values[member] = {
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "bytes": len(data),
                    "device": 1,
                    "inode": index + int(self.inode_fault and count == len(keeper.TRIALS)),
                }
            return 0, json.dumps(
                {
                    "schemaVersion": 1,
                    "scope": "ReleaseFetchGuestProbe",
                    "runId": RUN_ID,
                    "revision": REVISION,
                    "attempts": count,
                    "published": values,
                    "passed": True,
                }
            ).encode()
        raise AssertionError

    def assert_native(
        self, argv: Sequence[str], env: Mapping[str, str], timeout: float, accepted: tuple[int, ...]
    ) -> None:
        """Verify the real controller gets only fixed targets, bounded N/M and its API token."""
        if not (
            argv[1:3] == ["release", "fetch"]
            and env.get("GH_TOKEN") == TOKEN
            and "-F" not in argv
            and "--host" in argv
            and Path(env["PATH"]).name == "native-bin"
            and {entry.name for entry in Path(env["PATH"]).iterdir()} == set(keeper.TOOL_ROLES)
            and accepted == (0,)
            and timeout <= keeper.NATIVE_MAX + 5
        ):
            raise AssertionError


def prepare(root: Path) -> tuple[Path, Path]:
    """Create tiny inert source files; no candidate is executable in these unit tests."""
    acquired = root / "acquired"
    (acquired / "artifact").mkdir(parents=True, mode=0o700)
    manifest = {"revision": REVISION, "archiveSha256": hashlib.sha256(b"inert image").hexdigest()}
    for name, data in (
        ("image.tar", b"inert image"),
        ("release.json", json.dumps(manifest).encode()),
        ("artifact.zip", b"inert zip"),
    ):
        write(acquired / "artifact" / name, data)
    write(
        acquired / "selection.json",
        json.dumps({"revision": REVISION, "ciRunId": 789, "artifactId": 123}).encode(),
    )
    binary = root / "inert-tool"
    write(binary, b"inert fixture bytes never executed\n" * 4096, 0o700)
    (root / "ssh-home").mkdir(mode=0o700)
    return binary, acquired


@final
class ClockRead:
    """Advance only the virtual clock after an actual chunk from the original file."""

    def __init__(
        self,
        original: BinaryIO,
        clock: list[float],
        control: keeper.Control,
        reads: list[int],
        *,
        at_eof: bool = False,
    ) -> None:
        """Borrow the existing file without opening, closing or replacing its FD."""
        self.original, self.clock, self.control, self.reads = original, clock, control, reads
        self.at_eof = at_eof

    def read(self, size: int) -> bytes:
        """Preserve actual bytes, then make the next cooperative checkpoint late."""
        data = self.original.read(size)
        if bool(data) != self.at_eof:
            self.reads.append(len(data))
            self.clock[0] = self.control.body_end
        return data

    def seek(self, offset: int) -> int:
        """Reset the original read cursor, matching the production hash operation."""
        return self.original.seek(offset)


class FetchKeeperTests(unittest.TestCase):
    """Exercise complete owned flow with inert process/VM observations and real local files."""

    def exercise(  # noqa: C901, PLR0913, PLR0915 -- One closed owned fixture.
        self,
        *,
        native_fault: bool = False,
        close_fault: bool = False,
        inode_fault: bool = False,
        late_hash: bool = False,
        qemu_status: int | bool | None = 0,
        observation: dict[str, object] | None = None,
        main_late_signal: bool = False,
    ) -> tuple[dict[str, object], list[FakeHost], bool, bool]:
        """Attempt real known-host retirement and source closes before collecting assertions."""
        with tempfile.TemporaryDirectory(prefix="fetch-keeper-own-") as name:
            root = Path(name).resolve()
            binary, acquired = prepare(root)
            hosts: list[FakeHost] = []
            clock = [100.0]
            control = keeper.Control(started=clock[0]) if late_hash else keeper.Control()
            real_time, real_digest, real_close = (
                time.monotonic,
                keeper.Held.digest,
                keeper.Held.close,
            )
            reads: list[int] = []
            closed: list[bool] = []
            restores: list[int] = []
            masks: list[int] = []
            exit_status: int | None = None
            stdout_closed = False

            def host(output: Path, control: keeper.Control) -> FakeHost:
                created = FakeHost(output, control)
                created.native_fault, created.close_fault = native_fault, close_fault
                created.inode_fault = inode_fault
                created.qemu_status = qemu_status
                hosts.append(created)
                return created

            def digest(held: keeper.Held, owner: keeper.Control) -> str:
                if (
                    late_hash
                    and not reads
                    and held.path == binary
                    and (root / "ssh-home/.ssh/known_hosts").exists()
                ):
                    original = held.stream
                    held.stream = cast(
                        "BinaryIO", cast("object", ClockRead(original, clock, owner, reads))
                    )
                    try:
                        return real_digest(held, owner)
                    finally:
                        held.stream = original
                return real_digest(held, owner)

            def close(held: keeper.Held) -> None:
                real_close(held)
                closed.append(held.stream.closed)

            def handlers(
                number: int, handler: int | Callable[[int, FrameType | None], object] | None
            ) -> int:
                if not callable(handler):
                    restores.append(number)
                    if len(restores) == 1:
                        control.signal(signal.SIGTERM, None)
                return signal.SIG_DFL

            def mask(mode: int) -> int:
                masks.append(mode)
                return 0o022 if len(masks) == 1 else 0o077

            def seed(_host: keeper.Host, work: Path, _port: int, _run_id: str) -> None:
                write(work / "client", b"inert generated identity")
                write(work / "host.pub", (KEY + "\n").encode())

            with (
                patch.dict(os.environ, CI, clear=True),
                patch.object(keeper, "ci_home", return_value=root / "ssh-home"),
                patch.object(keeper, "Host", side_effect=host),
                patch.object(keeper, "executable", return_value=str(binary)),
                patch.object(vm, "preflight"),
                patch.object(vm, "prepare_disk"),
                patch.object(vm, "create_seed", side_effect=seed),
                patch.object(vm, "Guest", FakeGuest),
                patch.object(vm, "boot"),
                patch.object(vm, "executable", return_value=str(binary)),
                patch.object(uuid, "uuid4", return_value=uuid_value()),
                patch.object(
                    time, "monotonic", side_effect=lambda: clock[0] if late_hash else real_time()
                ),
                patch.object(keeper.Held, "digest", digest),
                patch.object(keeper.Held, "close", close),
            ):
                if main_late_signal:
                    buffer = io.BytesIO()
                    stdout = io.TextIOWrapper(buffer, encoding="utf-8", write_through=True)
                    try:
                        with (
                            patch.object(keeper, "Control", return_value=control),
                            patch.object(signal, "signal", handlers),
                            patch.object(os, "umask", mask),
                            patch.object(sys, "stdout", stdout),
                        ):
                            exit_status = keeper.main(
                                [
                                    "--binary",
                                    str(binary),
                                    "--acquired",
                                    str(acquired),
                                    "--output",
                                    str(root / "output"),
                                ]
                            )
                        stdout.flush()
                        report = cast("dict[str, object]", json.loads(buffer.getvalue()))
                    finally:
                        stdout.close()
                    stdout_closed = stdout.closed and buffer.closed
                else:
                    report = keeper.run(binary, acquired, root / "output", control)
            home_closed = not (root / "ssh-home/.ssh").exists()
            work_closed = not list((root / "output").glob("fetch-vm-private-*"))
            if observation is not None:
                observation.update(
                    {
                        "reads": reads,
                        "closed": closed,
                        "remaining": control.end - clock[0],
                        "exitStatus": exit_status,
                        "restores": restores,
                        "masks": masks,
                        "stdoutClosed": stdout_closed,
                        "controlStatus": control.status,
                    }
                )
        return report, hosts, home_closed, work_closed

    def test_whole_policy_keeps_cached_then_fresh_and_closes_before_positive(self) -> None:
        """Actual local custody runs; VM/process effects are explicitly inert here."""
        report, hosts, home_closed, work_closed = self.exercise()
        self.assertTrue(report["passed"])
        self.assertTrue(report["cleanupPassed"])
        self.assertTrue(report["samePublishedInodes"])
        self.assertEqual(report["remoteOutcome"], "validated")
        self.assertTrue(home_closed and work_closed)
        calls = hosts[0].native_calls
        self.assertEqual(len(calls), 2)
        self.assertIn("--verified-directory", calls[0][0])
        self.assertNotIn("--verified-directory", calls[1][0])
        self.assertNotIn("GH_TOKEN", hosts[0].env)
        self.assertNotIn(TOKEN, json.dumps(report))
        self.assertFalse(report["authenticatedOpsDistribution"])

    def test_first_native_failure_keeps_uncertain_remote_negative_and_retires_all_peers(
        self,
    ) -> None:
        """A failing VM close cannot skip known-host, original input or work cleanup."""
        report, hosts, home_closed, work_closed = self.exercise(native_fault=True, close_fault=True)
        self.assertFalse(report["passed"])
        self.assertFalse(report["cleanupPassed"])
        self.assertTrue(report["cleanupUncertain"])
        self.assertEqual(report["failure"], "cached-local-first")
        self.assertEqual(report["remoteOutcome"], "unknown")
        self.assertTrue(home_closed and work_closed)
        self.assertEqual(len(hosts[0].native_calls), 1)
        self.assertNotIn("INERT_PRIVATE", json.dumps(report))
        self.assertEqual(report["trials"], [])

    def test_a_changed_repeat_inode_is_not_hidden_by_equal_bytes(self) -> None:
        """Retain the first successful trial and reject the second publication identity."""
        report, _hosts, home_closed, work_closed = self.exercise(inode_fault=True)
        self.assertFalse(report["passed"])
        self.assertEqual(report["failure"], "local-download-repeat")
        self.assertFalse(report["samePublishedInodes"])
        self.assertTrue(home_closed and work_closed)

    def test_body_hash_cannot_consume_reserved_vm_finalization(self) -> None:
        """A real post-boot source chunk reaches body_end and stops before another body effect."""
        observed: dict[str, object] = {}
        report, hosts, home_closed, work_closed = self.exercise(
            late_hash=True, observation=observed
        )
        self.assertFalse(report["passed"])
        self.assertEqual(report["failure"], "cached-local-first")
        self.assertEqual(report["remoteOutcome"], "not-started")
        self.assertEqual(hosts[0].native_calls, [])
        self.assertEqual(observed["reads"], [65536])
        self.assertEqual(observed["remaining"], keeper.VM_FINALIZATION)
        self.assertTrue(report["cleanupPassed"])
        self.assertTrue(home_closed and work_closed)
        self.assertTrue(observed["closed"] and all(cast("list[bool]", observed["closed"])))
        self.assertTrue(hosts[0].guest is not None and hosts[0].guest.closed)

    def test_late_qemu_failure_never_becomes_success_from_consuming_reap(self) -> None:
        """Keep both completed trial facts while a nonzero/signaled/unknown VM status refuses."""
        for status in (7, -9, True, False, None):
            with self.subTest(status=status):
                observed: dict[str, object] = {}
                report, hosts, home_closed, work_closed = self.exercise(
                    qemu_status=status, observation=observed
                )
                facts = cast("dict[str, object]", report["vmLifecycle"])
                self.assertFalse(report["passed"])
                self.assertFalse(report["cleanupPassed"])
                self.assertTrue(report["cleanupUncertain"])
                self.assertEqual(report["failure"], "cleanup")
                self.assertEqual(len(cast("list[object]", report["trials"])), 2)
                self.assertTrue(report["samePublishedInodes"])
                self.assertFalse(facts["qemuExitedZero"])
                self.assertEqual(facts["qemuReaped"], type(status) is int)
                self.assertEqual(facts["qemuReturnCode"], status if type(status) is int else None)
                self.assertTrue(home_closed and work_closed)
                self.assertTrue(observed["closed"] and all(cast("list[bool]", observed["closed"])))
                self.assertTrue(hosts[0].guest is not None and hosts[0].guest.closed)

    def test_replaced_work_directory_is_retained_and_every_original_peer_closes(self) -> None:
        """A real rename cannot authorize deleting a foreign regular directory at the old name."""
        with tempfile.TemporaryDirectory(prefix="fetch-work-owner-") as name:
            root = Path(name).resolve()
            binary, _ = prepare(root)
            output = root / "output"
            output.mkdir(mode=0o700)
            control = keeper.Control()
            owned = keeper.Work.create(output)
            descriptors = (owned.file_fd, owned.parent_fd)
            original_path = owned.path
            retained = output / "renamed-original-work"
            _ = original_path.rename(retained)
            original_path.mkdir(mode=0o700)
            marker = original_path / "foreign-marker"
            write(marker, b"foreign replacement must survive\n")
            known = KnownHost(root / "ssh-home", 22345, KEY)
            held = keeper.Held.open(binary, keeper.MAX_BINARY, control)
            host = FakeHost(output, control)
            guest = FakeGuest(host, [], qmp_path=retained / "qmp.sock")
            owners = keeper.Owners(
                guest=cast("vm.Guest", cast("object", guest)),
                known=known,
                inputs=[held],
                work=owned,
            )
            close_attempts: list[tuple[int, bool]] = []
            real_close = os.close

            def close(descriptor: int) -> None:
                real_close(descriptor)
                if descriptor in descriptors:
                    close_attempts.append((descriptor, True))

            report: dict[str, object] = {}
            control.settling = True
            with patch.object(os, "close", close):
                failure = owners.finish(control, report)
            foreign = marker.read_bytes()
            original_retained = retained.is_dir()
            known_retired = not (root / "ssh-home/.ssh").exists()
            input_closed, guest_closed = held.stream.closed, guest.closed
            work_disarmed = owned.file_fd is None and owned.parent_fd is None
        self.assertIsInstance(failure, ToolError)
        self.assertEqual(foreign, b"foreign replacement must survive\n")
        self.assertTrue(original_retained and known_retired and input_closed and guest_closed)
        self.assertTrue(work_disarmed)
        self.assertCountEqual(close_attempts, [(cast("int", fd), True) for fd in descriptors])

    def test_native_path_stays_on_pinned_original_and_rejects_changed_private_links(self) -> None:
        """A persistent ambient A-to-B switch never labels B as the recorded original A."""
        with tempfile.TemporaryDirectory(prefix="fetch-pinned-tools-") as name:
            root = Path(name).resolve()
            first, second = root / "tool-A", root / "tool-B"
            write(first, b"inert original executable A\n", 0o700)
            write(second, b"inert replacement executable B\n", 0o700)
            ambient = root / "ambient-bin"
            ambient.mkdir(mode=0o700)
            alias = ambient / "gh"
            alias.symlink_to(first)
            initial = Path(cast("str", shutil.which("gh", path=str(ambient)))).resolve()
            parent = root / "output"
            parent.mkdir(mode=0o700)
            control = keeper.Control()
            work = keeper.Work.create(parent)
            original = keeper.Held.open(initial, keeper.MAX_BINARY, control)
            failure: BaseException | None = None
            try:
                tools = keeper.NativeTools.create(
                    work, dict.fromkeys(keeper.TOOL_ROLES, original), control
                )
                alias.unlink()
                alias.symlink_to(second)
                now_ambient = Path(cast("str", shutil.which("gh", path=str(ambient)))).resolve()
                tools.check(control)
                now_native = Path(
                    cast("str", shutil.which("gh", path=str(tools.directory)))
                ).resolve()
                recorded_hash = original.sha256
                private = tools.directory / "gh"
                private.unlink()
                private.symlink_to(second)
                try:
                    tools.check(control)
                except ToolError as error:
                    failure = error
            finally:
                original.close()
                work.finish()
            unchanged_second = second.read_bytes()
            closed = original.stream.closed and work.parent_fd is None and work.file_fd is None
        self.assertEqual(initial, first)
        self.assertEqual(now_ambient, second)
        self.assertEqual(now_native, first)
        self.assertEqual(
            recorded_hash, hashlib.sha256(b"inert original executable A\n").hexdigest()
        )
        self.assertIsInstance(failure, ToolError)
        self.assertEqual(unchanged_second, b"inert replacement executable B\n")
        self.assertTrue(closed)

    def test_late_original_eof_cannot_return_a_positive_body_digest(self) -> None:
        """Observe actual EOF, then refuse its late completion before returning the hash."""
        with tempfile.TemporaryDirectory(prefix="fetch-eof-clock-") as name:
            path = Path(name).resolve() / "original"
            write(path, b"real original bytes")
            clock = [100.0]
            reads: list[int] = []
            control = keeper.Control(started=clock[0])
            with patch.object(time, "monotonic", side_effect=lambda: clock[0]):
                held = keeper.Held.open(path, 100, control)
                original = held.stream
                held.stream = cast(
                    "BinaryIO",
                    cast("object", ClockRead(original, clock, control, reads, at_eof=True)),
                )
                failure: BaseException | None = None
                result: str | None = None
                try:
                    result = held.digest(control)
                except ToolError as error:
                    failure = error
                finally:
                    held.stream = original
                    control.settling = True
                    held.close()
                closed = held.stream.closed
        self.assertIsInstance(failure, ToolError)
        self.assertIsNone(result)
        self.assertEqual(reads, [0])
        self.assertEqual(control.end - clock[0], keeper.VM_FINALIZATION)
        self.assertTrue(closed)

    def test_signal_during_actual_main_restoration_invalidates_emitted_positive_summary(
        self,
    ) -> None:
        """A known late signal cannot be ignored by the settling-mode clock check."""
        observed: dict[str, object] = {}
        report, hosts, home_closed, work_closed = self.exercise(
            main_late_signal=True, observation=observed
        )
        self.assertTrue(report["passed"])
        self.assertEqual(observed["exitStatus"], 1)
        self.assertEqual(observed["controlStatus"], 143)
        self.assertEqual(observed["restores"], [signal.SIGINT, signal.SIGTERM])
        self.assertEqual(observed["masks"], [0o077, 0o022])
        self.assertTrue(observed["stdoutClosed"])
        self.assertTrue(home_closed and work_closed)
        self.assertTrue(observed["closed"] and all(cast("list[bool]", observed["closed"])))
        self.assertTrue(hosts[0].guest is not None and hosts[0].guest.closed)

    def test_all_transport_roles_keep_pinned_tools(  # noqa: PLR0915 -- One owned role matrix.
        self,
    ) -> None:
        """Run actual boot/probe/Host boundaries with real files and inert process I/O."""
        with tempfile.TemporaryDirectory(prefix="fetch-all-tool-roles-") as name:
            root = Path(name).resolve()
            binary, acquired = prepare(root)
            replacement = root / "tool-B"
            write(replacement, b"inert replacement never executed", 0o700)
            ambient = root / "ambient-bin"
            ambient.mkdir(mode=0o700)
            for role in keeper.TOOL_ROLES:
                (ambient / role).symlink_to(binary)
            output = root / "output"
            output.mkdir(mode=0o700)
            control = keeper.Control()
            work = keeper.Work.create(output)
            owners = keeper.Owners(work=work)
            calls: list[tuple[list[str], dict[str, str]]] = []
            streams: list[BinaryIO] = []
            pins: dict[str, keeper.Held] = {}
            guest: FakeGuest | None = None
            try:
                original = keeper.Held.open(binary, keeper.MAX_BINARY, control)
                owners.inputs.append(original)
                for member in ("image.tar", "release.json", "artifact.zip"):
                    held = keeper.Held.open(acquired / "artifact" / member, 2**20, control)
                    owners.inputs.append(held)
                    pins[member] = held
                with patch.dict(os.environ, {"PATH": str(ambient)}, clear=True):
                    host = keeper.Host(output, control)
                    host.fixed_tools = dict.fromkeys(keeper.TOOL_ROLES, original)
                    guest = FakeGuest(host, [], qmp_path=work.path / "qmp.sock")
                    host.guest = cast("vm.Guest", cast("object", guest))
                    for role in keeper.TOOL_ROLES:
                        (ambient / role).unlink()
                        (ambient / role).symlink_to(replacement)

                    def run(  # noqa: PLR0913 -- Mirror the bounded process adapter.
                        arguments: Sequence[str],
                        *,
                        cwd: Path,
                        env: Mapping[str, str],
                        output: BinaryIO,
                        error: BinaryIO,
                        limits: bounded_process.Limits,
                    ) -> tuple[int, bytes, bytes]:
                        del cwd, limits
                        calls.append((list(arguments), dict(env)))
                        streams.extend((output, error))
                        data = b""
                        if "cloud-init" in arguments[-1]:
                            data = json.dumps(
                                {
                                    "status": "done",
                                    "extended_status": "done",
                                    "stage": None,
                                    "errors": [],
                                    "recoverable_errors": {},
                                    **{
                                        stage: {"errors": [], "recoverable_errors": {}}
                                        for stage in vm.CLOUD_STAGES
                                    },
                                }
                            ).encode()
                        elif "security_vm_fetch_guest.py" in arguments[-1]:
                            data = json.dumps(
                                {
                                    "schemaVersion": 1,
                                    "scope": "ReleaseFetchGuestProbe",
                                    "runId": RUN_ID,
                                    "revision": REVISION,
                                    "attempts": 1,
                                    "passed": True,
                                    "published": {
                                        member: {
                                            "sha256": pins[member].sha256,
                                            "bytes": pins[member].size,
                                            "device": 1,
                                            "inode": index,
                                        }
                                        for index, member in enumerate(
                                            ("image.tar", "release.json"), 1
                                        )
                                    },
                                }
                            ).encode()
                        _ = output.write(data)
                        return 0, b"", b""

                    with patch.object(bounded_process, "run", run):
                        _ = host.run("download-cloud", [str(replacement), "fixed-cloud-role"])
                        vm.boot(host, cast("vm.Guest", cast("object", guest)), work.path, 22345)
                        observed = keeper.observe_guest(
                            host,
                            work.path,
                            22345,
                            RUN_ID,
                            {"revision": REVISION, "artifactId": 123},
                            pins,
                            1,
                        )
                        inventory = keeper.inventory(
                            work.path, 22345, RUN_ID, REVISION, original.path
                        )
                        _ = host.run(
                            "fetch-stage", ["inert-ansible", "-i", str(inventory), "fixed-playbook"]
                        )
                        inventory_data = obj(decode_json(inventory.read_bytes()))
            finally:
                try:
                    if guest is not None:
                        guest.close()
                finally:
                    control.settling = True
                    cleanup = owners.finish(control, {})
            all_closed = all(stream.closed for stream in streams)
        self.assertIsNone(cleanup)
        self.assertTrue(all_closed)
        self.assertTrue(observed["passed"])
        self.assertTrue(guest is not None and guest.authentication_observed and guest.closed)
        self.assertEqual(len(calls), 5)
        self.assertTrue(all(argv[0] == str(binary) for argv, _ in calls[:-1]))
        self.assertEqual(calls[-1][0][0], "inert-ansible")
        self.assertEqual(calls[-1][1]["ANSIBLE_SSH_EXECUTABLE"], str(binary))
        self.assertEqual(
            string(
                inventory_data, "benchmark_hosts", "hosts", "vmfixture", "ansible_ssh_executable"
            ),
            str(binary),
        )

    def test_private_bin_refuses_visible_symlink_or_mode_change_before_links(self) -> None:
        """Do not adopt a substituted initial bin as a valid directory baseline."""
        for fault in ("symlink", "mode"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as name:
                root = Path(name).resolve()
                binary = root / "tool-A"
                write(binary, b"inert original", 0o700)
                foreign = root / "foreign"
                foreign.mkdir(mode=0o700)
                write(foreign / "marker", b"foreign must stay untouched")
                output = root / "output"
                output.mkdir(mode=0o700)
                control = keeper.Control()
                work = keeper.Work.create(output)
                original = keeper.Held.open(binary, 100, control)
                mkdir = Path.mkdir

                def changed(
                    path: Path,
                    mode: int,
                    expected: str = fault,
                    destination: Path = foreign,
                    make: Callable[..., None] = mkdir,
                ) -> None:
                    make(path, mode=mode)
                    if expected == "symlink":
                        path.rmdir()
                        path.symlink_to(destination, target_is_directory=True)
                    else:
                        path.chmod(0o755)

                failure: BaseException | None = None
                try:
                    with patch.object(Path, "mkdir", changed):
                        try:
                            _ = keeper.NativeTools.create(
                                work, dict.fromkeys(keeper.TOOL_ROLES, original), control
                            )
                        except ToolError as error:
                            failure = error
                finally:
                    original.close()
                    work.finish()
                names = [item.name for item in foreign.iterdir()]
                data = (foreign / "marker").read_bytes()
            self.assertIsInstance(failure, ToolError)
            self.assertEqual(names, ["marker"])
            self.assertEqual(data, b"foreign must stay untouched")

    def test_native_budget_uses_original_entry_and_refuses_before_no_room(self) -> None:
        """No second-call reset and no implicit default/caller increase is introduced."""
        control = keeper.Control(started=100)
        with patch.object(time, "monotonic", return_value=100):
            self.assertEqual(keeper.native_budget(control), 1080)
        with patch.object(time, "monotonic", return_value=1500):
            self.assertEqual(keeper.native_budget(control), 260)
        with patch.object(time, "monotonic", return_value=1640):
            self.assertEqual(keeper.native_budget(control), 120)
        with patch.object(time, "monotonic", return_value=1641), self.assertRaises(ToolError):
            _ = keeper.native_budget(control)
        with patch.object(time, "monotonic", return_value=1680), self.assertRaises(ToolError):
            _ = keeper.native_budget(control)

    def test_real_passwd_home_is_never_resolved_outside_dedicated_ci(self) -> None:
        """A normal developer invocation refuses before any real SSH profile action."""
        with (
            patch.object(pwd, "getpwuid", side_effect=AssertionError),
            self.assertRaises(ToolError),
        ):
            _ = keeper.ci_home({})
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            record = pwd.struct_passwd(
                ("fixture", "x", os.getuid(), os.getgid(), "", str(home), "/bin/sh")
            )
            with (
                patch.object(sys, "platform", "linux"),
                patch.object(pwd, "getpwuid", return_value=record),
            ):
                self.assertEqual(keeper.ci_home(CI), home)

    def test_original_source_change_refuses_and_still_closes_its_handle(self) -> None:
        """Check original bytes and stamp, rather than reopening a replacement baseline."""
        with tempfile.TemporaryDirectory() as name:
            path = Path(name).resolve() / "input"
            write(path, b"original")
            held = keeper.Held.open(path, 100, keeper.Control())
            _ = path.write_bytes(b"changed")
            failure: BaseException | None = None
            try:
                held.check(keeper.Control())
            except ToolError as error:
                failure = error
            finally:
                held.close()
            closed = held.stream.closed
        self.assertIsInstance(failure, ToolError)
        self.assertTrue(closed)

    def test_playbook_installs_only_fixed_unchanged_helpers_after_marker(self) -> None:
        """No app/service/provider task or imported deployment playbook enters the fixture."""
        playbook = yaml_value((ROOT / "security/vm/fetch.yml").read_text())
        play = obj(array(playbook)[0])
        self.assertEqual(play["hosts"], "benchmark_hosts")
        pre = objects(play, "pre_tasks")
        self.assertIn("vm_fixture_run_id", string(obj(pre[2], "ansible.builtin.assert"), "that", 0))
        helpers = next(
            task
            for task in objects(play, "tasks")
            if task["name"] == "Install only unchanged compatible receiver helpers"
        )
        self.assertEqual(
            helpers["loop"],
            [
                "fetch-release.py",
                "release_fetch_receiver.py",
                "release_trust.py",
                "release_artifact.py",
                "release_json.py",
            ],
        )
        text = (ROOT / "security/vm/fetch.yml").read_text()
        for forbidden in ("import_playbook", "docker", "systemd", "service:", "shell:"):
            self.assertNotIn(forbidden, text)


def uuid_value() -> object:
    """Return only the fixed run-ID observation used by the inert VM boundary."""
    return uuid.UUID(RUN_ID)


if __name__ == "__main__":
    _ = unittest.main()
