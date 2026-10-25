"""Inert restore keeper ownership, clocks and closed observations; no engine or VM."""

from __future__ import annotations

import copy
import fcntl
import io
import json
import os
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, ClassVar, cast, final, override
from unittest.mock import patch

from test_support import ROOT, obj, objects, string, yaml_value

# isort: split
import restore_verify as reference
import security_vm as vm
import security_vm_fetch as fetch
import security_vm_ops as ops
import security_vm_ops_guest as ops_guest
import security_vm_receive as receive
import security_vm_restore as keeper
import security_vm_restore_guest as guest
import security_vm_stage_guest as stage
import test_security_vm_ops_guest as ops_fixture
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from typing import BinaryIO


TOKEN = "a" * 32
REVISION = "b" * 40
CID = "c" * 64
IMAGE = "sha256:" + "d" * 64
BINARY_SHA = "e" * 64
VERSION = "29.8.2"
TERMINATED = 128 + int(signal.SIGTERM)


def write_input(path: Path, data: bytes) -> None:
    """Create only owned fixture leaves with a fixed private initial mode."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        if stream.write(data) != len(data):
            message = "short_inert_fixture_write"
            raise OSError(message)
        stream.flush()


def result(scenario: str = "current") -> dict[str, object]:
    """Supply only the closed independently required final semantic facts."""
    negative = scenario == "privacy-invalid"
    return {
        "containerId": CID,
        "postgresImage": IMAGE,
        "sourceUnchanged": True,
        "sourceLedgerAndCountsUnchanged": True,
        "snapshotAbsent": True,
        "labelAbsent": True,
        "isolationInspected": True,
        "actorRemoved": True,
        "freshnessMatched": not negative,
        "freshnessPreserved": negative,
        "lockReacquired": True,
        "verified": not negative,
        "cleanupPassed": True,
        "counts": None
        if negative
        else {
            "users": 1,
            "rooms": 1,
            "sessions": int(scenario.startswith("historical")),
            "credentials": 0,
            "activeIncidents": 1,
            "resolvedIncidents": 1,
        },
        "backupSha256": BINARY_SHA,
        "outcomePresent": not negative,
        "privacyRefusalObserved": negative,
        "nativeRawExit": int(negative),
        "engineVersion": VERSION,
        "postgresVersionMatched": True,
        "historyObserved": True,
        "restoreExecObserved": True,
        "amcheckExecObserved": not negative,
    }


def inspection() -> list[object]:
    """Return private Docker-shaped fields only, without executing a daemon."""
    return [
        {
            "Id": CID,
            "Name": "/scpub-restore-" + TOKEN,
            "Image": IMAGE,
            "Config": {
                "User": "999:999",
                "Entrypoint": ["docker-entrypoint.sh"],
                "Cmd": reference.create_arguments("scpub-restore-" + TOKEN, IMAGE)[
                    reference.create_arguments("scpub-restore-" + TOKEN, IMAGE).index(IMAGE) + 1 :
                ],
                "Env": [
                    "PGDATA=/var/lib/postgresql/data",
                    "POSTGRES_DB=restorecheck",
                    "POSTGRES_HOST_AUTH_METHOD=trust",
                    "POSTGRES_INITDB_ARGS=--auth-local=peer --auth-host=reject",
                ],
                "Labels": {reference.LABEL: "scpub-restore-" + TOKEN},
            },
            "HostConfig": {
                "NetworkMode": "none",
                "ReadonlyRootfs": True,
                "Privileged": False,
                "CapDrop": ["ALL"],
                "CapAdd": None,
                "SecurityOpt": ["no-new-privileges:true"],
                "Memory": 512 * 1024**2,
                "MemorySwap": 512 * 1024**2,
                "PidsLimit": 64,
                "NanoCpus": 500000000,
                "ShmSize": 16 * 1024**2,
                "LogConfig": {"Type": "none"},
                "Tmpfs": {
                    "/var/lib/postgresql": (
                        "rw,nosuid,nodev,noexec,size=256m," + "uid=999,gid=999,mode=0700"
                    ),
                    "/var/run/postgresql": (
                        "rw,nosuid,nodev,noexec,size=1m," + "uid=999,gid=999,mode=0700"
                    ),
                    "/tmp": "rw,nosuid,nodev,noexec,size=16m,mode=1777",  # noqa: S108 -- Container.
                },
            },
            "Mounts": [],
        }
    ]


class RestoreClockTests(unittest.TestCase):
    """Entry origins and post-observation crossings cannot create extra setup time."""

    def test_controller_origins_and_exact_setup_end(self) -> None:
        """The same360/1320/1800 boundaries apply at unrelated monotonic origins."""
        for origin in (0.0, 100.0, 10000.0):
            with self.subTest(origin=origin):
                control = keeper.Control(started=origin)
                with patch.object(time, "monotonic", return_value=origin + 359.9):
                    control.advance()
                self.assertFalse(control.setup)
                self.assertEqual(control.end, origin + 1800)
                self.assertEqual(control.body_end, origin + 1680)
                late = keeper.Control(started=origin)
                with (
                    patch.object(time, "monotonic", return_value=origin + 360),
                    self.assertRaises((ValueError, ToolError)),
                ):
                    late.advance()
                self.assertTrue(late.setup)

    def test_controller_final_live_setup_check_catches_clock_crossing(self) -> None:
        """Remaining debt passes, then the actual final setup fence refuses."""
        control = keeper.Control(started=0)
        with (
            patch.object(time, "monotonic", side_effect=[359.9, 359.9, 360.1]),
            self.assertRaises((ValueError, ToolError)),
        ):
            control.advance()
        self.assertTrue(control.setup)

    def test_guest_phase_has_the_same_live_crossing_and_cancellation_guard(self) -> None:
        """The guest never widens on a stale admitted duration or pending signal."""
        for cancelled in (False, True):
            control = guest.Control(0)
            control.setup_end = 360
            if cancelled:
                control.signal(signal.SIGTERM, None)
            with (
                patch.object(time, "monotonic", side_effect=[359.9, 359.9, 360.1]),
                self.assertRaises((ValueError, ToolError)),
            ):
                control.advance()
            self.assertTrue(control.setup)

    def test_finalization_does_not_rewrite_native_or_fixture_debt(self) -> None:
        """Normal work at B refuses even when original final N remains open."""
        control = keeper.Control(started=0)
        control.setup = False
        with (
            patch.object(time, "monotonic", return_value=1680),
            self.assertRaises((ValueError, ToolError)),
        ):
            control.check()
        control.settling = True
        with patch.object(time, "monotonic", return_value=1680):
            control.check()
        self.assertEqual((keeper.NATIVE, keeper.FINALIZATION, keeper.DEBT), (1200, 120, 1320))


class RestoreProjectionTests(unittest.TestCase):
    """External output cannot substitute private values for fixed semantic claims."""

    def test_three_cases_keep_expected_native_verdicts_separate(self) -> None:
        """A successful negative fixture still records actual native1 and no freshness."""
        for name in guest.CASES:
            self.assertEqual(guest.native_projection(result(name), name), result(name))

    def test_unknown_nested_secret_and_bool_status_are_refused(self) -> None:
        """Neither arbitrary fields nor bool-as-int statuses can escape the summary."""
        for field, value in (
            ("secret", {"token": "PRIVATE"}),
            ("nativeRawExit", True),
            ("counts", {"users": True}),
            ("engineVersion", "PRIVATE"),
        ):
            row = result()
            row[field] = value
            with (
                self.subTest(field=field),
                self.assertRaises(ValueError) as failure,
            ):
                _ = guest.native_projection(row, "current")
            self.assertNotIn("PRIVATE", str(failure.exception))

    def test_unknown_failed_checkpoint_is_never_echoed(self) -> None:
        """Only a closed checkpoint/first-cause projection is accepted by the parent."""
        expected: dict[str, object] = {
            "runId": TOKEN,
            "revision": REVISION,
            "scenario": "current",
            "sourcePins": {},
            "engineVersion": VERSION,
        }
        frame: dict[str, object] = {
            "schemaVersion": 1,
            "scope": "RestoreVerificationGuest",
            **{k: expected[k] for k in ("runId", "revision", "scenario", "sourcePins")},
            "passed": False,
            "failure": "refused",
            "nativeRawExit": 0,
            "nativePassed": True,
            "finalStatus": 0,
            "checkpoint": "observe",
            "cleanupPassed": True,
            "native": None,
        }
        self.assertEqual(keeper.projection(frame, expected)["nativeRawExit"], 0)
        for changed in ("PRIVATE", {"secret": "PRIVATE"}):
            bad = dict(frame, checkpoint=changed)
            with self.assertRaises(ToolError) as caught:
                _ = keeper.projection(bad, expected)
            self.assertNotIn("PRIVATE", str(caught.exception))

    def test_inspection_join_and_restrictions_refuse_foreign_or_weakened_actor(self) -> None:
        """Same bytes or a familiar name cannot replace the actual known identity/profile."""
        expected = {"name": "scpub-restore-" + TOKEN, "image": IMAGE}
        self.assertEqual(guest.isolation(inspection(), expected)["id"], CID)
        for key, value in (
            ("NetworkMode", "bridge"),
            ("ReadonlyRootfs", False),
            ("Privileged", True),
            ("MemorySwap", 0),
        ):
            row = copy.deepcopy(inspection())
            cast("dict[str, object]", cast("dict[str, object]", row[0])["HostConfig"])[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                _ = guest.isolation(row, expected)
        foreign = copy.deepcopy(inspection())
        cast("dict[str, object]", foreign[0])["Image"] = "sha256:" + "f" * 64
        with self.assertRaises(ValueError):
            _ = guest.isolation(foreign, expected)

    def test_engine_decoder_refuses_duplicates_and_nonfinite(self) -> None:
        """The mature byte decoder remains authoritative for raw engine observations."""
        for data in (b'[{"Id":"a","Id":"b"}]', b"[NaN]"):
            with self.subTest(data=data), self.assertRaises(ValueError):
                _ = guest.inspection(data)


class RestoreOriginalIoTests(unittest.TestCase):
    """Use actual local pipes/files/children without namespace, engine or global signals."""

    def test_original_pipe_flags_restore_and_peer_eof_refuses(self) -> None:
        """A duplicated input shares flags; the borrowed original is restored explicitly."""
        incoming, outgoing = os.pipe()
        stream = os.fdopen(incoming, "rb")
        control = guest.Control(0)
        wire: guest.Wire | None = None
        before = fcntl.fcntl(incoming, fcntl.F_GETFL)
        try:
            with (
                patch.object(sys, "stdin", stream),
                patch.object(time, "monotonic", return_value=0),
            ):
                wire = guest.Wire(control)
                self.assertNotEqual(fcntl.fcntl(incoming, fcntl.F_GETFL), before)
                _ = os.write(outgoing, b'{"fixed":true}\n')
                self.assertEqual(wire.line(), b'{"fixed":true}')
                wire.no_extra()
                os.close(outgoing)
                outgoing = -1
                with self.assertRaises(ValueError):
                    wire.no_extra()
                wire.close()
                restored = fcntl.fcntl(incoming, fcntl.F_GETFL)
        finally:
            if wire is not None:
                wire.close()
            stream.close()
            if outgoing >= 0:
                os.close(outgoing)
        self.assertEqual(restored, before)
        self.assertTrue(stream.closed)

    def test_actual_child_known_seed_id_survives_late_sink_close(self) -> None:
        """Natural raw0/full CID registers before an actual later original-close failure."""
        self.actual_child_case(flush_fault=False)

    def test_actual_child_body_flush_failure_wins_over_peer_close_failure(self) -> None:
        """Two actual original sinks and all child pipes settle before the first-cause assertion."""
        self.actual_child_case(flush_fault=True)

    def actual_child_case(self, *, flush_fault: bool) -> None:
        """Keep the same mature pump and original local files under both paired faults."""
        body, close = OSError("fixture_body_flush"), OSError("fixture_sink_close")
        children: list[subprocess.Popen[bytes]] = []
        opened: list[io.BufferedRandom] = []
        real_popen = subprocess.Popen
        real_adopt = stage.adopt
        count = 0

        class FaultStream(io.BufferedRandom):
            @override
            def flush(self) -> None:
                super().flush()
                if flush_fault and self is opened[0]:
                    raise body

            @override
            def close(self) -> None:
                if self.closed:
                    return
                # Closing can invoke flush; preserve the explicit peer-close failure here.
                if flush_fault and self is opened[0]:
                    io.BufferedRandom.close(self)
                    return
                super().close()
                if self is opened[1]:
                    raise close

        def adopt(descriptor: int, mode: str) -> BinaryIO:
            nonlocal count
            count += 1
            if count <= len(("stdout", "stderr")):
                value = FaultStream(io.FileIO(descriptor, "r+"))
                opened.append(value)
                return cast("BinaryIO", value)
            return real_adopt(descriptor, mode)

        def spawn(*_arguments: object, **_options: object) -> subprocess.Popen[bytes]:
            child = real_popen(
                [sys.executable, "-I", "-B", "-c", "print('c'*64)"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            children.append(child)
            return child

        with tempfile.TemporaryDirectory(prefix="restore-keeper-child-") as directory:
            control = guest.Control(time.monotonic())
            command = guest.Commands(control, [])
            command.creating_seed = True
            with (
                patch.object(guest, "ROOT", Path(directory)),
                patch.object(stage, "adopt", side_effect=adopt),
                patch.object(subprocess, "Popen", side_effect=spawn),
                self.assertRaises(OSError) as caught,
            ):
                _ = command.run(["create", "owned-inert"])
            child_closed = all(
                child.returncode == 0
                and all(
                    stream is None or stream.closed
                    for stream in (child.stdin, child.stdout, child.stderr)
                )
                for child in children
            )
            streams_closed = all(stream.closed for stream in opened)
        self.assertIs(caught.exception, body if flush_fault else close)
        self.assertTrue(child_closed and streams_closed)
        self.assertEqual(command.created_seed, None if flush_fault else CID)


class RestoreWorkflowTests(unittest.TestCase):
    """Keep the fixture optional, finite and source-pinned without changing product policy."""

    def test_fixed_three_independent_jobs_and_failure_only_closed_upload_path(self) -> None:
        """No matrix case borrows another guest's native envelope or credential scope."""
        workflow = obj(
            yaml_value((ROOT / keeper.NEW_PATHS[0]).read_text(), scalars_as_strings=True), "jobs"
        )
        job = obj(workflow, "release-backup-runtime")
        self.assertEqual(obj(obj(job, "strategy"), "matrix"), {"scenario": sorted(guest.CASES)})
        steps = objects(job, "steps")
        toolchain = next(
            string(step, "run")
            for step in steps
            if "rustup toolchain install" in string(step.get("run", ""))
        )
        for component in ("rustfmt", "clippy", "rust-analyzer"):
            self.assertIn("--component " + component, toolchain)
        upload = next(step for step in steps if "upload-artifact@" in string(step.get("uses", "")))
        self.assertEqual(
            string(obj(upload, "with"), "path"),
            "${{ runner.temp }}/release-backup-runtime/summary.json",
        )
        self.assertEqual(string(upload, "if"), "${{ always() }}")
        self.assertNotIn("server_artifact_id", (ROOT / keeper.NEW_PATHS[0]).read_text())

    def test_source_closure_contains_all_real_migrations_and_exact_verifier(self) -> None:
        """Current SQL checksums are real source bytes, not fake catalog values."""
        self.assertEqual(len(keeper.MIGRATIONS), 36)
        self.assertIn("ops/ansible/files/restore-verify.sql", keeper.GUEST_FILES)
        self.assertTrue(all((ROOT / path).is_file() for path in keeper.SOURCE_INPUTS))
        for path in keeper.MIGRATIONS:
            self.assertEqual(guest.source_path(path)[0], guest.INPUT / path)
        with self.assertRaises(ValueError):
            _ = guest.source_path("../../PRIVATE")


