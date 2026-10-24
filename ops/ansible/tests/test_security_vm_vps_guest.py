"""Inert future-Debian fixture tests; no candidate, VM, service or workload runs."""

from __future__ import annotations

import fcntl
import hashlib
import importlib.machinery
import io
import json
import os
import py_compile
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from contextlib import ExitStack, suppress
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import security_vm_ops_guest as guest
import security_vm_vps_guest as probe
import vps_ops_bootstrap as bootstrap
from test_security_vm_ops_guest import fixture

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO, NoReturn


class ExecObservedError(RuntimeError):
    """Stop before the kernel boundary; fixture bytes are never executable."""


@final
class Pipe(io.BytesIO):
    """A real inert stream whose after-effect close fault must not skip peers."""

    def __init__(self, name: str, closed: list[str], *, fail: bool = False) -> None:
        """Keep one after-effect fault separate from actual stream closure."""
        super().__init__()
        self.name: str = name
        self.label = name
        self.trace = closed
        self.fail = fail

    @override
    def close(self) -> None:
        super().close()
        self.trace.append(self.label)
        if self.fail:
            message = "late-owned-pipe-close"
            raise OSError(message)


@dataclass
class InertChild:
    """Only subprocess construction is substituted; no PID is signalled or waited."""

    stdin: Pipe
    stdout: Pipe
    stderr: Pipe
    pid: int = 999999


