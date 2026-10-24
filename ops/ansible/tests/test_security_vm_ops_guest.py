"""Check the Ops guest's original-byte boundaries with inert owned files only."""

from __future__ import annotations

import hashlib
import io
import json
import os
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from contextlib import suppress
from dataclasses import dataclass, field, replace
from functools import partial
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast, final, override
from unittest.mock import Mock, patch

from test_support import ROOT

if TYPE_CHECKING:
    from typing import BinaryIO

sys.path.insert(0, str(ROOT / "build"))
import bounded_process
import security_vm_ops_guest as guest

RUN = "1" * 32
REVISION = "2" * 40


def fixture(*, extra: str | None = None, mode: int = 0o600) -> tuple[guest.Options, bytes]:
    """Construct a tiny authenticated transfer, not an executable or acquired artifact."""
    selected: dict[str, object] = {
        "repository": "swiftraccoon/simplestChat",
        "revision": REVISION,
        "packageVersion": "0.1.0",
        "protocolVersion": 1,
        "target": "x86_64-unknown-linux-musl",
        "buildRunId": 123,
        "runAttempt": 1,
    }
    selection = json.dumps(selected, separators=(",", ":")).encode()
    binary = b"an inert fixture, never executed\n"
    manifest = json.dumps(
        {
            "schemaVersion": 1,
            "artifactType": "simplestchat-ops",
            **selected,
            "binary": {
                "name": "simplestchat-ops",
                "sha256": hashlib.sha256(binary).hexdigest(),
                "bytes": len(binary),
            },
            "inputDigests": {"Cargo.lock": "a" * 64},
            "evidenceDigests": {"test.json": "b" * 64},
        }
    ).encode()
    members = dict(zip(guest.NAMES, (manifest, selection, binary), strict=True))
    pins = {name: (hashlib.sha256(data).hexdigest(), len(data)) for name, data in members.items()}
    measured: dict[str, object] = {
        "sha256": pins[guest.NAMES[2]][0],
        "bytes": len(binary),
        "architecture": "x86_64",
        "elfClass": 64,
        "littleEndian": True,
        "fileType": "dynamic",
        "programHeaders": 10,
        "noInterpreter": True,
        "neededCount": 0,
        "noNeeded": True,
        "pieFlag": True,
        "bindNow": True,
        "nonExecutableStack": True,
        "noWritableExecutableLoad": True,
        "relroPresent": True,
        "noRpath": True,
        "noRunpath": True,
        "noTextRelocations": True,
        "noLoaderHooks": True,
    }
    request: dict[str, object] = {"schemaVersion": 1}
    for key, name in zip(("manifest", "selection", "binary"), guest.NAMES, strict=True):
        request[key] = {
            "path": str(guest.INPUT / name),
            "sha256": pins[name][0],
            "bytes": pins[name][1],
        }
    raw_request = json.dumps(request, separators=(",", ":")).encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for name, data in members.items():
            item = tarfile.TarInfo(name)
            item.size, item.mode = len(data), mode
            archive.addfile(item, io.BytesIO(data))
        if extra is not None:
            item = tarfile.TarInfo(extra)
            item.size, item.mode = 1, 0o600
            archive.addfile(item, io.BytesIO(b"x"))
    transfer = buffer.getvalue()
    options = guest.Options(
        RUN,
        selection,
        selected,
        456,
        pins,
        hashlib.sha256(transfer).hexdigest(),
        len(transfer),
        raw_request,
        hashlib.sha256(raw_request).hexdigest(),
        measured,
    )
    return options, transfer


@final
class FileCloseFaults:
    """Wrap real owned files only to inject an error after an actual writer close."""

    def __init__(self) -> None:
        """Keep the original functions and sentinel error separate from patched globals."""
        self.fdopen = os.fdopen
        self.require = guest.require
        self.late = OSError("late-owned-writer-close")
        self.handles: list[BinaryIO] = []
        self.closed: list[str] = []
        self.body_errors: list[guest.OpsError] = []

    def opened(self, descriptor: int, mode: str) -> Mock:
        """Delegate every actual read/write/stat operation to the same real descriptor."""
        if mode not in ("w+b", "rb"):
            raise AssertionError
        source = self.fdopen(descriptor, mode)
        self.handles.append(source)

        def closed() -> None:
            source.close()
            self.closed.append(mode)
            if mode == "w+b":
                raise self.late

        return Mock(wraps=source, close=Mock(side_effect=closed))

    def required(self, condition: object, code: str = "ops_guest_unproven") -> None:
        """Observe the real failed pin check without changing its result or exception."""
        try:
            self.require(condition, code)
        except guest.OpsError as error:
            self.body_errors.append(error)
            raise