class RestoreReviewRegressions(unittest.TestCase):
    """Bind review findings to closed neighbor cases rather than broad normalization."""

    def test_redundant_native_facts_must_join_inner_observation(self) -> None:
        """Neither a positive nor an expected-negative row can contradict its raw child."""
        for scenario in ("current", "privacy-invalid"):
            expected: dict[str, object] = {
                "runId": TOKEN,
                "revision": REVISION,
                "scenario": scenario,
                "sourcePins": {},
                "engineVersion": VERSION,
            }
            raw = int(scenario == "privacy-invalid")
            frame: dict[str, object] = {
                "schemaVersion": 1,
                "scope": "RestoreVerificationGuest",
                **{k: expected[k] for k in ("runId", "revision", "scenario", "sourcePins")},
                "passed": True,
                "failure": None,
                "nativeRawExit": raw,
                "nativePassed": raw == 0,
                "finalStatus": 0,
                "checkpoint": "complete",
                "cleanupPassed": True,
                "native": result(scenario),
            }
            self.assertTrue(keeper.projection(frame, expected)["passed"])
            for fields in (
                {"nativeRawExit": None, "nativePassed": None},
                {"nativeRawExit": 7, "nativePassed": False},
                {"nativePassed": raw != 0},
            ):
                with self.subTest(scenario=scenario, fields=fields), self.assertRaises(ToolError):
                    _ = keeper.projection(dict(frame, **fields), expected)


