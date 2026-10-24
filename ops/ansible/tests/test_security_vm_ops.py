"""Exercise the complete new keeper with tiny original files and inert VM boundaries."""

from __future__ import annotations

import hashlib
import importlib
import io
import json
import os
import py_compile
import re
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from dataclasses import dataclass, field
from functools import partial
from importlib.machinery import (
    EXTENSION_SUFFIXES,
    ExtensionFileLoader,
    PathFinder,
    SourceFileLoader,
    SourcelessFileLoader,
)
from pathlib import Path
from typing import TYPE_CHECKING, Self, cast, final, override
from unittest.mock import patch

from test_support import ROOT

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from typing import BinaryIO

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))
import bounded_process
import security_vm as vm
import security_vm_ops as keeper
import security_vm_ops_guest as guest
from security_vm_qmp import Observer
from test_security_vm_ops_guest import fixture

SOURCE_COUNT = 26
PROBE = r"""
import json
import runpy
import sys
from pathlib import Path

if 'argparse' in sys.modules:
    raise SystemExit('The import probe preloaded argparse.')
loaded = runpy.run_path(sys.argv[1], run_name='owned_projected_keeper')
root = Path(loaded['ROOT']).resolve()
origin = Path(sys.modules['argparse'].__file__).resolve()
installation = Path(sys.base_prefix).resolve()
output = Path(sys.argv[2])
output.mkdir(mode=0o700)
host = loaded['Host'](output, loaded['protocol'].Control())
print(json.dumps({
    'root': str(root),
    'argparseOrigin': str(origin),
    'installation': str(installation),
    'stdlibOriginTrusted': origin.is_relative_to(installation),
    'ansibleConfig': host.env['ANSIBLE_CONFIG'],
    'dontWriteBytecode': host.env['PYTHONDONTWRITEBYTECODE'],
    'credentialAbsent': 'GH_TOKEN' not in host.env and 'GITHUB_TOKEN' not in host.env,
    'cloudPin': str(loaded['vm'].PIN),
}, separators=(',', ':')))
"""


def source_inputs() -> list[str]:
    """Use the production source manifest for the fixture, then execute actual imports."""
    text = (ROOT / "xtask/src/ci/ops_artifact/runtime/sources.rs").read_text()
    match = re.search(r"const NAMES: &\[&str\] = &\[(.*?)\];", text, re.DOTALL)
    if match is None:
        message = "The closed source-input manifest is unavailable."
        raise AssertionError(message)
    names = re.findall(r'"([^"\n]+)"', match[1])
    if len(names) != SOURCE_COUNT or len(set(names)) != SOURCE_COUNT:
        message = "The source-input fixture contract changed."
        raise AssertionError(message)
    return names


def copy_sources(output: Path) -> list[Path]:
    """Model only the closed projection's input files; Rust owns its real creation and witnesses."""
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    names = source_inputs()
    directories = {parent for name in names for parent in Path(name).parents if parent.parts}
    for relative in sorted(directories, key=lambda path: (len(path.parts), str(path))):
        (output / relative).mkdir(mode=0o700)
        (output / relative).chmod(0o700)
    files: list[Path] = []
    for name in names:
        path = output / name
        data = (ROOT / name).read_bytes()
        if path.write_bytes(data) != len(data):
            raise AssertionError
        path.chmod(0o600)
        files.append(path)
    return files


def shadow_module(
    caller: Path,
    kind: str,
    marker: Path,
) -> tuple[Path, type[SourceFileLoader | SourcelessFileLoader | ExtensionFileLoader]]:
    """Construct loadable source/legacy bytecode or a recognized inert extension filename."""
    path = caller / "build/argparse.py"
    payload = (
        "from pathlib import Path\nPath("
        + repr(str(marker))
        + ").write_text('inert caller module executed\\n')\n"
        + "raise RuntimeError('inert caller module must not load')\n"
    )
    if kind == "source":
        _ = path.write_text(payload)
        return path, SourceFileLoader
    if kind == "bytecode":
        _ = path.write_text(payload)
        compiled = path.with_suffix(".pyc")
        _ = py_compile.compile(str(path), cfile=str(compiled), doraise=True)
        path.unlink()
        return compiled, SourcelessFileLoader
    if kind != "extension" or not EXTENSION_SUFFIXES:
        raise AssertionError
    extension = caller / ("build/argparse" + EXTENSION_SUFFIXES[0])
    _ = extension.write_bytes(b"inert invalid native extension, never loaded")
    return extension, ExtensionFileLoader