class GuestHelpTests(unittest.TestCase):
    """Bind the guest check to real maintained CLI vocabulary without executing a candidate."""

    @staticmethod
    def current_help() -> bytes:
        """Read only the fixed string literals in the maintained Rust concat constant."""
        source = (ROOT / "ops/src/main.rs").read_text()
        body = source.split("const USAGE: &str = concat!(", 1)[1].split("\n);", 1)[0]
        parts: list[str] = []
        for line in body.splitlines():
            if not line.strip():
                continue
            part = cast("object", json.loads(line.strip().removesuffix(",")))
            if not isinstance(part, str):
                message = "maintained help must remain fixed string data"
                raise TypeError(message)
            parts.append(part)
        return ("".join(parts) + "\n").encode()

    def test_current_legacy_and_additional_future_routes_preserve_required_commands(self) -> None:
        """Help ordering may grow while exact program and manifest contracts remain required."""
        current = self.current_help()
        legacy = b"\n".join(
            (
                b"Usage: simplestchat-ops release manifest --path PATH",
                b"       simplestchat-ops release preflight [--helpers-json JSON]",
                b"       simplestchat-ops distribution inspect-executable --path PATH",
                b"       simplestchat-ops distribution install --request ABS --request-sha256 SHA",
                (
                    b"       simplestchat-ops distribution verify-pair --manifest PATH "
                    b"--path PATH --selection PATH"
                ),
                b"",
            )
        )
        future = current + b"       simplestchat-ops release attestation verify [OPTIONS]\n"
        for output in (current, legacy, future):
            with self.subTest(output=output.splitlines()[0]):
                guest.help_receipt(output, b"")
        guest.help_receipt(current + b" " * (guest.MAX_HELP - len(current)), b"")

    def test_wrong_program_manifest_error_output_duplicate_usage_and_bound_refuse(self) -> None:
        """Related or prefixed manifest text cannot replace the complete fixed route line."""
        current = self.current_help()
        manifest = b"simplestchat-ops release manifest --path PATH\n"
        invalid = (
            current.replace(b"Usage: simplestchat-ops ", b"Usage: another-program ", 1),
            current.replace(manifest, b""),
            current.replace(manifest, b"simplestchat-ops release manifest --path WRONG\n"),
            current.replace(manifest, b"simplestchat-ops release manifest --path PATH --extra\n"),
            current + b"Usage: simplestchat-ops PRIVATE\n",
            current + b" " * (guest.MAX_HELP + 1 - len(current)),
        )
        for output in invalid:
            with self.subTest(output=output[:80]), self.assertRaises(guest.OpsError):
                guest.help_receipt(output, b"")
        with self.assertRaises(guest.OpsError):
            guest.help_receipt(current, b"PRIVATE stderr")

    def test_a_nonzero_help_child_remains_negative_before_receipt_validation(self) -> None:
        """The real case dispatcher refuses nonzero status even for complete valid help bytes."""
        current = self.current_help()
        for status in (1, 2):
            runner = object.__new__(guest.Runner)
            runner.records = [
                {"name": name, "exitStatus": expected, "passed": True}
                for name, expected in guest.VPS_CASES
            ]
            refused: BaseException | None = None
            with (
                io.BytesIO(b"inert original, never executed") as source,
                patch.object(guest.Runner, "check"),
                patch.object(guest.Runner, "execute", return_value=(status, current, b"")),
            ):
                try:
                    _ = runner.case("source-help", ["--help"], file=cast("BinaryIO", source))
                except guest.OpsError as error:
                    refused = error
            self.assertIsInstance(refused, guest.OpsError)
            self.assertEqual(
                runner.records[-1],
                {"name": "source-help", "exitStatus": status, "passed": False},
            )


