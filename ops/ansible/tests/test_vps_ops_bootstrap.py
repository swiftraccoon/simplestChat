"""Exercise the real receiver on inert owned files; never execute an Ops candidate."""

from __future__ import annotations

import fcntl
import hashlib
import importlib.machinery
import io
import json
import os
import py_compile
import select
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import vps_ops_bootstrap as bootstrap

if TYPE_CHECKING:
    from collections.abc import Generator, Sequence

RUN = "b" * 32
BODIES = (b'{"manifest":"inert"}', b'{"selection":"inert"}', b"INERT-NOT-EXECUTED")
OPERATION = b'{"schemaVersion":1,"scope":"OpsCapacityOperation","action":"lookup"}'
RELEASE = b"release\n"
STDERR = 2


def archive_bytes(bodies: Sequence[bytes] = BODIES) -> bytes:
    """Use the mature writer to construct the same fixed USTAR metadata profile."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:", format=tarfile.USTAR_FORMAT) as archive:
        for name, body in zip(bootstrap.NAMES, bodies, strict=True):
            item = tarfile.TarInfo(name)
            item.mode = bootstrap.MODE_PRIVATE
            item.size = len(body)
            archive.addfile(item, io.BytesIO(body))
    return output.getvalue()


def pin(data: bytes) -> dict[str, object]:
    """Bind actual fixture bytes instead of a fabricated successful receiver result."""
    return {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}


def header_value(mode: str = "stage", archive: bytes | None = None) -> dict[str, object]:
    """Keep CI identity, outer run and member pins distinct in fixed fixture data."""
    return {
        "schemaVersion": 1,
        "scope": "OpsCapacityBootstrap",
        "mode": mode,
        "run": RUN,
        "selection": {
            "repository": "swiftraccoon/simplestChat",
            "revision": "a" * 40,
            "packageVersion": "0.1.0",
            "protocolVersion": 1,
            "target": "x86_64-unknown-linux-musl",
            "buildRunId": 44001,
            "runAttempt": 2,
        },
        "artifactId": 99001,
        "transfer": pin(archive_bytes() if archive is None else archive),
        "members": {name: pin(body) for name, body in zip(bootstrap.NAMES, BODIES, strict=True)},
        "request": pin(OPERATION),
    }


def encoded(value: dict[str, object]) -> bytes:
    """Supply one compact header LF while leaving operation bytes independent."""
    return json.dumps(value, separators=(",", ":")).encode() + b"\n"


class ExecObservedError(Exception):
    """Stop at the actual exec boundary without running fixture bytes."""


class HeaderTests(unittest.TestCase):
    """Probe closed framing without trusting input-provided authentication claims."""

    def test_boundaries_duplicates_and_numeric_types_are_real_parser_refusals(self) -> None:
        """A valid standard JSON document still needs all closed field contracts."""
        raw = encoded(header_value())
        padded = raw[:-1] + b" " * (bootstrap.MAX_HEADER - len(raw)) + b"\n"
        self.assertEqual(len(padded), bootstrap.MAX_HEADER)
        self.assertEqual(bootstrap.Header.parse(padded).run, RUN)
        invalid = [
            padded[:-1] + b" \n",
            raw.replace(b'"schemaVersion":1', b'"schemaVersion":true'),
            raw.replace(b'"schemaVersion":1', b'"schemaVersion":1.0'),
            raw.replace(b'"schemaVersion":1', b'"schemaVersion":1e0'),
            raw.replace(b'"schemaVersion":1', b'"schemaVersion":1,"schemaVersion":1'),
            raw.replace(b'"bytes":', b'"unexpected":0,"bytes":', 1),
            raw.replace(b'"artifactId":99001', b'"artifactId":9007199254740992'),
            raw.replace(b'"runAttempt":2', b'"runAttempt":0'),
            raw.replace(b'"0.1.0"', b'"00.1.0"'),
            raw.replace(b'"0.1.0"', b'"4294967296.1.0"'),
            raw.replace(b'"sha256":"', b'"sha256":"Z', 1),
            b"\xef\xbb\xbf" + raw,
            raw[:-1] + b"\r\n",
            raw.replace(b"stage", b"\xff"),
        ]
        for value in invalid:
            with self.subTest(value=value[:30]), self.assertRaises((ValueError, UnicodeError)):
                _ = bootstrap.Header.parse(value)

    def test_real_header_reads_leave_archive_and_private_request_untouched(self) -> None:
        """No BufferedReader may consume the subsequent tar/request/release frame."""
        raw = encoded(header_value())
        source = io.BytesIO(raw + archive_bytes() + OPERATION + RELEASE)
        actual_read = os.read

        def read(fd: int, count: int) -> bytes:
            return source.read(count) if fd == 0 else actual_read(fd, count)

        control = bootstrap.Control(time.monotonic_ns())
        with (
            patch.object(os, "read", side_effect=read),
            patch.object(select, "select", return_value=([0], [], [])),
        ):
            header = bootstrap.read_header(control)
        self.assertEqual(header.run, RUN)
        self.assertEqual(source.tell(), len(raw))
        self.assertEqual(source.read(), archive_bytes() + OPERATION + RELEASE)

    def test_quiet_input_deadline_and_signal_are_latched_before_read(self) -> None:
        """An empty readiness result cannot reset the original clock or status."""
        origin = 1
        control = bootstrap.Control(origin)
        end = origin + bootstrap.HEADER_SECONDS * bootstrap.NANOSECOND
        with (
            patch.object(time, "monotonic_ns", side_effect=[origin, origin, end]),
            patch.object(select, "select", return_value=([], [], [])),
            patch.object(os, "read") as read,
            self.assertRaises(bootstrap.BootstrapError) as raised,
        ):
            _ = bootstrap.read_input(1, control, end)
        self.assertEqual(raised.exception.status, 124)
        read.assert_not_called()
        control.signal(signal.SIGTERM, None)
        self.assertIs(control.failure, raised.exception)
        control = bootstrap.Control(time.monotonic_ns())

        def ready(*_: object) -> tuple[list[int], list[int], list[int]]:
            control.signal(signal.SIGINT, None)
            return [0], [], []

        with (
            patch.object(select, "select", side_effect=ready),
            patch.object(os, "read") as read,
            self.assertRaises(bootstrap.BootstrapError) as raised,
        ):
            _ = bootstrap.read_input(1, control)
        self.assertEqual(raised.exception.status, 130)
        read.assert_not_called()


class ReceiverTests(unittest.TestCase):
    """Retain actual descriptors/modes while replacing only privileged fixture roots and exec."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Declare owned fixture state before unittest setup."""
        super().__init__(method_name)
        self.parent: Path = ROOT
        self.stage: Path = ROOT
        self.stack: ExitStack = ExitStack()

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.parent = Path(temporary.name).resolve()
        self.parent.chmod(0o700)
        self.stage = self.parent / ("ops-runtime." + RUN)
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        _ = self.stack.enter_context(patch.object(bootstrap, "OWNER_UID", os.getuid()))
        _ = self.stack.enter_context(
            patch.object(bootstrap, "OWNER_GID", self.parent.stat().st_gid)
        )
        _ = self.stack.enter_context(
            patch.object(bootstrap, "open_parent", side_effect=self.parents)
        )

    def parents(self, owners: bootstrap.Owners) -> list[bootstrap.Directory]:
        """Supply an owned fixture parent; production has no selectable root flag."""
        descriptor = owners.add(os.open(self.parent, bootstrap.DIRECTORY_FLAGS))
        return [bootstrap.Directory(descriptor, os.fstat(descriptor.fd), None, str(self.parent))]

    @contextmanager
    def input(self, data: bytes) -> Generator[io.BytesIO]:
        """Use actual regular-file reads elsewhere, substituting only the borrowed stdin."""
        source = io.BytesIO(data)
        actual_read = os.read

        def read(fd: int, count: int) -> bytes:
            return source.read(count) if fd == 0 else actual_read(fd, count)

        def ready(
            readers: list[int], writers: list[int], errors: list[int], _timeout: float
        ) -> tuple[list[int], list[int], list[int]]:
            return readers, writers, errors

        with (
            patch.object(os, "read", side_effect=read),
            patch.object(select, "select", side_effect=ready),
        ):
            yield source

    def stage_once(self) -> None:
        """Reach and inspect the real fd-exec boundary without executing anything."""
        header = bootstrap.Header.parse(encoded(header_value()))
        owners = bootstrap.Owners()
        self.addCleanup(owners.close)

        def execute(fd: int, argv: list[str], env: dict[str, str]) -> None:
            metadata = os.fstat(fd)
            self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o755)
            self.assertEqual(metadata.st_ino, (self.stage / bootstrap.NAMES[-1]).stat().st_ino)
            self.assertEqual(fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE, os.O_RDONLY)
            self.assertFalse(os.get_inheritable(fd))
            self.assertEqual(os.lseek(fd, 0, os.SEEK_CUR), 0)
            self.assertEqual([item.fd for item in owners.descriptors if item.fd >= 0], [fd])
            self.assertEqual(
                argv,
                [
                    "simplestchat-ops",
                    "capacity-vps",
                    "serve",
                    "--transport-json",
                    header.raw,
                    "--origin-monotonic-ns",
                    str(control.origin),
                ],
            )
            self.assertEqual(env, bootstrap.ENVIRONMENT)
            raise ExecObservedError

        control = bootstrap.Control(time.monotonic_ns())
        with (
            self.input(archive_bytes() + OPERATION + RELEASE) as source,
            patch.object(os, "execve", side_effect=execute),
            self.assertRaises(ExecObservedError),
        ):
            bootstrap.run(header, control, owners)
        self.assertEqual(source.read(), OPERATION + RELEASE)
        self.assertIsNone(owners.close())
        for name, body in zip(bootstrap.NAMES, BODIES, strict=True):
            self.assertEqual((self.stage / name).read_bytes(), body)
        self.assertEqual(set(self.parent.iterdir()), {self.stage})

    def test_stage_preserves_original_custody_and_frame_cursor(self) -> None:
        """All writers/spool/parent peers close before the original read-only fd-exec."""
        self.stage_once()

    def test_invoke_rehashes_existing_stage_without_reading_archive_or_request(self) -> None:
        """Subsequent RPCs reuse exact files and never resend candidate bytes."""
        self.stage_once()
        owners = bootstrap.Owners()
        self.addCleanup(owners.close)
        with (
            patch.object(bootstrap, "read_input") as read,
            patch.object(os, "execve", side_effect=ExecObservedError),
            self.assertRaises(ExecObservedError),
        ):
            bootstrap.run(
                bootstrap.Header.parse(encoded(header_value("invoke"))),
                bootstrap.Control(time.monotonic_ns()),
                owners,
            )
        read.assert_not_called()
        self.assertIsNone(owners.close())

    def test_existing_stage_is_never_overwritten(self) -> None:
        """Even an exact existing stage requires invoke rather than stage replacement."""
        self.stage_once()
        original = (self.stage / bootstrap.NAMES[-1]).read_bytes()
        owners = bootstrap.Owners()
        self.addCleanup(owners.close)
        with patch.object(os, "execve") as execute, self.assertRaises(FileExistsError):
            bootstrap.run(
                bootstrap.Header.parse(encoded(header_value())),
                bootstrap.Control(time.monotonic_ns()),
                owners,
            )
        execute.assert_not_called()
        self.assertEqual((self.stage / bootstrap.NAMES[-1]).read_bytes(), original)

    def test_deadline_during_final_file_check_refuses_before_exec(self) -> None:
        """The last filesystem check cannot create a fresh unchecked exec window."""
        origin = time.monotonic_ns()
        control = bootstrap.Control(origin)
        owners = bootstrap.Owners()
        self.addCleanup(owners.close)
        clock = [origin]
        late_checks: list[int] = []
        actual_fstat = os.fstat

        def fstat(fd: int) -> os.stat_result:
            value = actual_fstat(fd)
            if len(owners.descriptors) > len(bootstrap.NAMES) and [
                item.fd for item in owners.descriptors if item.fd >= 0
            ] == [fd]:
                late_checks.append(fd)
                clock[0] = control.end
            return value

        with (
            self.input(archive_bytes() + OPERATION),
            patch.object(time, "monotonic_ns", side_effect=lambda: clock[0]),
            patch.object(os, "fstat", side_effect=fstat),
            patch.object(os, "execve") as execute,
            self.assertRaises(bootstrap.BootstrapError) as raised,
        ):
            bootstrap.run(bootstrap.Header.parse(encoded(header_value())), control, owners)
        self.assertEqual(raised.exception.status, 124)
        self.assertEqual(len(late_checks), 1)
        execute.assert_not_called()
        self.assertIsNone(owners.close())

    def test_invoke_rejects_extra_entries_and_changed_original_bytes(self) -> None:
        """A selected digest does not excuse namespace or same-inode content changes."""
        self.stage_once()
        extra = self.stage / "argparse.pyc"
        _ = extra.write_bytes(b"ignored workspace injection")
        for mutate in (False, True):
            if mutate:
                extra.unlink()
                _ = (self.stage / bootstrap.NAMES[-1]).write_bytes(b"changed")
            owners = bootstrap.Owners()
            self.addCleanup(owners.close)
            with (
                patch.object(os, "execve") as execute,
                self.assertRaises(bootstrap.BootstrapError),
            ):
                bootstrap.run(
                    bootstrap.Header.parse(encoded(header_value("invoke"))),
                    bootstrap.Control(time.monotonic_ns()),
                    owners,
                )
            execute.assert_not_called()
            self.assertIsNone(owners.close())

    def test_physical_tar_metadata_and_padding_refuse_before_mature_parser(self) -> None:
        """Unexpected metadata cannot allocate extended bodies or reach extraction."""
        invalid = bytearray(archive_bytes())
        invalid[-1] = 1
        header = bootstrap.Header.parse(encoded(header_value(archive=bytes(invalid))))
        owners = bootstrap.Owners()
        self.addCleanup(owners.close)
        with (
            self.input(bytes(invalid)),
            patch.object(tarfile, "open") as mature,
            self.assertRaises(bootstrap.BootstrapError),
        ):
            bootstrap.run(header, bootstrap.Control(time.monotonic_ns()), owners)
        mature.assert_not_called()
        self.assertIsNone(owners.close())
        self.assertEqual(list(self.stage.iterdir()), [])

    def test_body_failure_survives_after_effect_close_and_all_peers_close(self) -> None:
        """A failed original transfer remains primary after a real close then OSError."""
        value = header_value()
        transfer = cast("dict[str, object]", value["transfer"])
        transfer["sha256"] = "0" * 64
        raw = encoded(value)
        actual_close, actual_write = os.close, os.write
        attempted: list[int] = []
        diagnostics = bytearray()

        def close(fd: int) -> None:
            actual_close(fd)
            attempted.append(fd)
            if len(attempted) == 1:
                error = OSError("PRIVATE_CLOSE_TEXT")
                raise error

        def write(fd: int, data: bytes) -> int:
            if fd == STDERR:
                diagnostics.extend(data)
                return len(data)
            self.assertNotEqual(fd, 1)
            return actual_write(fd, data)

        with (
            self.input(raw + archive_bytes()),
            patch.object(os, "close", side_effect=close),
            patch.object(os, "write", side_effect=write),
            patch.object(sys, "platform", "linux"),
            patch.object(os, "geteuid", return_value=os.getuid()),
            patch.object(bootstrap, "ORIGIN", time.monotonic_ns()),
        ):
            status = bootstrap.main([])
        self.assertEqual(status, 1)
        self.assertEqual(diagnostics, b"ops_capacity_identity\n")
        self.assertEqual(len(attempted), 3)
        for fd in attempted:
            with self.assertRaises(OSError):
                _ = os.fstat(fd)

    def test_member_body_error_precedes_late_signal_and_close_only_stays_negative(self) -> None:
        """Use actual extraction to distinguish body, late signal and library-close faults."""
        archive = archive_bytes()
        actual_close = tarfile.ExFileObject.close
        actual_write = os.write
        original_control = bootstrap.Control
        for wrong_pin in (True, False):
            value = header_value()
            value["run"] = RUN if wrong_pin else "c" * 32
            if wrong_pin:
                members = cast("dict[str, object]", value["members"])
                cast("dict[str, object]", members[bootstrap.NAMES[0]])["sha256"] = "0" * 64
            controls: list[bootstrap.Control] = []
            closed: set[int] = set()
            diagnostics = bytearray()

            def control(
                origin: int, *, observed: list[bootstrap.Control] = controls
            ) -> bootstrap.Control:
                instance = original_control(origin)
                observed.append(instance)
                return instance

            def close(
                source: tarfile.ExFileObject,
                *,
                already: set[int] = closed,
                observed: list[bootstrap.Control] = controls,
                should_signal: bool = wrong_pin,
            ) -> None:
                actual_close(source)
                if id(source) not in already:
                    already.add(id(source))
                    if should_signal:
                        observed[0].signal(signal.SIGTERM, None)
                    error = OSError("PRIVATE_LIBRARY_CLOSE_TEXT")
                    raise error

            def write(fd: int, data: bytes, *, diagnostic: bytearray = diagnostics) -> int:
                if fd == STDERR:
                    diagnostic.extend(data)
                    return len(data)
                return actual_write(fd, data)

            with (
                self.subTest(wrong_pin=wrong_pin),
                self.input(encoded(value) + archive + OPERATION),
                patch.object(bootstrap, "Control", side_effect=control),
                patch.object(tarfile.ExFileObject, "close", new=close),
                patch.object(os, "write", side_effect=write),
                patch.object(sys, "platform", "linux"),
                patch.object(os, "geteuid", return_value=os.getuid()),
                patch.object(bootstrap, "ORIGIN", time.monotonic_ns()),
            ):
                status = bootstrap.main([])
            self.assertEqual(status, 1)
            self.assertEqual(
                diagnostics, b"ops_capacity_identity\n" if wrong_pin else b"ops_capacity_close\n"
            )
            self.assertTrue(closed)