def observation(options: guest.Options) -> dict[str, object]:
    """Model a closed guest receipt; this is deliberately not a candidate execution claim."""
    return {
        "schemaVersion": 1,
        "scope": "OpsDebianGuestReceipt",
        "runId": options.run_id,
        "cases": [
            {"name": name, "exitStatus": status, "passed": True} for name, status in guest.CASES
        ],
        "install": {"firstChanged": True, "secondChanged": False},
        "passed": True,
        "failure": None,
    }


class Socket:
    """A modeled loopback port reservation; no socket/network operation is performed."""

    def __enter__(self) -> Self:
        """Enter the modeled reservation without acquiring a real socket."""
        return self

    def __exit__(self, *_: object) -> None:
        """Retire the modeled reservation."""
        return

    def bind(self, address: tuple[str, int]) -> None:
        """Accept only the fixed loopback ephemeral-port request."""
        if address != ("127.0.0.1", 0):
            raise AssertionError

    def getsockname(self) -> tuple[str, int]:
        """Return a fixed inert port; nothing is listening there."""
        return "127.0.0.1", 12345


@dataclass
class Scenario:
    """One fixed fault scenario with inert external boundaries and real input owners."""

    root: Path
    options: guest.Options
    data: bytes
    failure: str | None
    control: guest.Control
    trace: list[str] = field(default_factory=list)
    clock: float = 0

    def monotonic(self) -> float:
        """Advance only when the scenario deliberately crosses its original deadline."""
        return self.clock

    def note(self, name: str) -> Callable[..., None]:
        """Return a typed inert external action whose order remains observable."""

        def recorded(*_: object, **_kwargs: object) -> None:
            self.trace.append(name)

        return recorded


@final
class CaseHost(keeper.Host):
    """Keep the real Host environment/owners while modeling only external commands."""

    def __init__(self, output: Path, control: guest.Control, *, state: Scenario) -> None:
        """Create the real private command namespace without acquiring a subprocess."""
        super().__init__(output, control)
        self.state = state

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
        """Model stage/SSH only, consuming the actual original borrowed transfer file."""
        state = self.state
        state.trace.append(name)
        if not argv or timeout <= 0 or not accepted:
            raise AssertionError
        if name == "ops-stage":
            if state.failure == "stage":
                message = "ops_guest_unproven"
                raise guest.OpsError(message)
            return (
                0,
                b"vmfixture : ok=8 changed=4 unreachable=0 failed=0 "
                + b"skipped=0 rescued=0 ignored=0\n",
            )
        if name != "ops-cases" or input_file is None:
            raise AssertionError
        self.control.check()
        if input_file.read() != state.data:
            raise AssertionError
        row = observation(state.options)
        if state.failure == "guest":
            row = {**row, "passed": False, "failure": "cases"}
            return 1, json.dumps(row).encode()
        if state.failure == "input-after":
            with (state.root / "transfer").open("r+b") as stream:
                _ = stream.write(b"x")
        if state.failure == "cancel":
            self.control.signal(15, None)
        return 0, json.dumps(row).encode()