class GuestInputTests(unittest.TestCase):
    """Mature tar framing and retained original metadata cannot adopt replacements."""

    def receiver(self, path: Path) -> guest.Receiver:
        """Use only the test's private directory and current UID, never fixed guest paths."""
        path.mkdir(mode=0o700)
        return guest.Receiver(path, owner=os.getuid())

    def test_exact_transfer_becomes_same_readonly_inode_with_one_mode_transition(self) -> None:
        """Writers close before executable mode is set; input identity and bytes stay original."""
        options, data = fixture()
        with tempfile.TemporaryDirectory() as directory:
            receiver = self.receiver(Path(directory) / "input")
            try:
                receiver.extract(io.BytesIO(data), options.pins, lambda: None)
                self.assertEqual(set(receiver.files), set(guest.NAMES))
                for name, held in receiver.files.items():
                    held.check(receiver.parent, lambda: None)
                    self.assertEqual(
                        stat.S_IMODE(held.metadata.st_mode),
                        0o755 if name == guest.NAMES[2] else 0o600,
                    )
                    self.assertFalse(held.file.writable())
                    self.assertEqual(held.metadata.st_nlink, 1)
                    with self.assertRaises(OSError):
                        _ = os.write(held.file.fileno(), b"x")
            finally:
                files = [held.file for held in receiver.files.values()]
                receiver.close()
            self.assertTrue(all(file.closed for file in files))
            self.assertEqual(receiver.parent, -1)

    def test_alias_extra_mode_trailer_and_late_checkpoint_refuse(self) -> None:
        """All framing/copy failures leave admitted owners available for all-attempt closure."""
        original, good = fixture()
        variants = [fixture(extra="extra")[1], fixture(mode=0o4600)[1], good + b"foreign"]
        alias = bytearray(good)
        alias[:100] = b"./ops-release.json".ljust(100, b"\0")
        alias[148:156] = b"        "
        checksum = sum(alias[:512])
        alias[148:156] = f"{checksum:06o}\0 ".encode()
        variants.append(bytes(alias))
        for index, data in enumerate(variants):
            with self.subTest(index=index), tempfile.TemporaryDirectory() as directory:
                receiver = self.receiver(Path(directory) / "input")
                try:
                    with self.assertRaises((guest.OpsError, tarfile.TarError)):
                        receiver.extract(io.BytesIO(data), original.pins, lambda: None)
                finally:
                    receiver.close()
                self.assertEqual(receiver.parent, -1)

    def test_same_bytes_replacement_and_same_inode_rewrite_refuse(self) -> None:
        """Fresh matching bytes do not become a new baseline and same-length mutation is visible."""
        options, data = fixture()
        for replacement in (False, True):
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as directory:
                receiver = self.receiver(Path(directory) / "input")
                try:
                    receiver.extract(io.BytesIO(data), options.pins, lambda: None)
                    held = receiver.files[guest.NAMES[0]]
                    path = receiver.root / held.name
                    original = path.read_bytes()
                    if replacement:
                        _ = path.rename(path.with_name("original"))
                        _ = path.write_bytes(original)
                        path.chmod(0o600)
                    else:
                        with path.open("r+b") as output:
                            _ = output.write(b"x" + original[1:])
                    with self.assertRaises(guest.OpsError):
                        held.check(receiver.parent, lambda: None)
                finally:
                    receiver.close()

    def test_all_file_closes_attempted_after_an_earlier_close_error(self) -> None:
        """A failed first peer never skips the later admitted File or directory owner."""
        with tempfile.TemporaryDirectory() as directory:
            receiver = self.receiver(Path(directory) / "input")
            calls: list[str] = []

            class Broken(io.BytesIO):
                @override
                def close(self) -> None:
                    calls.append("first")
                    super().close()
                    raise KeyboardInterrupt

            class Later(io.BytesIO):
                @override
                def close(self) -> None:
                    calls.append("second")
                    super().close()

            metadata = Path(directory).stat()
            receiver.files = {
                "first": guest.Held(Broken(), "first", metadata, "0" * 64, 0),
                "second": guest.Held(Later(), "second", metadata, "0" * 64, 0),
            }
            with self.assertRaises(guest.OpsError):
                receiver.close()
            self.assertEqual(calls, ["first", "second"])
            self.assertEqual(receiver.parent, -1)

    def test_member_pin_failure_precedes_close_failure_and_cleanup_only_failure_stays_negative(
        self,
    ) -> None:
        """Retain the original failed hash and close the reader even after a writer close fault."""
        for bad_pin in (True, False):
            with self.subTest(bad_pin=bad_pin):
                self.member_close_case(bad_pin=bad_pin)

    def member_close_case(self, *, bad_pin: bool) -> None:
        """Use actual inert file bytes and observe the exception raised by the real pin check."""
        with tempfile.TemporaryDirectory() as directory:
            receiver = self.receiver(Path(directory) / "input")
            item = tarfile.TarInfo(guest.NAMES[2])
            item.size = 3
            faults = FileCloseFaults()
            expected = "0" * 64 if bad_pin else hashlib.sha256(b"abc").hexdigest()
            try:
                with (
                    patch.object(os, "fdopen", side_effect=faults.opened),
                    patch.object(guest, "require", side_effect=faults.required),
                    self.assertRaises((guest.OpsError, OSError)) as raised,
                ):
                    receiver.member(item, io.BytesIO(b"abc"), (expected, 3), lambda: None)
                self.assertFalse(receiver.files)
                self.assertTrue(all(source.closed for source in faults.handles))
                self.assertEqual(len(faults.handles), 1 if bad_pin else 2)
                self.assertEqual(stat.S_IMODE((receiver.root / item.name).stat().st_mode), 0o600)
                if bad_pin:
                    self.assertEqual(faults.body_errors, [raised.exception])
                    self.assertIsInstance(raised.exception, guest.OpsError)
                else:
                    self.assertIs(raised.exception, faults.late)
                    self.assertIn("rb", faults.closed)
            finally:
                receiver.close()

    def test_whole_transfer_pin_failure_precedes_after_effect_writer_close_failure(self) -> None:
        """The whole-transfer digest remains primary without changing root-only admission policy."""
        options, data = fixture()
        options = replace(options, transfer_sha256="0" * 64)
        faults = FileCloseFaults()
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(guest, "ROOT", Path(directory)),
            patch.object(os, "fdopen", side_effect=faults.opened),
            patch.object(guest, "require", side_effect=faults.required),
            self.assertRaises(guest.OpsError) as raised,
        ):
            _ = guest.receive(io.BytesIO(data), options, guest.Control())
        self.assertEqual(faults.body_errors, [raised.exception])
        self.assertEqual(len(faults.handles), 1)
        self.assertTrue(faults.handles[0].closed)
        self.assertEqual(faults.closed, ["w+b"])