@final
class ProbeTests(unittest.TestCase):
    """Use actual private copies/FDs with only UID/root and kernel-exec seams adapted."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Declare unittest's deferred owned fixture state before setup."""
        super().__init__(method_name)
        self.stack = ExitStack()
        self.temporary = ""
        self.root = ROOT
        self.artifacts = ROOT
        self.probes = ROOT
        self.receiver: guest.Receiver | None = None
        self.options = fixture()[0]
        self.control = guest.Control()

    @property
    def source_receiver(self) -> guest.Receiver:
        """Require that setup completed before a case touches original files."""
        if self.receiver is None:
            message = "fixture setup incomplete"
            raise AssertionError(message)
        return self.receiver

    @staticmethod
    def admitted_parent(owners: bootstrap.Owners) -> list[bootstrap.Directory]:
        """Adapt only the privileged ancestor boundary to one owned test directory."""
        fd = owners.add(os.open(bootstrap.ROOT, bootstrap.DIRECTORY_FLAGS))
        return [bootstrap.Directory(fd, os.fstat(fd.fd), None, str(bootstrap.ROOT))]

    @override
    def setUp(self) -> None:
        """Create inert original files whose actual hashes/counts drive every pin."""
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.temporary = self.stack.enter_context(tempfile.TemporaryDirectory())
        self.root = Path(self.temporary)
        os.chown(self.root, -1, os.getgid())
        self.artifacts = self.root / "artifacts"
        self.artifacts.mkdir(mode=0o700)
        self.probes = self.root / "probes"
        self.probes.mkdir(mode=0o700)
        _ = self.stack.enter_context(patch.object(bootstrap, "ROOT", self.artifacts))
        _ = self.stack.enter_context(patch.object(probe, "PROBES", self.probes))
        _ = self.stack.enter_context(patch.object(bootstrap, "OWNER_UID", os.getuid()))
        _ = self.stack.enter_context(patch.object(bootstrap, "OWNER_GID", os.getgid()))
        _ = self.stack.enter_context(patch.object(bootstrap, "open_parent", self.admitted_parent))
        original_receiver = guest.Receiver
        _ = self.stack.enter_context(
            patch.object(guest, "Receiver", partial(original_receiver, owner=os.getuid()))
        )
        source = self.root / "original"
        source.mkdir(mode=0o700)
        self.receiver = original_receiver(source, owner=os.getuid())
        self.addCleanup(self.receiver.close)
        self.options, original = fixture()
        self.control = guest.Control()
        with tarfile.open(fileobj=io.BytesIO(original), mode="r:") as archive:
            for item in archive:
                member = cast("BinaryIO", archive.extractfile(item))
                with member:
                    self.receiver.member(
                        item, member, self.options.pins[item.name], self.control.check
                    )

    def materialize(self, name: str) -> bytes:
        """Exercise actual staged copy and all writers' closure on inert bytes."""
        (self.probes / name).mkdir(mode=0o700)
        raw = probe.header(self.options, name)
        probe.stage(self.options, self.source_receiver, raw, self.control)
        return raw

    def test_full_original_pins_and_distinct_frame_are_retained(self) -> None:
        """CI IDs, VM run, probe run, E and transfer bytes have separate meanings."""
        for name, _, _ in probe.CASES:
            raw = probe.header(self.options, name)
            value = guest.object_json(raw)
            self.assertEqual(value["selection"], self.options.selected)
            self.assertEqual(value["artifactId"], self.options.artifact_id)
            self.assertNotEqual(value["run"], self.options.run_id)
            self.assertEqual(value["request"], probe.pin(probe.LOOKUP))
            self.assertEqual(
                value["transfer"],
                {"sha256": self.options.transfer_sha256, "bytes": self.options.transfer_bytes},
            )
            for member, expected in self.options.pins.items():
                self.assertEqual(
                    cast("dict[str, object]", value["members"])[member],
                    {"sha256": expected[0], "bytes": expected[1]},
                )
        self.assertEqual(sum(not row[0].startswith("vps-") for row in guest.CASES), 28)
        self.assertEqual(len(probe.CASES), 5)

    def test_stage_copies_original_files_without_executing_or_replacing_them(self) -> None:
        """Original readonly owners survive copied file writer closure."""
        before = {
            name: guest.stamp(os.fstat(held.file.fileno()))
            for name, held in self.source_receiver.files.items()
        }
        raw = self.materialize(probe.CASES[0][0])
        stage = self.artifacts / ("ops-runtime." + bootstrap.Header.parse(raw).run)
        self.assertEqual({p.name for p in stage.iterdir()}, set(guest.NAMES))
        for name, held in self.source_receiver.files.items():
            data = (stage / name).read_bytes()
            self.assertEqual((hashlib.sha256(data).hexdigest(), len(data)), self.options.pins[name])
            self.assertEqual(guest.stamp(os.fstat(held.file.fileno())), before[name])
            self.assertEqual(
                stat.S_IMODE((stage / name).stat().st_mode),
                0o755 if name == guest.NAMES[-1] else 0o600,
            )
            held.check(self.source_receiver.parent, self.control.check)

    def test_real_bootstrap_final_boundary_executes_only_observed_readonly_fd(self) -> None:
        """All five interpositions reach real bootstrap admission, then stop before exec."""
        for name, _, _ in probe.CASES:
            with self.subTest(name=name):
                self.observe_boundary(name)

    def observe_boundary(self, name: str) -> None:
        """Keep callbacks bound to one case and stop before kernel execution."""
        raw = self.materialize(name)
        selected = bootstrap.Header.parse(raw)
        canonical = self.artifacts / ("ops-runtime." + selected.run) / guest.NAMES[-1]
        original_inode = (canonical.stat().st_dev, canonical.stat().st_ino)
        captured: list[tuple[int, int, int]] = []
        initial = time.monotonic_ns()
        clock = [initial]
        read_gate, write_gate = os.pipe()

        def stop(fd: int, argv: Sequence[str], env: Mapping[str, str]) -> NoReturn:
            self.assertIs(type(fd), int)
            current = os.fstat(fd)
            captured.append((fd, current.st_dev, current.st_ino))
            self.assertFalse(os.get_inheritable(fd))
            self.assertEqual(fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE, os.O_RDONLY)
            self.assertEqual(os.lseek(fd, 0, os.SEEK_CUR), 0)
            self.assertEqual(
                list(argv),
                [
                    "simplestchat-ops",
                    "capacity-vps",
                    "serve",
                    "--transport-json",
                    selected.raw,
                    "--origin-monotonic-ns",
                    str(initial),
                ],
            )
            self.assertEqual(dict(env), bootstrap.ENVIRONMENT)
            self.assertEqual(
                os.read(fd, self.options.pins[guest.NAMES[-1]][1] + 1),
                b"an inert fixture, never executed\n",
            )
            self.assertEqual(
                (current.st_dev, current.st_ino) == original_inode,
                name not in ("vps-wrong-image", "vps-stage-unlinked"),
            )
            if name == "vps-origin-expired":
                self.assertGreaterEqual(clock[0], initial + probe.EXPIRED_NS)
            raise ExecObservedError

        def read_header(control: bootstrap.Control) -> bootstrap.Header:
            control.end = control.origin + bootstrap.HEADER_SECONDS * bootstrap.NANOSECOND
            return selected

        def advance(_: float) -> None:
            clock[0] += bootstrap.NANOSECOND

        try:
            with (
                patch.object(bootstrap, "ORIGIN", initial),
                patch.object(probe, "child_guard"),
                patch.object(sys, "platform", "linux"),
                patch.object(bootstrap, "read_header", read_header),
                patch.object(probe, "gate"),
                patch.object(os, "execve", stop),
                patch.object(time, "monotonic_ns", side_effect=lambda: clock[0]),
                patch.object(time, "sleep", side_effect=advance),
                patch.object(sys, "stderr", io.StringIO()),
            ):
                self.assertEqual(probe.child(name, read_gate), 1)
            self.assertEqual(len(captured), 1)
            with self.assertRaises(OSError):
                _ = os.fstat(captured[0][0])
            self.assertTrue((self.probes / name / "preexec.ready").is_dir())
            data = (self.probes / name / "preexec.json").read_bytes()
            with patch.object(time, "monotonic_ns", return_value=clock[0]):
                origin, recorded = probe.parse_trace(data, name, raw)
            self.assertEqual(origin, initial)
            self.assertEqual(recorded[:2], captured[0][1:])
            if name == "vps-stage-unlinked":
                self.assertEqual((canonical.stat().st_dev, canonical.stat().st_ino), original_inode)
                self.assertEqual(recorded[5], 0)
                self.assertFalse((self.probes / name / "unlinked-image").exists())
            elif name == "vps-stage-exchange":
                self.assertNotEqual(
                    (canonical.stat().st_dev, canonical.stat().st_ino), original_inode
                )
        finally:
            with suppress(OSError):
                os.close(read_gate)
            os.close(write_gate)

    def test_alternate_writer_closes_before_executable_mode_is_admitted(self) -> None:
        """At the real chmod boundary, the selected inode has its readonly owner."""
        name = probe.CASES[3][0]
        (self.probes / name).mkdir(mode=0o700)
        selected = bootstrap.Header.parse(probe.header(self.options, name)).members[-1]
        original = self.source_receiver.files[guest.NAMES[-1]].file
        actual_chmod = os.fchmod
        observed: list[int] = []

        def chmod(fd: int, mode: int) -> None:
            self.assertEqual(mode, 0o755)
            self.assertEqual(fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE, os.O_RDONLY)
            self.assertEqual(stat.S_IMODE(os.fstat(fd).st_mode), 0o600)
            observed.append(fd)
            actual_chmod(fd, mode)

        with patch.object(os, "fchmod", side_effect=chmod):
            fd = probe.alternate(
                original.fileno(),
                self.probes / name / "copy",
                selected,
                bootstrap.Control(time.monotonic_ns()),
            )
        try:
            self.assertEqual(observed, [fd])
            self.assertEqual(stat.S_IMODE(os.fstat(fd).st_mode), 0o755)
            self.assertFalse(os.get_inheritable(fd))
            self.assertEqual(os.read(fd, selected.count + 1), b"an inert fixture, never executed\n")
        finally:
            os.close(fd)

    def test_trace_refuses_unknown_coerced_or_misbound_data(self) -> None:
        """A marker/JSON alone never authorizes a positive observation."""
        name = probe.CASES[0][0]
        raw = probe.header(self.options, name)
        baseline: dict[str, object] = {
            "schemaVersion": 1,
            "case": name,
            "originMonotonicNs": 1,
            "headerSha256": hashlib.sha256(raw[:-1]).hexdigest(),
            "execution": [1] * probe.STAMP_FIELDS,
            "cloexec": True,
            "readonly": True,
        }
        _ = probe.parse_trace(json.dumps(baseline).encode(), name, raw)
        for field, bad in [
            ("schemaVersion", True),
            ("case", "arbitrary"),
            ("originMonotonicNs", True),
            ("originMonotonicNs", time.monotonic_ns() + probe.EXPIRED_NS),
            ("headerSha256", "0" * 64),
            ("execution", [True] * probe.STAMP_FIELDS),
            ("cloexec", False),
            ("readonly", False),
            ("unexpected", True),
        ]:
            value = baseline | {field: bad}
            with self.assertRaises(ValueError):
                _ = probe.parse_trace(json.dumps(value).encode(), name, raw)
        with self.assertRaises(ValueError):
            _ = probe.parse_trace(b" " * (probe.MAX_TRACE + 1), name, raw)

    def test_fd_observation_checks_inode_not_reused_descriptor_number(self) -> None:
        """Real owned files model procfs link targets; no candidate/process runs."""
        fds = self.root / "fds"
        fds.mkdir()
        candidate = self.root / "candidate"
        _ = candidate.write_bytes(b"inert")
        other = self.root / "other"
        _ = other.write_bytes(b"different")
        (fds / "3").symlink_to(other)
        actual_scandir = os.scandir
        image = guest.stamp(candidate.stat())

        def observed_scandir(_: object) -> object:
            return actual_scandir(fds)

        with patch.object(os, "scandir", side_effect=observed_scandir):
            self.assertTrue(probe.candidate_fds_absent(123, image))
            (fds / "64").symlink_to(candidate)
            self.assertFalse(probe.candidate_fds_absent(123, image))

    def test_fixed_child_cli_and_original_budget_admission(self) -> None:
        """Unknown selectors and insufficient original time refuse before children."""
        for arguments in [
            [],
            ["--root", str(self.root)],
            ["--child", "arbitrary", "3"],
            ["--child", probe.CASES[0][0], "-1"],
        ]:
            self.assertNotEqual(probe.main(arguments), 0)
        control = guest.Control(seconds=probe.REQUIRED_SECONDS - 1)
        with patch.object(guest, "guard"), patch.object(probe, "run_case") as run:
            with self.assertRaises(ValueError):
                _ = probe.exercise(self.options, self.source_receiver, control)
            run.assert_not_called()

    def test_construction_failure_closes_both_original_gate_descriptors(self) -> None:
        """A failed Popen cannot leave either newly created pipe owner live."""
        created: list[int] = []
        original_pipe = os.pipe

        def pipe() -> tuple[int, int]:
            pair = original_pipe()
            created.extend(pair)
            return pair

        with (
            patch.object(os, "pipe", side_effect=pipe),
            patch.object(guest, "empty_cache"),
            patch.object(subprocess, "Popen", side_effect=OSError("inert-construction-fault")),
            self.assertRaises(OSError),
        ):
            _ = probe.run_case(
                probe.CASES[0][0], probe.header(self.options, probe.CASES[0][0]), self.control
            )
        self.assertEqual(len(created), 2)
        for fd in created:
            with self.assertRaises(OSError):
                _ = os.fstat(fd)

    def test_original_deadline_and_primary_survive_stop_and_all_peer_close_faults(self) -> None:
        """Exercise the real parent callback/catch/finalizer path without a process."""
        closed: list[str] = []
        fake = InertChild(
            Pipe("stdin", closed, fail=True), Pipe("stdout", closed), Pipe("stderr", closed)
        )
        name = probe.CASES[0][0]
        raw = probe.header(self.options, name)

        def pump(
            child_process: subprocess.Popen[bytes],
            output: BinaryIO,
            error: BinaryIO,
            limits: bounded_process.Limits,
            *,
            input_data: bytes = b"",
        ) -> int:
            _ = child_process, output, error, input_data
            self.control.end = time.monotonic() - 1
            self.assertIsNotNone(limits.healthy)
            self.assertFalse(cast("Callable[[], bool]", limits.healthy)())
            self.assertEqual(self.control.status, guest.DEADLINE_STATUS)
            message = "command_lease_lost"
            raise bounded_process.ProcessError(message)

        with (
            patch.object(guest, "empty_cache"),
            patch.object(subprocess, "Popen", return_value=fake),
            patch.object(probe, "send_header"),
            patch.object(probe, "observe_exec", return_value=(1, True)),
            patch.object(bounded_process, "pump", side_effect=pump),
            patch.object(
                bounded_process, "stop", side_effect=OSError("late-stop-fault")
            ) as stopped,
            self.assertRaisesRegex(bounded_process.ProcessError, "command_lease_lost"),
        ):
            _ = probe.run_case(name, raw, self.control)
        stopped.assert_called_once()
        self.assertEqual(self.control.status, guest.DEADLINE_STATUS)
        self.assertEqual(closed, ["stdin", "stdout", "stderr"])
        self.assertTrue(fake.stdin.closed and fake.stdout.closed and fake.stderr.closed)

    def test_child_keeps_original_deadline_but_exposes_an_independent_close_failure(self) -> None:
        """Expected deadline124 cannot turn an after-effect fixture close into a pass."""
        reader, writer = os.pipe()
        original_close = bootstrap.Descriptor.close

        def close(descriptor: bootstrap.Descriptor) -> None:
            original_close(descriptor)
            message = "late-owned-gate-close"
            raise OSError(message)

        try:
            with (
                patch.object(probe, "child_guard"),
                patch.object(bootstrap, "main", return_value=124),
                patch.object(bootstrap.Descriptor, "close", autospec=True, side_effect=close),
                patch.object(os, "write", return_value=21) as written,
            ):
                self.assertEqual(probe.child(probe.CASES[-1][0], reader), 124)
            written.assert_called_once_with(2, b"vps_exec_probe_close\n")
            with self.assertRaises(OSError):
                _ = os.fstat(reader)
        finally:
            with suppress(OSError):
                os.close(reader)
            os.close(writer)

    def test_child_constructor_failure_still_closes_the_owned_inherited_gate(self) -> None:
        """Guard/flag admission cannot bypass a known child pipe's finalizer."""
        reader, writer = os.pipe()
        try:
            with (
                patch.object(probe, "child_guard", side_effect=ValueError("inert-guard-fault")),
                self.assertRaises(ValueError),
            ):
                _ = probe.child(probe.CASES[0][0], reader)
            with self.assertRaises(OSError):
                _ = os.fstat(reader)
        finally:
            with suppress(OSError):
                os.close(reader)
            os.close(writer)

    def test_import_admission_refuses_legacy_bytecode_native_and_package_siblings(self) -> None:
        """Reject actual loader-selected filenames before any first-party import."""
        directory = self.root / "imports"
        directory.mkdir(mode=0o700)
        allowed = directory / "keeper.py"
        _ = allowed.write_bytes(b"# inert trusted fixture source\n")
        allowed.chmod(0o600)
        marker = self.root / "must-not-execute"
        source = self.root / "compile-only.py"
        _ = source.write_text(
            "from pathlib import Path\nPath("
            + repr(str(marker))
            + ").write_text('executed')\nraise RuntimeError('must not import')\n"
        )
        with (
            patch.object(probe, "OWNER_UID", os.getuid()),
            patch.object(probe, "OWNER_GID", os.getgid()),
        ):
            probe.import_directory(directory, frozenset({allowed.name}), frozenset(), 0o600)
            legacy = directory / "argparse.pyc"
            _ = py_compile.compile(str(source), cfile=str(legacy), doraise=True)
            source.unlink()
            with self.assertRaises(ValueError):
                probe.import_directory(directory, frozenset({allowed.name}), frozenset(), 0o600)
            legacy.unlink()
            native = directory / ("argparse" + importlib.machinery.EXTENSION_SUFFIXES[0])
            _ = native.write_bytes(b"inert invalid native module; never loaded")
            native.chmod(0o600)
            with self.assertRaises(ValueError):
                probe.import_directory(directory, frozenset({allowed.name}), frozenset(), 0o600)
            native.unlink()
            package = directory / "argparse"
            package.mkdir(mode=0o700)
            _ = (package / "__init__.py").write_text("raise RuntimeError('must not import')\n")
            with self.assertRaises(ValueError):
                probe.import_directory(directory, frozenset({allowed.name}), frozenset(), 0o600)
        self.assertFalse(marker.exists())


if __name__ == "__main__":
    _ = unittest.main()