@final
class CaseGuest:
    """Model only the VM boundary while retaining exact lifetime and late-failure facts."""

    def __init__(
        self, host: keeper.Host, argv: Sequence[str], *, qmp_path: Path, state: Scenario
    ) -> None:
        """Accept the single inert VM command and fixed socket name."""
        if not host.output.is_dir() or argv != ["inert-no-exec"] or qmp_path.name != "qmp.sock":
            raise AssertionError
        self.state = state
        self.failure: str | None = None
        state.trace.append("created")

    def require_alive(self) -> None:
        """Record an original-guest liveness boundary without starting QEMU."""
        self.state.trace.append("alive")

    def close(self) -> None:
        """Model exact cleanup, including cancellation or elapsed time after body success."""
        state = self.state
        state.trace.append("closed")
        if state.failure == "cleanup":
            message = "ops_guest_unproven"
            raise guest.OpsError(message)
        if state.failure == "late-cancel":
            state.control.signal(15, None)
        if state.failure == "late-deadline":
            state.clock = state.control.end + 120

    def diagnostics(self) -> dict[str, object]:
        """Supply modeled closed facts; these are never represented as real VM execution."""
        return {
            "cleanupReturnCode": -15,
            "drainer": "completed",
            "authenticatedSsh": True,
            "qmp": {
                "connected": True,
                "greetingReceived": True,
                "capabilitiesNegotiated": True,
                "eofObserved": True,
                "failure": None,
            },
            "cloudInit": {"passed": True},
        }


class EntryMaskTests(unittest.TestCase):
    """Use actual Unix socket admission while the VM execution boundary stays inert."""

    def test_main_keeps_qmp_socket_private_and_restores_caller_mask_on_success_and_error(
        self,
    ) -> None:
        """The existing observer accepts the socket without relaxing its permission rule."""
        options, data = fixture()
        for fail in (False, True):
            with (
                self.subTest(fail=fail),
                tempfile.TemporaryDirectory(prefix="ops-qmp-", dir="/tmp") as directory,
            ):
                self.entry_mask_case(Path(directory).resolve(), options, data, fail=fail)

    def entry_mask_case(
        self, root: Path, options: guest.Options, data: bytes, *, fail: bool
    ) -> None:
        """Enter the actual CLI under022, including handled I/O and parser-error exits."""
        input_path = root / "input"
        _ = input_path.write_bytes(data)
        output = root / "output"
        observed: list[int] = []

        def run(
            selected: guest.Options, destination: Path, descriptor: int, control: guest.Control
        ) -> dict[str, object]:
            self.assertEqual(selected.run_id, options.run_id)
            self.assertEqual(destination, output)
            self.assertEqual(os.fstat(descriptor).st_size, len(data))
            control.check()
            destination.mkdir(mode=0o700)
            path = destination / "qmp.sock"
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(str(path))
                listener.listen(1)
                listener.settimeout(1)
                observed.append(path.stat().st_mode & 0o777)
                observer = Observer(path, control.end)
                # Only admission is exercised. The peer closes before sending
                # any QMP greeting; no emulator or guest success is inferred.
                observer.start()
                try:
                    channel = listener.accept()[0]
                    channel.close()
                finally:
                    observer.close()
                facts = observer.diagnostics()
                self.assertTrue(facts["connected"] is True)
                self.assertTrue(facts["greetingReceived"] is False)
                self.assertEqual(facts["failure"], "vm_qmp_eof_incomplete")
            if fail:
                message = "inert failure after private socket admission"
                raise OSError(message)
            return {"passed": True}

        previous_mask = os.umask(0o022)
        try:
            with (
                input_path.open("rb") as source,
                io.TextIOWrapper(io.BytesIO(), encoding="utf-8") as stdout,
                patch.object(sys, "stdin", source),
                patch.object(sys, "stdout", stdout),
                patch.object(sys, "stderr", io.StringIO()),
                patch.object(keeper, "run", side_effect=run),
            ):
                status = keeper.main(["--output", str(output), *options.argv()])
                self.assertEqual(status, 1 if fail else 0)
                restored = os.umask(0o022)
                self.assertEqual(restored, 0o022)
                self.assertEqual(observed, [0o700])
                with self.assertRaises(SystemExit):
                    _ = keeper.main(["--unknown"])
                self.assertEqual(os.umask(0o022), 0o022)
                self.assertFalse(source.closed)
        finally:
            _ = os.umask(previous_mask)