class GuestProtocolTests(unittest.TestCase):
    """Controller pins, strict command projections and first-known status remain independent."""

    def test_original_argv_roundtrip_and_install_request_bytes_are_not_reconstructed(self) -> None:
        """Retain whitespace in a pinned request through the keeper/guest interface."""
        options, _ = fixture()
        raw = b" \n" + options.install_raw + b"\n"
        options = replace(options, install_raw=raw, install_sha256=hashlib.sha256(raw).hexdigest())
        parsed = guest.Options.parse(options.argv())
        self.assertEqual(parsed, options)
        for flag in ("--root", "--host", "--executable", "--command"):
            with (
                self.subTest(flag=flag),
                patch("sys.stderr", new=io.StringIO()),
                self.assertRaises(SystemExit),
            ):
                _ = guest.Options.parse([*options.argv(), flag, str(guest.ROOT / "forbidden")])

    def test_measurement_requires_all_nineteen_original_fields_and_exact_types(self) -> None:
        """Reject subsets and boolean aliases for the original controller ELF facts."""
        options, _ = fixture()
        self.assertEqual(len(options.measured), 19)
        self.assertEqual(
            guest.executable(json.dumps(options.measured).encode(), *options.pins[guest.NAMES[2]]),
            options.measured,
        )
        for key in options.measured:
            value = dict(options.measured)
            del value[key]
            with self.subTest(key=key), self.assertRaises(guest.OpsError):
                _ = guest.executable(json.dumps(value).encode(), *options.pins[guest.NAMES[2]])
        for value in (
            {**options.measured, "littleEndian": 1},
            {**options.measured, "elfClass": True},
        ):
            with self.assertRaises(guest.OpsError):
                _ = guest.executable(json.dumps(value).encode(), *options.pins[guest.NAMES[2]])

    def test_preflight_normalizes_only_independent_uuid4(self) -> None:
        """UUID version/variant and complete schemas remain checked before comparison."""
        value = {"schemaVersion": 1, "runId": "11111111111141118111111111111111", "prepared": True}
        self.assertEqual(
            guest.preflight_projection(json.dumps(value).encode()),
            {**value, "runId": "<independent-uuid4>"},
        )
        for row in (
            {**value, "runId": "1" * 32},
            {**value, "prepared": 1},
            {**value, "private": "secret"},
        ):
            with self.assertRaises(guest.OpsError):
                _ = guest.preflight_projection(json.dumps(row).encode())

    def test_receipts_require_original_pins_and_strict_boolean_integer_types(self) -> None:
        """JSON equality cannot promote float counts or integer success flags."""
        options, _ = fixture()
        install: dict[str, object] = {"schemaVersion": 1, "passed": True, "changed": True}
        for prefix, name in zip(("manifest", "selection", "binary"), guest.NAMES, strict=True):
            install[prefix + "Sha256"], install[prefix + "Bytes"] = options.pins[name]
        guest.install_receipt(json.dumps(install).encode(), b"", options, changed=True)
        for key, item in (
            ("schemaVersion", 1.0),
            ("passed", 1),
            ("changed", 1),
            ("binaryBytes", float(options.pins[guest.NAMES[2]][1])),
        ):
            with self.subTest(key=key), self.assertRaises(guest.OpsError):
                guest.install_receipt(
                    json.dumps({**install, key: item}).encode(), b"", options, changed=True
                )
        error = {"schemaVersion": 1, "passed": False, "failure": "ops_install_unproven"}
        for value in ({**error, "passed": 0}, {**error, "schemaVersion": True}):
            with self.assertRaises(guest.OpsError):
                guest.error_receipt(
                    json.dumps(value).encode(), b"", "ops_install_unproven", schema=True
                )
        self.pair_types(options)

    def pair_types(self, options: guest.Options) -> None:
        """Check selected numeric claim types against the separate original selection."""
        claims = {
            "schemaVersion": 1,
            "artifactType": "simplestchat-ops",
            **options.selected,
            "inputDigestCount": 1,
            "evidenceDigestCount": 1,
        }
        manifest: dict[str, object] = {
            "inputDigests": {"lock": "a" * 64},
            "evidenceDigests": {"test": "b" * 64},
        }
        pair = {
            "manifestSha256": options.pins[guest.NAMES[0]][0],
            "manifestBytes": options.pins[guest.NAMES[0]][1],
            "claims": claims,
            "executable": options.measured,
        }
        guest.pair_receipt(json.dumps(pair).encode(), b"", options, manifest)
        for key, item in (("protocolVersion", True), ("runAttempt", 1.0), ("buildRunId", 123.0)):
            with self.subTest(key=key), self.assertRaises(guest.OpsError):
                guest.pair_receipt(
                    json.dumps({**pair, "claims": {**claims, key: item}}).encode(),
                    b"",
                    options,
                    manifest,
                )

    def test_isolated_bytecode_directory_must_remain_empty(self) -> None:
        """Source hashes and -B cannot authorize a planted ignored bytecode file."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache = root / "python-cache"
            cache.mkdir(mode=0o700)
            original_directory = guest.directory
            with (
                patch.object(guest, "ROOT", root),
                patch.object(
                    guest, "directory", side_effect=partial(original_directory, owner=os.getuid())
                ),
            ):
                guest.empty_cache()
                _ = (cache / "foreign.pyc").write_bytes(b"not source authority")
                with self.assertRaises(guest.OpsError):
                    guest.empty_cache()

    def test_vps_source_namespace_rejects_siblings_links_and_late_directory_changes(self) -> None:
        """Only three original source leaves become importable, with full directory checks."""
        for fault in ("extra", "link", "replace"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                os.chown(root, -1, os.getgid())
                for name in guest.VPS_SOURCE_NAMES:
                    path = root / name
                    _ = path.write_bytes(b"inert fixture source\n")
                    path.chmod(0o600)
                control = guest.Control(20)
                original_directory = guest.directory
                with (
                    patch.object(guest, "VPS_SOURCE", root),
                    patch.object(
                        guest,
                        "directory",
                        side_effect=partial(original_directory, owner=os.getuid()),
                    ),
                ):
                    before = guest.vps_namespace(control)
                    if fault == "extra":
                        _ = (root / "argparse.pyc").write_bytes(b"ignored sibling")
                    elif fault == "link":
                        target = root / "vps_ops_bootstrap.py"
                        target.unlink()
                        target.symlink_to(root / "security_vm_ops_guest.py")
                    else:
                        target = root / "vps_ops_bootstrap.py"
                        data = target.read_bytes()
                        target.unlink()
                        _ = target.write_bytes(data)
                        target.chmod(0o600)
                    with self.assertRaises(guest.OpsError):
                        _ = guest.vps_namespace(control, before if fault == "replace" else None)

    def test_vps_namespace_primary_precedes_after_effect_close_and_signal(self) -> None:
        """A known rejected sibling remains primary while the actual FD still closes."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            os.chown(root, -1, os.getgid())
            control = guest.Control(20)
            _ = (root / "argparse.pyc").write_bytes(b"rejected before import")
            original_directory, original_close = guest.directory, os.close
            closed: list[int] = []

            def close(fd: int) -> None:
                original_close(fd)
                closed.append(fd)
                control.signal(15, None)
                message = "late-owned-close"
                raise OSError(message)

            with (
                patch.object(guest, "VPS_SOURCE", root),
                patch.object(
                    guest, "directory", side_effect=partial(original_directory, owner=os.getuid())
                ),
                patch.object(os, "close", side_effect=close),
                self.assertRaises(guest.OpsError),
            ):
                _ = guest.vps_namespace(control)
            self.assertEqual(control.status, 1)
            self.assertEqual(len(closed), 1)
            with self.assertRaises(OSError):
                _ = os.fstat(closed[0])

    def test_cancel_and_original_deadline_latch_without_resetting_cleanup(self) -> None:
        """Repeated signals do not throw into owners, and cleanup uses the original extra tail."""
        with patch.object(time, "monotonic", return_value=10):
            control = guest.Control(20)
        control.signal(15, None)
        control.signal(2, None)
        self.assertEqual(control.status, 143)
        control.settling = True
        with patch.object(time, "monotonic", return_value=149):
            control.check()
        with (
            patch.object(time, "monotonic", return_value=150),
            self.assertRaises(guest.OpsError),
        ):
            control.check()
        self.assertEqual(control.status, 143)
        control.status = 1
        control.signal(15, None)
        self.assertEqual(control.status, 1)