class RestoreWholeControllerTests(unittest.TestCase):
    """Run the real keeper order/original-USTAR owners with only external roles inert."""

    def test_database_fact_is_only_the_observed_ledger_and_counts(self) -> None:
        """An unobserved complete-database claim cannot enter the exact native projection."""
        value = result()
        self.assertTrue(guest.native_projection(value, "current")["sourceLedgerAndCountsUnchanged"])
        value["sourceDatabaseUnchanged"] = value.pop("sourceLedgerAndCountsUnchanged")
        with self.assertRaises(ValueError):
            _ = guest.native_projection(value, "current")

    def test_whole_controller(  # noqa: C901, PLR0915 -- Ordered real owners.
        self,
    ) -> None:
        """Actual original files/TAR/Input must survive until a current child and QEMU verdict."""
        fixture_options, transfer = ops_fixture.fixture()
        payloads: dict[str, bytes] = {}
        with tarfile.open(fileobj=io.BytesIO(transfer), mode="r:") as archive:
            for name in ("ops-release.json", "simplestchat-ops"):
                stream = archive.extractfile(name)
                self.assertIsNotNone(stream)
                if stream is not None:
                    with stream:
                        payloads[name] = stream.read()
        events: list[str] = []
        sessions: list[object] = []
        guests: list[object] = []
        held: list[fetch.Held] = []
        original_open = fetch.Held.open
        base_host = receive.Host

        def write(path: Path, data: bytes) -> None:
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                self.assertEqual(stream.write(data), len(data))
                stream.flush()

        class Host(base_host):
            @override
            def command(
                self,
                name: str,
                argv: Sequence[str],
                environment: Mapping[str, str],
                timeout: float,
                accepted: tuple[int, ...],
            ) -> tuple[int, bytes]:
                self.control.check()
                events.append(name)
                _ = argv, environment, timeout, accepted
                if name == "authenticate-ops":
                    names = [
                        "ops-evidence.json",
                        "ops-predicate.json",
                        "ops-preparation.json",
                        "ops-attestation.jsonl",
                        *[f"ops-evidence-{i:02d}.bin" for i in range(9)],
                    ]
                    for member in names:
                        write(self.output / "ops-acquired/input" / member, b"INERT-EVIDENCE\n")
                    for member, data in payloads.items():
                        write(self.output / "ops-acquired/input" / member, data)
                elif name == "measure-signed-executable":
                    return 0, guest.encoded(fixture_options.measured)
                elif name not in {"authenticate-retained-ops", "restore-fixture-setup"}:
                    message = "unexpected_inert_command"
                    raise ValueError(message)
                return 0, b""

        @final
        class Guest:
            failure: str | None = None
            closed = False

            def __init__(self, *_arguments: object, **_keywords: object) -> None:
                guests.append(self)

            def close(self) -> None:
                events.append("qemu-close")
                self.closed = True

            def require_alive(self) -> None:
                if self.closed:
                    message = "inert_guest_closed"
                    raise ValueError(message)

            def diagnostics(self) -> dict[str, object]:
                return {
                    "cleanupReturnCode": 0,
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
        class Session:
            frames: list[object]
            expected: dict[str, object]
            closed = False

            def __init__(
                self, argv: Sequence[str], environment: Mapping[str, str], control: fetch.Control
            ) -> None:
                self.frames = []
                self.expected = {}
                self.control = control
                sessions.append(self)
                self.case.assertNotIn("GH_TOKEN", environment)
                self.case.assertNotIn("FIXTURE_TOKEN", " ".join(argv))

            case: ClassVar[unittest.TestCase]

            def send(self, value: object) -> None:
                self.frames.append(value)
                events.append("request" if len(self.frames) == 1 else "go")
                if len(self.frames) == 1:
                    self.expected = guest.dictionary(value)
                else:
                    self.case.assertFalse(cast("keeper.Control", self.control).setup)
                    self.case.assertTrue(guest.dictionary(value)["go"])

            def line(self, _seconds: float) -> dict[str, object]:
                if len(self.frames) == 1:
                    return {
                        "schemaVersion": 1,
                        "scope": "RestoreVerificationReady",
                        "runId": self.expected["runId"],
                        "ready": True,
                    }
                return {
                    "schemaVersion": 1,
                    "scope": "RestoreVerificationGuest",
                    **{
                        key: self.expected[key]
                        for key in ("runId", "revision", "scenario", "sourcePins")
                    },
                    "passed": True,
                    "failure": None,
                    "nativeRawExit": 0,
                    "nativePassed": True,
                    "finalStatus": 0,
                    "checkpoint": "complete",
                    "cleanupPassed": True,
                    "native": result(),
                }

            def finish(self) -> int:
                events.append("session-finish")
                return 0

            def close(self) -> None:
                self.closed = True
                events.append("session-close")

        Session.case = self

        def opened(
            path: Path, maximum: int, control: fetch.Control, *, system: bool = False
        ) -> fetch.Held:
            value = original_open(path, maximum, control, system=system)
            held.append(value)
            return value

        def staged(
            host: ops.Host, active: vm.Guest, work: Path, port: int, options: ops_guest.Options
        ) -> None:
            _ = host, active
            _ = ops.inventory(work, port, options)

        def installed(*_arguments: object) -> dict[str, object]:
            events.append("ops33")
            return {
                "passed": True,
                "install": {"firstChanged": True, "secondChanged": False},
                "cases": [
                    {"name": name, "exitStatus": expected, "passed": True}
                    for name, expected in ops_guest.CASES
                ],
            }

        with tempfile.TemporaryDirectory(prefix="restore-whole-inert-") as temporary:
            project = Path(temporary).resolve() / "project"
            for name in keeper.SOURCE_INPUTS:
                write(project / name, (ROOT / name).read_bytes())
            for name in (
                "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
                "target/debug/xtask",
            ):
                write(project / name, b"INERT-PROGRAM-NEVER-EXECUTED\n")
            tool_root = Path(temporary).resolve() / "tools"
            for name in (*fetch.TOOL_ROLES, "ansible-playbook"):
                write(tool_root / name, b"INERT-TOOL-NEVER-EXECUTED\n")
                (tool_root / name).chmod(0o755)

            def executable_path(name: str) -> str:
                return str(tool_root / name)

            options = keeper.Options()
            options.output = Path(temporary).resolve() / "output"
            options.run_id, options.run_attempt, options.artifact_id = 123, 1, 456
            options.scenario = "current"
            control = keeper.Control()
            with (
                patch.object(keeper, "ROOT", project),
                patch.object(keeper, "admit"),
                patch.object(vm, "preflight"),
                patch.object(keeper, "executable", side_effect=executable_path),
                patch.object(receive, "Host", Host),
                patch.object(fetch.Held, "open", side_effect=opened),
                patch.object(vm, "prepare_disk"),
                patch.object(vm, "create_seed"),
                patch.object(vm, "qemu_command", return_value=["INERT"]),
                patch.object(vm, "Guest", Guest),
                patch.object(vm, "boot"),
                patch.object(vm, "recap"),
                patch.object(ops, "stage", side_effect=staged),
                patch.object(ops, "exercise", side_effect=installed),
                patch.object(receive, "Session", Session),
                patch.dict(
                    os.environ, {"GH_TOKEN": "FIXTURE_TOKEN", "GITHUB_SHA": ops_fixture.REVISION}
                ),
            ):
                observed = keeper.run(options, control)
            all_originals_closed = all(item.stream.closed for item in held)
        self.assertTrue(observed["passed"], (observed["failure"], observed["checkpoint"], events))
        self.assertTrue(observed["cleanupPassed"])
        self.assertEqual(observed["opsVerifierCalls"], 6)
        self.assertEqual(len(cast("list[object]", observed["opsCases"])), 33)
        self.assertTrue(all_originals_closed)
        self.assertTrue(all(cast("Guest", item).closed for item in guests))
        self.assertTrue(all(cast("Session", item).closed for item in sessions))
        self.assertLess(events.index("authenticate-retained-ops"), events.index("ops33"))
        self.assertLess(events.index("ops33"), events.index("request"))
        self.assertLess(events.index("go"), events.index("session-finish"))
        self.assertLess(events.index("session-close"), events.index("qemu-close"))


class RestoreFinalBoundaryTests(unittest.TestCase):
    """Exercise real admission/namespace owners around reviewed late-fault boundaries."""

    def test_dangling_snapshot_and_outcome_never_certify_absence(self) -> None:
        """Only an actual missing directory entry can support the closed absence fact."""
        control = guest.Control(time.monotonic())
        with tempfile.TemporaryDirectory(prefix="restore-absence-") as directory:
            for name in ("snapshot.dump", "outcome.json"):
                path = Path(directory) / name
                self.assertTrue(guest.absent(path, control))
                path.symlink_to("nonexistent-target")
                self.assertFalse(path.exists())
                self.assertFalse(guest.absent(path, control))
                path.unlink()
                write_input(path, b"retained")
                self.assertFalse(guest.absent(path, control))

    def test_native_late_candidate_has_no_child_and_closes_admitted_sinks(self) -> None:
        """The final1320 check runs AFTER real logs and a delayed candidate check."""
        now = 359.9
        opened: list[BinaryIO] = []
        real_adopt = stage.adopt

        def adopt(descriptor: int, mode: str) -> BinaryIO:
            value = real_adopt(descriptor, mode)
            opened.append(value)
            return value

        def late() -> None:
            nonlocal now
            now = 600

        with tempfile.TemporaryDirectory(prefix="restore-late-admission-") as directory:
            path = Path(directory)
            candidate = path / "candidate"
            write_input(candidate, b"INERT")
            with candidate.open("rb") as stream:
                control = guest.Control(0)
                control.setup = False
                command = guest.Commands(control, [])
                command.native = cast(
                    "stage.Original", cast("object", SimpleNamespace(file=stream, check=late))
                )
                with (
                    patch.object(guest, "ROOT", path),
                    patch.object(time, "monotonic", side_effect=lambda: now),
                    patch.object(stage, "adopt", side_effect=adopt),
                    patch.object(subprocess, "Popen") as spawn,
                    self.assertRaises(ValueError),
                ):
                    _ = command.run(["release", "backup", "verify"], native=True, timeout=1200)
                spawn.assert_not_called()
                all_sinks_closed = len(opened) == len(("stdout", "stderr")) and all(
                    value.closed for value in opened
                )
            self.assertTrue(stream.closed)
        self.assertTrue(all_sinks_closed)

    def test_first_io_failure_survives_cleanup_signal_or_deadline(self) -> None:
        """An actual original close and later status do not overwrite the body classification."""
        for later_status in (guest.DEADLINE_STATUS, TERMINATED):
            with (
                self.subTest(later_status=later_status),
                tempfile.TemporaryDirectory() as directory,
            ):
                self.first_io_case(later_status, Path(directory))

    def first_io_case(self, later_status: int, directory: Path) -> None:
        """Keep one pair of actual originals/closures independent of subsequent cases."""
        control = guest.Control(0)
        now = 0.0
        path = directory / "owned"
        write_input(path, b"actual-original")
        stream = path.open("rb")
        body = OSError("PRIVATE_BODY")
        cleanup = OSError("PRIVATE_CLOSE")
        source_names = {
            "build/security_vm_restore_guest.py",
            "build/security_vm_stage_guest.py",
            "ops/ansible/files/restore_verify.py",
            "ops/ansible/files/restore-verify.sql",
            "ops/ansible/files/monitoring-schema.sql",
            "ops/ansible/templates/public-runtime-grants.sql.j2",
            "migrations/001_create_users.sql",
        }
        request: dict[str, object] = {
            "schemaVersion": 1,
            "scope": "RestoreVerificationRequest",
            "runId": TOKEN,
            "revision": REVISION,
            "scenario": "current",
            "setupSeconds": 360,
            "bodySeconds": 1680,
            "sourcePins": dict.fromkeys(source_names, BINARY_SHA),
            "binary": {"sha256": BINARY_SHA, "bytes": 1},
            "postgresImage": "docker.io/library/postgres:18.6-bookworm@sha256:" + "f" * 64,
            "engineVersion": VERSION,
        }

        @final
        class Fixture:
            commands = SimpleNamespace(native_status=None)

            def __init__(self, *_arguments: object) -> None:
                pass

            def setup(self) -> None:
                control.checkpoint = "seed"
                raise body

            def finish(self) -> None:
                nonlocal now
                stream.close()
                if later_status == TERMINATED:
                    control.signal(signal.SIGTERM, None)
                else:
                    now = control.end + 0.1
                    control.check()
                raise cleanup

        wire = cast(
            "guest.Wire",
            cast(
                "object",
                SimpleNamespace(line=lambda: guest.encoded(request), no_extra=lambda: None),
            ),
        )
        with (
            patch.object(time, "monotonic", side_effect=lambda: now),
            patch.object(guest, "Fixture", Fixture),
        ):
            observed = guest.execute(control, wire)
        self.assertTrue(stream.closed)
        self.assertFalse(observed["passed"])
        self.assertEqual(observed["failure"], "io")
        self.assertEqual(observed["finalStatus"], later_status)
        self.assertNotIn("PRIVATE", json.dumps(observed))

    def test_guest_late_signal_during_umask_restore_invalidates_emitted_success(self) -> None:
        """Only mocked process-global setters run; the real Control signal latch stays live."""
        control = guest.Control(time.monotonic())
        control.settling = True
        closed: list[str] = []

        def emit(_value: object) -> None:
            pass

        wire = cast(
            "guest.Wire",
            cast("object", SimpleNamespace(emit=emit, close=lambda: closed.append("wire"))),
        )
        calls = 0

        def mask(_value: int) -> int:
            nonlocal calls
            calls += 1
            if calls > 1:
                control.signal(signal.SIGINT, None)
            return 0o022

        with (
            patch.object(guest, "Control", return_value=control),
            patch.object(guest, "Wire", return_value=wire),
            patch.object(guest, "execute", return_value={"passed": True}),
            patch.object(signal, "signal", return_value=signal.SIG_DFL),
            patch.object(os, "umask", side_effect=mask),
        ):
            status = guest.main()
        self.assertEqual(closed, ["wire"])
        self.assertEqual(status, 130)


class RestoreHistoryTests(unittest.TestCase):
    """Actual daemon-shaped rows must bind the fixed socket commands and actor order."""

    def events(self, *, negative: bool, ready_codes: tuple[int, ...] = (0,)) -> bytes:
        """Construct finite private daemon rows, including forced PG exit137."""
        connection = " -h " + reference.SOCKET + " -U postgres -d " + reference.DATABASE
        commands = [
            *[
                '/bin/sh -c test "$(cat /proc/1/comm)" = postgres && pg_isready -q' + connection
                for _ in ready_codes
            ],
            "psql -X -q -t -A --set ON_ERROR_STOP=1" + connection,
            "timeout --signal=TERM --kill-after=2s 120s "
            + "pg_restore --exit-on-error --single-transaction"
            + connection,
            "psql -X -q -t -A --set ON_ERROR_STOP=1" + connection,
            "psql -X -q -t -A --set ON_ERROR_STOP=1" + connection,
        ]
        if not negative:
            commands.append(
                "timeout --signal=TERM --kill-after=2s 120s pg_amcheck "
                + "--install-missing --heapallindexed --parent-check"
                + connection
            )
        statuses = [*ready_codes, 0, 0, 0, int(negative)] + ([] if negative else [0])
        rows: list[bytes] = []

        def row(action: str, extra: Mapping[str, object] | None = None) -> None:
            rows.append(
                guest.encoded(
                    {
                        "Type": "container",
                        "Action": action,
                        "Actor": {
                            "ID": CID,
                            "Attributes": {
                                "name": "scpub-restore-" + TOKEN,
                                "image": IMAGE,
                                **(extra or {}),
                            },
                        },
                    }
                )
            )

        row("create")
        row("start")
        for index, (command, status) in enumerate(zip(commands, statuses, strict=True), 1):
            identity = {"execID": f"{index:064x}"}
            row("exec_create: " + command, identity)
            row("exec_start: " + command, identity)
            row("exec_die", {**identity, "exitCode": str(status)})
        row("die", {"exitCode": "137"})
        row("destroy")
        return b"".join(rows)

    def check_history(self, data: bytes, *, negative: bool) -> int:
        """Use the real history decoder with only its engine transport replaced."""
        control = guest.Control(time.monotonic())
        instance = guest.Fixture(control, {}, cast("guest.Wire", object()))
        instance.native_actor = {"id": CID, "name": "scpub-restore-" + TOKEN, "image": IMAGE}
        instance.seed_image = IMAGE

        def checked(_arguments: Sequence[str]) -> bytes:
            return data

        instance.commands = cast("guest.Commands", cast("object", SimpleNamespace(checked=checked)))
        return instance.history(CID, negative=negative)

    def test_both_planned_verdicts_require_real_restore_sql_and_expected_amcheck_order(
        self,
    ) -> None:
        """Removing the running PG server may report137; this is distinct from native0."""
        for negative in (False, True):
            self.assertEqual(
                self.check_history(self.events(negative=negative), negative=negative), 1
            )

    def test_exec_creation_alone_never_proves_started_or_completed_work(self) -> None:
        """Created execution objects alone supply no restore or amcheck completion evidence."""
        rows = [ops_guest.object_json(line) for line in self.events(negative=False).splitlines()]
        data = b"".join(
            guest.encoded(row)
            for row in rows
            if not cast("str", row["Action"]).startswith(("exec_start", "exec_die"))
        )
        with self.assertRaises(ValueError):
            _ = self.check_history(data, negative=False)

    def test_exec_identity_completion_order_and_status_must_join(self) -> None:
        """A start event precedes task launch, so only the matching die supplies its status."""
        rows = [ops_guest.object_json(line) for line in self.events(negative=False).splitlines()]
        changes: list[list[dict[str, object]]] = [
            [row for row in rows if not cast("str", row["Action"]).startswith(missing)]
            for missing in ("exec_start", "exec_die")
        ]
        for field, value in (("execID", "f" * 64), ("exitCode", "1"), ("exitCode", "00")):
            changed = copy.deepcopy(rows)
            completion = next(row for row in changed if row["Action"] == "exec_die")
            guest.dictionary(guest.dictionary(completion["Actor"])["Attributes"])[field] = value
            changes.append(changed)
        for changed in changes:
            with self.assertRaises(ValueError):
                _ = self.check_history(
                    b"".join(guest.encoded(row) for row in changed), negative=False
                )
        self.assertEqual(
            self.check_history(self.events(negative=False, ready_codes=(1, 2, 0)), negative=False),
            3,
        )
        with self.assertRaises(ValueError):
            _ = self.check_history(self.events(negative=True, ready_codes=(0, 0)), negative=True)

    def test_exec_die_delivery_may_lag_another_complete_cli(self) -> None:
        """Per-exec ordering is required; delayed monitor events are not globally serialized."""
        rows = [ops_guest.object_json(line) for line in self.events(negative=False).splitlines()]
        delayed = [row for row in rows if row["Action"] == "exec_die"]
        others = [row for row in rows if row["Action"] != "exec_die"]
        data = b"".join(guest.encoded(row) for row in [*others, *delayed])
        self.assertEqual(self.check_history(data, negative=False), 1)

    def test_foreign_actor_missing_restore_and_extra_exec_never_make_history_proof(self) -> None:
        """Counts alone cannot hide a foreign identity or an unreviewed exec operation."""
        source = self.events(negative=False)
        for changed in (
            source.replace(CID.encode(), b"f" * 64),
            source.replace(b" pg_restore ", b" wrong_restore "),
            source.replace(b"psql -X", b"psql -U"),
        ):
            with self.assertRaises(ValueError):
                _ = self.check_history(changed, negative=False)


class RestoreInputAdmissionTests(unittest.TestCase):
    """No late/cancelled admission or shared-OFD flag failure loses a known input owner."""

    def test_expired_or_cancelled_wire_never_opens_a_duplicate(self) -> None:
        """Before admission, both clock and pending signal guard the first FD effect."""
        for cancelled in (False, True):
            control = guest.Control(0)
            if cancelled:
                control.signal(signal.SIGINT, None)
            with (
                patch.object(time, "monotonic", return_value=31 if not cancelled else 0),
                patch.object(os, "dup") as duplicate,
                self.assertRaises(ValueError),
            ):
                _ = guest.Wire(control)
            duplicate.assert_not_called()

    def test_late_admitted_wire_restores_flags_and_closes_duplicate_before_assertions(self) -> None:
        """A real duplicate exists before its post-admission refusal and still settles."""
        incoming, outgoing = os.pipe()
        original = os.fdopen(incoming, "rb")
        before = fcntl.fcntl(incoming, fcntl.F_GETFL)
        descriptors: list[int] = []
        real_dup = os.dup
        late = ValueError("late_input_admission")

        def duplicate(number: int) -> int:
            value = real_dup(number)
            descriptors.append(value)
            return value

        control = guest.Control(0)
        try:
            with (
                patch.object(sys, "stdin", original),
                patch.object(control, "check", side_effect=[None, late]),
                patch.object(os, "dup", side_effect=duplicate),
                self.assertRaises(ValueError) as caught,
            ):
                _ = guest.Wire(control)
            restored = fcntl.fcntl(incoming, fcntl.F_GETFL)
            closed = False
            try:
                _ = os.fstat(descriptors[0])
            except OSError:
                closed = True
        finally:
            original.close()
            os.close(outgoing)
        self.assertIs(caught.exception, late)
        self.assertEqual(restored, before)
        self.assertTrue(closed and original.closed)