class KeeperTests(unittest.TestCase):
    """One keeper owns boot/stage/cases/cleanup and the original retained transfer."""

    def input(self, path: Path, data: bytes) -> BinaryIO:
        """Keep the original readonly0600 inode, exactly as Native's retained stdin supplies it."""
        _ = path.write_bytes(data)
        path.chmod(0o600)
        return path.open("rb")

    def test_input_whole_hash_before_parser_and_same_inode_mutation_after_transfer_refuse(
        self,
    ) -> None:
        """The original witness survives rewind, SSH cursor consumption and final checking."""
        options, data = fixture()
        with (
            tempfile.TemporaryDirectory() as directory,
            self.input(Path(directory) / "transfer", data) as source,
        ):
            held = keeper.Input(source.fileno(), options)
            try:
                held.admit(guest.Control())
                self.assertEqual(source.tell(), 0)
                _ = held.file.read(12)
                held.check(guest.Control())
                self.assertEqual(source.tell(), 0)
                with Path(source.name).open("r+b") as mutation:
                    _ = mutation.write(b"x")
                with self.assertRaises(guest.OpsError):
                    held.check(guest.Control())
            finally:
                held.close()
            self.assertFalse(source.closed)

    def test_input_rejects_writable_and_nonzero_kernel_or_logical_cursor(self) -> None:
        """A readonly Python wrapper does not conceal a writableFD or buffered cursor mismatch."""
        options, data = fixture()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "transfer"
            with self.input(path, data) as source:
                _ = source.read(1)
                with self.assertRaises(guest.OpsError):
                    _ = keeper.Input(source.fileno(), options)
            with path.open("r+b") as source, self.assertRaises(guest.OpsError):
                _ = keeper.Input(source.fileno(), options)
            with path.open("rb") as source:
                _ = source.peek(1)
                with self.assertRaises(guest.OpsError):
                    _ = keeper.Input(source.fileno(), options)

    def test_complete_keeper_checks_original_pins_then_retires_exact_guest_before_return(
        self,
    ) -> None:
        """Only inert external boot/transport/VM boundaries replace the real controller."""
        options, data = fixture()
        for failure in (
            None,
            "stage",
            "guest",
            "cleanup",
            "input-after",
            "cancel",
            "late-cancel",
            "late-deadline",
        ):
            with self.subTest(failure=failure):
                self.keeper_case(options, data, failure)

    def keeper_case(self, options: guest.Options, data: bytes, failure: str | None) -> None:
        """Keep each scenario in its own scope so no later fault can alter captured inputs."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            output = root / "output"
            control = guest.Control(1800)
            state = Scenario(root, options, data, failure, control, clock=control.started)
            with (
                self.input(root / "transfer", data) as source,
                patch.object(vm, "preflight", side_effect=state.note("preflight")),
                patch.object(vm, "prepare_disk", side_effect=state.note("disk")),
                patch.object(vm, "create_seed", side_effect=state.note("seed")),
                patch.object(vm, "qemu_command", return_value=["inert-no-exec"]),
                patch.object(vm, "boot", side_effect=state.note("boot")),
                patch.object(vm, "Guest", partial(CaseGuest, state=state)),
                patch.object(keeper, "Host", partial(CaseHost, state=state)),
                patch.object(socket, "socket", return_value=Socket()),
                patch.object(keeper, "executable", return_value="/usr/bin/inert-no-exec"),
                patch.object(time, "monotonic", side_effect=state.monotonic),
            ):
                report = keeper.run(options, output, source.fileno(), control)
                self.assertFalse(source.closed)
            self.assertEqual(report["passed"], failure is None)
            self.assertEqual(state.trace.count("created"), 1)
            self.assertEqual(state.trace.count("closed"), 1)
            self.assertLess(state.trace.index("boot"), state.trace.index("ops-stage"))
            self.assertEqual(state.trace[-1], "closed")
            self.assertFalse(any(output.glob("ops-vm-private-*")))
            if failure is None:
                self.assertTrue(report["cleanupPassed"])
                self.assertEqual(report["cases"], observation(options)["cases"])
                self.assertEqual(len(cast("list[object]", report["cases"])), 28)
                self.assertEqual(report["install"], {"firstChanged": True, "secondChanged": False})
            if failure == "input-after":
                self.assertFalse(report["cleanupPassed"])
            if failure in ("cancel", "late-cancel"):
                self.assertEqual(control.status, 143)
                self.assertEqual(report["failure"], "cancelled")
            if failure == "late-deadline":
                self.assertEqual(control.status, 124)
                self.assertEqual(report["failure"], "deadline")

    def test_receipt_rejects_missing_reordered_extra_and_wrong_install_rows(self) -> None:
        """A successful guest status cannot promote an incomplete or uncorrelated report."""
        options, _ = fixture()
        row = observation(options)
        self.assertEqual(keeper.guest_observation(json.dumps(row).encode(), options, 0), row)
        rows = cast("list[object]", row["cases"])
        for value in (
            {**row, "cases": rows[:-1]},
            {**row, "cases": list(reversed(rows))},
            {**row, "private": "secret"},
            {**row, "runId": "2" * 32},
            {**row, "install": {"firstChanged": True, "secondChanged": True}},
        ):
            with self.assertRaises(guest.OpsError):
                _ = keeper.guest_observation(json.dumps(value).encode(), options, 0)

    def test_qemu_return_without_original_stream_and_qmp_eof_is_never_positive(self) -> None:
        """Forced pipe shutdown or missing handshake facts cannot be replaced by a close return."""
        positive = {
            "qemuReaped": True,
            "qemuReturnCode": -15,
            "authenticatedSsh": True,
            "drainerCompleted": True,
            "qmpConnected": True,
            "qmpGreetingReceived": True,
            "qmpCapabilitiesNegotiated": True,
            "qmpEofObserved": True,
            "qmpPassed": True,
            "cloudInitPassed": True,
            "closeReturned": True,
        }
        self.assertTrue(keeper.positive_lifecycle(positive))
        for key in positive:
            with self.subTest(key=key):
                self.assertFalse(
                    keeper.positive_lifecycle(
                        {**positive, key: None if key == "qemuReturnCode" else False}
                    )
                )

    def test_controller_admission_expiry_latches_deadline_and_closes_every_capture(self) -> None:
        """A late second clock sample stays 124; previously known failures retain precedence."""
        for earlier in (0, 1, 143):
            with self.subTest(earlier=earlier), tempfile.TemporaryDirectory() as directory:
                self.admission_case(Path(directory), earlier)

    def admission_case(self, root: Path, earlier: int) -> None:
        """Use real capture owners while forbidding every process dispatch."""
        output = root / "output"
        output.mkdir(mode=0o700)
        control = guest.Control(20)
        control.status = earlier
        host = keeper.Host(output, control)
        original_fdopen = os.fdopen
        captures: list[BinaryIO] = []

        def opened(descriptor: int, mode: str) -> BinaryIO:
            if mode != "w+b":
                raise AssertionError
            source = original_fdopen(descriptor, mode)
            captures.append(source)
            return source

        path = root / "borrowed-input"
        _ = path.write_bytes(b"inert original input")
        with path.open("rb") as source:
            with (
                patch.object(os, "fdopen", side_effect=opened),
                patch.object(
                    time,
                    "monotonic",
                    side_effect=(
                        control.started,
                        control.end - 0.001,
                        control.end,
                    ),
                ),
                patch.object(bounded_process, "run", side_effect=AssertionError) as dispatch,
                self.assertRaises(guest.OpsError),
            ):
                _ = host.run("admission", ["inert-no-exec"], input_file=source)
            dispatch.assert_not_called()
            self.assertFalse(source.closed)
        self.assertEqual(control.status, earlier or guest.DEADLINE_STATUS)
        self.assertEqual(len(captures), 0 if earlier else 2)
        self.assertTrue(all(stream.closed for stream in captures))
        control.signal(15, None)
        self.assertEqual(control.status, earlier or guest.DEADLINE_STATUS)


class SourceProjectionTests(unittest.TestCase):
    """Execute the real keeper imports from a closed fixture, without a VM or candidate."""

    def test_projected_keeper_excludes_caller_source_bytecode_and_native_extension_siblings(
        self,
    ) -> None:
        """Check actual loading and derived paths, rather than only source lists or argv."""
        for kind in ("source", "bytecode", "extension"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                self.projected_case(Path(directory).resolve(), kind)

    def projected_case(self, root: Path, kind: str) -> None:
        """Keep extras in the modeled caller while the actual keeper loads only its projection."""
        caller, projected = root / "caller", root / "source"
        _ = copy_sources(caller)
        selected = copy_sources(projected)
        originals = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in selected}
        marker = root / "caller-module-executed"
        shadow, loader = shadow_module(caller, kind, marker)
        importlib.invalidate_caches()
        spec = PathFinder.find_spec("argparse", [str(caller / "build")])
        self.assertIsNotNone(spec)
        if spec is None:
            raise AssertionError
        self.assertEqual(spec.origin, str(shadow))
        self.assertIsInstance(spec.loader, loader)
        environment = self.projection_environment(root)
        script = projected / "build/security_vm_ops.py"
        cache = root / "cache"
        cache.mkdir(mode=0o700)
        prefix = [sys.executable, "-I", "-B", "-X", "pycache_prefix=" + str(cache)]
        actual = self.invoke(projected, environment, [*prefix, str(script), "--help"])
        self.assertEqual(
            (actual.returncode, actual.stdout, actual.stderr),
            (
                2,
                b"",
                b"Ops guest arguments are invalid.\n",
            ),
        )
        probe = self.invoke(
            projected,
            environment,
            [*prefix, "-c", PROBE, str(script), str(root / "probe-output")],
        )
        self.assertEqual((probe.returncode, probe.stderr), (0, b""))
        self.probe_observation(probe.stdout, projected, caller)
        self.assertFalse(marker.exists())
        self.assertFalse(any(cache.iterdir()))
        self.assertEqual(
            {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in selected},
            originals,
        )
        for name in sorted(guest.BASE | guest.FETCH | guest.FORCE):
            self.assertTrue((projected / "ops/ansible/files" / name).is_file())

    def projection_environment(self, root: Path) -> dict[str, str]:
        """Use an explicit credential-free environment and separate mutable directories."""
        for name in ("home", "tmp"):
            (root / name).mkdir(mode=0o700)
        return {
            "PATH": str(Path(sys.executable).parent) + os.pathsep + os.defpath,
            "HOME": str(root / "home"),
            "TMPDIR": str(root / "tmp"),
            "LANG": "C",
            "LC_ALL": "C",
            "PYTHONDONTWRITEBYTECODE": "1",
        }

    def invoke(
        self,
        projected: Path,
        environment: dict[str, str],
        argv: Sequence[str],
    ) -> subprocess.CompletedProcess[bytes]:
        """Bound the actual interpreter and fixed import-only probe; launch no VM or candidate."""
        completed = subprocess.run(  # noqa: S603 -- Trusted interpreter, owned source projection and fixed inert import probe.
            argv,
            cwd=projected,
            env=environment,
            capture_output=True,
            timeout=10,
            check=False,
        )
        self.assertLessEqual(len(completed.stdout) + len(completed.stderr), 16384)
        return completed

    def probe_observation(self, raw: bytes, projected: Path, caller: Path) -> None:
        """Require the actual argparse origin and projected paths to stay in their domains."""
        value = guest.exact(
            guest.object_json(raw),
            {
                "root",
                "argparseOrigin",
                "installation",
                "stdlibOriginTrusted",
                "ansibleConfig",
                "dontWriteBytecode",
                "credentialAbsent",
                "cloudPin",
            },
        )
        self.assertEqual(value["root"], str(projected))
        self.assertTrue(value["stdlibOriginTrusted"] is True)
        self.assertEqual(value["ansibleConfig"], str(projected / "ops/ansible/ansible.cfg"))
        self.assertEqual(value["dontWriteBytecode"], "1")
        self.assertTrue(value["credentialAbsent"] is True)
        self.assertEqual(value["cloudPin"], str(projected / "security/vm/debian-cloud.json"))
        origin, installation = value["argparseOrigin"], value["installation"]
        self.assertIsInstance(origin, str)
        self.assertIsInstance(installation, str)
        if not isinstance(origin, str) or not isinstance(installation, str):
            message = "The import probe did not return path strings."
            raise TypeError(message)
        self.assertTrue(Path(origin).is_relative_to(Path(installation)))
        self.assertFalse(Path(origin).is_relative_to(projected))
        self.assertFalse(Path(origin).is_relative_to(caller))


if __name__ == "__main__":
    _ = unittest.main()