@dataclass
class GuestScenario:
    """One inert body/finalizer observation; no candidate runtime is claimed."""

    options: guest.Options
    fault: str | None
    control: guest.Control
    trace: list[str] = field(default_factory=list)
    clock: float = 0

    def now(self) -> float:
        """Sample the original fixed clock without resetting its deadline."""
        return self.clock

    def exercise(self, runner: FinalRunner, selected: guest.Options) -> None:
        """Supply inert case observations at the isolated runtime boundary."""
        if selected is not self.options:
            raise AssertionError
        if self.fault == "body":
            raise OSError
        runner.records.extend(
            {"name": name, "exitStatus": status, "passed": True} for name, status in guest.CASES
        )


@final
class FinalTransfer(io.BytesIO):
    """Expose one explicitly tracked final close."""

    def __init__(self, state: GuestScenario) -> None:
        """Create a memory-only supplied transfer owner."""
        super().__init__()
        self.name = "inert-supplied-transfer"
        self.state = state

    @override
    def close(self) -> None:
        """Record closure before releasing the inert source."""
        self.state.trace.append("transfer")
        super().close()


@final
class FinalReceiver:
    """Model the already-tested receiver's one finalizer boundary."""

    def __init__(self, state: GuestScenario) -> None:
        """Keep this scenario's fault and trace independent from every other run."""
        self.state = state

    def close(self) -> None:
        """Retain a negative close result without skipping the following peer."""
        self.state.trace.append("receiver")
        if self.state.fault == "receiver-close":
            raise OSError