class IsolationTests(unittest.TestCase):
    """Execute only trusted argument-refusal bootstrap code under production isolation flags."""

    def test_caller_python_pyc_and_extension_siblings_never_import(self) -> None:
        """The -c receiver never inserts a checkout directory on sys.path."""
        body = (ROOT / "build/vps_ops_bootstrap.py").read_text()
        for kind in ("source", "bytecode", "extension"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                marker = root / "marker"
                source = root / "json.py"
                _ = source.write_text(f"open({str(marker)!r}, 'w').close()\nraise RuntimeError\n")
                if kind == "bytecode":
                    _ = py_compile.compile(str(source), cfile=str(root / "json.pyc"), doraise=True)
                    source.unlink()
                elif kind == "extension":
                    source.unlink()
                    _ = (root / ("json" + importlib.machinery.EXTENSION_SUFFIXES[0])).write_bytes(
                        b"invalid inert extension; never loaded"
                    )
                spec = importlib.machinery.PathFinder.find_spec("json", [str(root)])
                self.assertIsNotNone(spec)
                result = subprocess.run(  # noqa: S603 -- Trusted -c source, invalid arguments only.
                    [sys.executable, "-I", "-B", "-c", body, "--unexpected"],
                    cwd=root,
                    env={"PATH": "/usr/bin:/bin", "HOME": str(root), "LC_ALL": "C"},
                    capture_output=True,
                    check=False,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertEqual(result.stderr, b"ops_capacity_arguments\n")
                self.assertFalse(marker.exists())


if __name__ == "__main__":
    _ = unittest.main()