@final
class FinalRunner:
    """Supply a completed inert body and independently fallible finalizer."""

    def __init__(self, selected: guest.Control, receiver: FinalReceiver) -> None:
        """Use only the scenario's original control owner."""
        self.state = receiver.state
        self.control = selected
        self.receiver = receiver
        if selected is not self.state.control:
            raise AssertionError
        self.records: list[dict[str, object]] = []
        self.install_first, self.install_second = True, False

    def check(self) -> None:
        """Use the original monotonic deadline."""
        self.state.control.check()

    def finish(self) -> None:
        """Inject a close failure, signal or elapsed cleanup boundary after body success."""
        state = self.state
        state.trace.append("runner")
        if state.fault == "runner-close":
            raise OSError
        if state.fault == "late-cancel":
            state.control.signal(15, None)
        if state.fault == "late-deadline":
            state.clock = state.control.end + 120


class GuestFinalizationTests(unittest.TestCase):
    """Use supplied inert runtime observations to test original owner completion only."""

    def test_five_custody_probes_precede_all_original_install_cases_under_one_owner(self) -> None:
        """Keep actual guest run ordering/finalization while substituting both runtimes."""
        options, _ = fixture()
        control = guest.Control(200)
        state = GuestScenario(options, None, control, clock=control.started)
        transfer = FinalTransfer(state)

        def probes(runner: FinalRunner, selected: guest.Options) -> None:
            self.assertIs(selected, options)
            self.assertIs(runner.control, control)
            self.assertIs(runner.receiver.state, state)
            self.assertEqual(runner.records, [])
            state.trace.append("probes")
            runner.records.extend(
                {"name": name, "exitStatus": status, "passed": True}
                for name, status in guest.VPS_CASES
            )

        def install(runner: FinalRunner, selected: guest.Options) -> None:
            self.assertIs(selected, options)
            self.assertIs(runner.control, control)
            self.assertEqual(len(runner.records), len(guest.VPS_CASES))
            state.trace.append("install")
            runner.records.extend(
                {"name": name, "exitStatus": status, "passed": True}
                for name, status in guest.INSTALL_CASES
            )

        with (
            patch.object(guest, "guard"),
            patch.object(guest, "receive", return_value=(transfer, FinalReceiver(state))),
            patch.object(guest, "Runner", FinalRunner),
            patch.object(guest, "vps_cases", side_effect=probes),
            patch.object(guest, "exercise", side_effect=install),
            patch.object(time, "monotonic", side_effect=state.now),
        ):
            report = guest.run(options, io.BytesIO(), control)
        self.assertTrue(report["passed"])
        self.assertEqual(len(cast("list[object]", report["cases"])), 33)
        self.assertEqual(len(guest.INSTALL_CASES), 28)
        self.assertEqual(state.trace, ["probes", "install", "runner", "receiver", "transfer"])

    def test_probe_adapter_refuses_partial_wrong_or_unproven_custody_rows(self) -> None:
        """A typed source module still cannot supply malformed positive observations."""
        for fault in (None, "missing", "order", "status", "cloexec", "readonly", "image", "extra"):
            with self.subTest(fault=fault):
                self.probe_adapter(fault)

    def probe_adapter(self, fault: str | None) -> None:
        """Exercise the real closed adapter with one inert fixed-module boundary."""
        options, _ = fixture()
        control = guest.Control(200)
        state = GuestScenario(options, None, control)
        runner = FinalRunner(control, FinalReceiver(state))
        rows: list[dict[str, object]] = [
            {
                "name": name,
                "exitStatus": status,
                "passed": True,
                "kernelImageObserved": name != guest.VPS_CASES[-1][0],
                "originalCloexec": True,
                "originalReadonly": True,
            }
            for name, status in guest.VPS_CASES
        ]
        if fault == "missing":
            _ = rows.pop()
        if fault == "order":
            rows.reverse()
        for condition, key, value in [
            ("status", "exitStatus", True),
            ("cloexec", "originalCloexec", False),
            ("readonly", "originalReadonly", False),
            ("image", "kernelImageObserved", False),
            ("extra", "PRIVATE-extra", True),
        ]:
            if fault == condition:
                rows[0][key] = value

        def exercise(
            selected: guest.Options, receiver: FinalReceiver, owner: guest.Control
        ) -> list[dict[str, object]]:
            self.assertIs(selected, options)
            self.assertIs(receiver, runner.receiver)
            self.assertIs(owner, control)
            return rows

        module = SimpleNamespace(
            CASES=tuple((name, status, b"unused") for name, status in guest.VPS_CASES),
            exercise=exercise,
        )
        with (
            patch.object(guest, "import_module", return_value=module),
            patch.object(sys, "path", list(sys.path)),
        ):
            if fault is None:
                guest.vps_cases(cast("guest.Runner", cast("object", runner)), options)
                self.assertEqual(
                    [row["name"] for row in runner.records], [name for name, _ in guest.VPS_CASES]
                )
                self.assertTrue(
                    all(set(row) == {"name", "exitStatus", "passed"} for row in runner.records)
                )
            else:
                with self.assertRaises(ValueError):
                    guest.vps_cases(cast("guest.Runner", cast("object", runner)), options)

    def test_late_cancellation_deadline_and_peer_close_failures_remain_negative(self) -> None:
        """All acquired peers settle even after an earlier finalizer or the body fails."""
        for fault in (
            None,
            "runner-close",
            "receiver-close",
            "late-cancel",
            "late-deadline",
            "body",
        ):
            with self.subTest(fault=fault):
                self.guest_case(fault)

    def guest_case(self, fault: str | None) -> None:
        """Model the execution boundary; no candidate, marked host or VM is acquired."""
        options, _ = fixture()
        control = guest.Control(20)
        state = GuestScenario(options, fault, control, clock=control.started)
        transfer = FinalTransfer(state)
        with (
            patch.object(guest, "guard"),
            patch.object(guest, "receive", return_value=(transfer, FinalReceiver(state))),
            patch.object(guest, "Runner", FinalRunner),
            patch.object(guest, "vps_cases"),
            patch.object(guest, "exercise", side_effect=state.exercise),
            patch.object(time, "monotonic", side_effect=state.now),
        ):
            report = guest.run(options, io.BytesIO(), control)
        self.assertEqual(state.trace, ["runner", "receiver", "transfer"])
        self.assertTrue(transfer.closed)
        self.assertEqual(report["passed"], fault is None)
        if fault == "late-cancel":
            self.assertEqual(report["failure"], "cancelled")
            self.assertEqual(control.status, 143)
        if fault == "late-deadline":
            self.assertEqual(report["failure"], "deadline")
            self.assertEqual(control.status, 124)
        if fault == "body":
            self.assertEqual(report["failure"], "cases")
            self.assertEqual(control.status, 1)

    def test_private_dispatch_closes_every_pipe_and_preserves_the_original_failure(self) -> None:
        """The selected original FD stays borrowed on construction, pump and close failures."""
        for fault in ("spawn", "pump", "close"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "inert-input"
                _ = path.write_bytes(b"inert fixture, never executed")
                with path.open("rb") as source:
                    self.dispatch_case(source, fault)
                    self.assertFalse(source.closed)

    def test_pump_health_expiry_survives_guest_run_and_all_owned_finalizers(self) -> None:
        """A real callback-to-run path retains 124 instead of replacing it with generic failure."""
        options, _ = fixture()
        control = guest.Control(20)
        state = GuestScenario(options, None, control, clock=control.started)
        runner = object.__new__(guest.Runner)
        runner.control, runner.env, runner.records = control, {}, []
        runner.installed, runner.install_first, runner.install_second = None, None, None
        transfer = FinalTransfer(state)
        health: list[bool] = []

        class Pipe(io.BytesIO):
            name: str

            def __init__(self, name: str) -> None:
                super().__init__()
                self.name = name

            @override
            def close(self) -> None:
                state.trace.append(self.name)
                super().close()

        output, error = Pipe("stdout"), Pipe("stderr")
        child = Mock(stdin=None, stdout=output, stderr=error)

        def pump(
            selected: subprocess.Popen[bytes],
            _output: BinaryIO,
            _error: BinaryIO,
            limits: bounded_process.Limits,
        ) -> int:
            self.assertIs(selected, child)
            state.clock = control.end + 1
            if limits.healthy is None:
                raise AssertionError
            health.append(limits.healthy())
            message = "command_lease_lost"
            raise bounded_process.ProcessError(message)

        def stop(selected: subprocess.Popen[bytes]) -> None:
            self.assertIs(selected, child)
            state.trace.append("stop")

        def finish() -> None:
            state.trace.append("runner")

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inert-input"
            _ = path.write_bytes(b"inert fixture, never executed")
            with path.open("rb") as source:

                def exercise(selected: guest.Runner, selected_options: guest.Options) -> None:
                    self.assertIs(selected, runner)
                    self.assertIs(selected_options, options)
                    _ = selected.execute(source, ["--help"])

                with (
                    patch.object(guest, "guard"),
                    patch.object(guest, "receive", return_value=(transfer, FinalReceiver(state))),
                    patch.object(guest, "Runner", return_value=runner),
                    patch.object(guest, "vps_cases"),
                    patch.object(guest, "exercise", side_effect=exercise),
                    patch.object(runner, "process", bounded_process, create=True),
                    patch.object(runner, "finish", side_effect=finish),
                    patch.object(guest, "empty_cache"),
                    patch.object(subprocess, "Popen", return_value=child),
                    patch.object(bounded_process, "pump", side_effect=pump),
                    patch.object(bounded_process, "stop", side_effect=stop),
                    patch.object(time, "monotonic", side_effect=state.now),
                ):
                    report = guest.run(options, io.BytesIO(), control)
                self.assertFalse(source.closed)
        self.assertEqual(health, [False])
        self.assertEqual(control.status, 124)
        self.assertEqual(report["failure"], "deadline")
        self.assertFalse(report["passed"])
        self.assertEqual(
            state.trace, ["stop", "stdout", "stderr", "runner", "receiver", "transfer"]
        )
        self.assertTrue(output.closed and error.closed and transfer.closed)

    def test_expiry_between_admission_samples_is_latched_without_spawning(self) -> None:
        """A second clock sample cannot turn an observed deadline into a generic refusal."""
        control = guest.Control(20)
        runner = object.__new__(guest.Runner)
        runner.control, runner.env = control, {}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inert-input"
            _ = path.write_bytes(b"never executed")
            with (
                path.open("rb") as source,
                patch.object(time, "monotonic", side_effect=(control.end - 1, control.end)),
                patch.object(subprocess, "Popen") as launch,
                self.assertRaises(guest.OpsError),
            ):
                _ = runner.execute(source, ["--help"])
            launch.assert_not_called()
        self.assertEqual(control.status, 124)

    def test_late_bytecode_namespace_check_cannot_admit_a_process(self) -> None:
        """Resample the original owner after filesystem work and before first-install dispatch."""
        control = guest.Control(20)
        state = GuestScenario(fixture()[0], None, control, clock=control.started)
        runner = object.__new__(guest.Runner)
        runner.control, runner.env = control, {}

        def cache_check() -> None:
            state.clock = control.end

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inert-input"
            _ = path.write_bytes(b"never executed")
            with path.open("rb") as source:
                with (
                    patch.object(time, "monotonic", side_effect=state.now),
                    patch.object(guest, "empty_cache", side_effect=cache_check),
                    patch.object(subprocess, "Popen", side_effect=AssertionError) as launch,
                    self.assertRaises(guest.OpsError),
                ):
                    _ = runner.execute(source, ["distribution", "install"])
                launch.assert_not_called()
                self.assertFalse(source.closed)
        self.assertEqual(control.status, 124)

    def dispatch_case(self, source: BinaryIO, fault: str) -> None:
        """Replace Popen with inert pipes; exercise the real dispatch/finalizer adapter."""
        trace: list[str] = []
        original = OSError("private-original")
        cleanup = OSError("private-cleanup")

        class Output(io.BytesIO):
            @override
            def close(self) -> None:
                trace.append("stdout")
                super().close()
                raise cleanup

        class Error(io.BytesIO):
            @override
            def close(self) -> None:
                trace.append("stderr")
                super().close()

        output, error = Output(), Error()
        child = Mock(stdin=None, stdout=output, stderr=error)
        runner = object.__new__(guest.Runner)
        runner.control, runner.env = guest.Control(), {}
        with (
            patch.object(runner, "process", bounded_process, create=True),
            patch.object(guest, "empty_cache"),
            patch.object(
                subprocess,
                "Popen",
                return_value=child,
                side_effect=original if fault == "spawn" else None,
            ) as launch,
            patch.object(
                bounded_process,
                "pump",
                return_value=0,
                side_effect=original if fault == "pump" else None,
            ),
            patch.object(bounded_process, "stop", side_effect=cleanup) as stop,
            self.assertRaises(OSError) as caught,
        ):
            _ = runner.execute(source, ["--help"])
        self.assertIs(caught.exception, cleanup if fault == "close" else original)
        self.assertEqual(launch.call_args.kwargs["pass_fds"], (source.fileno(),))
        self.assertTrue(launch.call_args.kwargs["close_fds"])
        self.assertEqual(
            launch.call_args.kwargs["executable"], "/proc/self/fd/" + str(source.fileno())
        )
        if fault == "spawn":
            self.assertFalse(trace)
            with suppress(OSError):
                output.close()
            error.close()
        else:
            self.assertEqual(trace, ["stdout", "stderr"])
        if fault == "pump":
            stop.assert_called_once_with(child)
        else:
            stop.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
