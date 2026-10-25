"""Inert whole-boundary host-deploy fixture tests; no Docker, VM or network."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import release_container_fixture as render
import release_container_harness as fixture
import security_vm_deploy as keeper
import security_vm_deploy_guest as guest
import security_vm_fetch as fetch_vm
import security_vm_ops_guest as ops_guest
import security_vm_stage_guest as stage
from security_tools import ToolError, require

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from typing import BinaryIO

    import security_vm_receive as receive

TOKEN = "1" * 32
CURRENT = "a" * 40
PREVIOUS = "b" * 40
BINARY = "c" * 64
IMAGE_A = "sha256:" + "d" * 64
IMAGE_B = "sha256:" + "e" * 64
POSTGRES_ID = "9" * 64
POSTGRES_IMAGE = "sha256:" + "a" * 64
ACTORS = [character * 64 for character in ("2", "3", "4", "5")]
VALIDATORS = 2
HANDLER_COUNT = 2
STAMP = "2026-10-24T00:00:00Z"


def frame() -> dict[str, object]:
    """Supply the exact same-owner authenticated input shape, without real artifacts."""
    return {
        "schemaVersion": 1,
        "runId": TOKEN,
        "revision": CURRENT,
        "previousRevision": PREVIOUS,
        "setupSeconds": 200,
        "bodySeconds": 4000,
        "guestSha256": BINARY,
        "sourcePins": dict.fromkeys(guest.GUEST_SOURCES, BINARY),
        "helperPins": dict.fromkeys(
            set(ops_guest.BASE | ops_guest.FETCH | ops_guest.FORCE) | {"security_vm_ops_guest.py"},
            BINARY,
        ),
        "binary": {"sha256": BINARY, "bytes": 100},
        "current": {
            name: {"sha256": BINARY, "bytes": 100} for name in ("image.tar", "release.json")
        },
        "previous": {
            name: {"sha256": BINARY, "bytes": 100} for name in ("image.tar", "release.json")
        },
    }


def prepared() -> dict[str, object]:
    """Provide passive prepared facts only, not authentication capability."""
    return {
        "schemaVersion": 1,
        "kind": "prepared",
        "runId": TOKEN,
        "revision": CURRENT,
        "previousRevision": PREVIOUS,
        "passed": True,
        "binarySha256": BINARY,
        "firstImage": IMAGE_A,
        "dependencySha256": BINARY,
        "migrationSha256": BINARY,
    }


def trial(*, negative: bool) -> dict[str, object]:
    """Describe distinct positive and negative observations, including real-exit authority."""
    return {
        "case": "owned-candidate-stop-rollback" if negative else "first-A-to-B",
        "passed": True,
        "exitStatus": 1 if negative else 0,
        "rollbackAttempted": negative,
        "rollbackPassed": negative,
        "retainedBackup": {
            "bytes": 1024,
            "sha256": BINARY,
            "listed": True,
            "restoreRehearsed": False,
        },
        "actorHistory": {
            "actorIds": ACTORS,
            "validatorCount": 2,
            "assetCaptureCount": 2,
            "allCreatedStartedAsRequiredExitedZeroAndRemoved": True,
            "historyRows": 30,
            "nativeIsolationInspected": False,
        },
        "selected": {"revision": CURRENT, "image": IMAGE_B, "appId": "6" * 64},
        "injectedCandidateId": "7" * 64 if negative else None,
        "manualStopObserved": negative,
        "dependencyProfile": "negative-inspection-response-barrier-v1"
        if negative
        else "original-docker-cli",
        "seconds": 10.0,
    }


def result() -> dict[str, object]:
    """Return a complete closed public observation for projection-fault tests."""
    return {
        "schemaVersion": 1,
        "kind": "result",
        "runId": TOKEN,
        "revision": CURRENT,
        "previousRevision": PREVIOUS,
        "passed": True,
        "cleanupPassed": True,
        "binarySha256": BINARY,
        "firstImage": IMAGE_A,
        "currentImage": IMAGE_B,
        "trials": [trial(negative=False), trial(negative=True)],
        "dependenciesUnchanged": True,
        "markerUnchanged": True,
        "seedAggregateUnchanged": True,
        "schemaUnchanged": True,
        "currentSelected": True,
        "actorHistoryComplete": True,
        "ownedInjectorStoppedCandidate": True,
        "nativeValidatorIsolationIndependentlyInspected": False,
        "phaseSeconds": {"prepare": 20.0, "positive": 30.0, "negative": 40.0},
        "separateProbe": {
            "passed": True,
            "isolationIndependentlyInspected": True,
            "migrationMapMatched": True,
            "exitStatus": 0,
            "containerId": "8" * 64,
        },
    }


def actor_binding() -> guest.ActorBinding:
    """Keep a preheld dependency distinct from signed server-image graphs."""
    return guest.ActorBinding(
        {"current": ({IMAGE_A}, {}), "previous": ({IMAGE_A}, {})},
        POSTGRES_ID,
        POSTGRES_IMAGE,
        TOKEN,
    )


def events(*, missing: str | None = None, wrong_image: bool = False) -> bytes:
    """Use actual event-shaped full actor IDs and image attributes, never an engine."""
    rows: list[str] = [
        json.dumps(
            {
                "Type": "container",
                "Action": action,
                "Actor": {
                    "ID": POSTGRES_ID,
                    "Attributes": {
                        "name": "simplestchat-public-postgres-1",
                        "image": fixture.POSTGRES,
                        "com.docker.compose.project": "simplestchat-public",
                        "com.docker.compose.service": "postgres",
                        render.LABEL: TOKEN,
                    },
                },
            }
        )
        for action in ("exec_create: psql -X", "exec_start: psql -X", "exec_die")
    ]
    for index, identifier in enumerate(ACTORS):
        kind = "validate" if index < VALIDATORS else "assets"
        name = "scpub-release-" + kind + "-" + str(index + 2) * 32
        actions = (
            ("create", "start", "die", "destroy") if index < VALIDATORS else ("create", "destroy")
        )
        for action in actions:
            if action == missing:
                continue
            attributes = {"name": name, "image": IMAGE_B if wrong_image else IMAGE_A}
            if action == "die":
                attributes["exitCode"] = "0"
            rows.append(
                json.dumps(
                    {
                        "Type": "container",
                        "Action": action,
                        "Actor": {"ID": identifier, "Attributes": attributes},
                    }
                )
            )
    return ("\n".join(rows) + "\n").encode()


@final
class ObservedFile:
    """Retain actual own-temp output streams under a test UID mapping only."""

    def __init__(self, path: Path) -> None:
        """Own the original stream before gathering any assertion facts."""
        self.path = path
        self.file = path.open("rb")
        self.closed = False
        try:
            self.before = os.fstat(self.file.fileno())
            self.sha256 = hashlib.sha256(self.file.read()).hexdigest()
            _ = self.file.seek(0)
        except BaseException as error:
            first = ops_guest.settle((self.close,), error)
            if first is not None:
                raise first from None
            raise

    def data(self, _maximum: int = guest.MAX_RECORD) -> bytes:
        """Read the same original output descriptor."""
        _ = self.file.seek(0)
        return self.file.read()

    def close(self) -> None:
        """Explicitly close once before any test assertion."""
        self.file.close()
        self.closed = True


def failure_facts() -> dict[str, object]:
    """Make a closed passive failure row without executing or authenticating anything."""
    control = guest.Control(100.0)
    commands = guest.Commands(control, cast("guest.Wire", cast("object", SimpleNamespace())), [])
    commands.sequence, commands.last_status, commands.last_role = 7, 1, "native-deploy"
    commands.native_observation = guest.native_facts(
        json.dumps(
            {
                "action": "deploy",
                "passed": False,
                "phase": "preflight",
                "failure": "RuntimeProfileError",
            }
        ).encode(),
        1,
    )
    owner = cast(
        "guest.Fixture",
        cast(
            "object",
            SimpleNamespace(
                commands=commands,
                value={"runId": TOKEN},
                checkpoint="positive-native",
                trials=[],
            ),
        ),
    )
    return guest.failure_snapshot("exercise", owner, ValueError("private-not-exported"), control)


class DeployProofTests(unittest.TestCase):
    """Closed scalar profiles and real owner finalizers refuse concrete counterexamples."""

    def test_native_failure_projection_is_closed_and_never_echoes_unknown_text(self) -> None:
        """Reported semantic fields remain passive and distinct from the real signed exit."""
        private_marker = "INERT_PRIVATE_URL_OR_CONFIG_MUST_NOT_ESCAPE"
        positive = guest.native_facts(
            json.dumps(
                {
                    "action": "deploy",
                    "passed": True,
                    "phase": "complete",
                    "private": private_marker,
                }
            ).encode(),
            1,
        )
        self.assertEqual(positive["nativeRawExit"], 1)
        self.assertIs(positive["nativePassed"], expr2=True)
        self.assertEqual(positive["nativePhase"], "complete")
        for failure, expected in (
            ("RuntimeProfileError", "runtime-profile"),
            ("OSError", "filesystem"),
            ("Command 031 failed; inspect private output", "native-command"),
            (private_marker, "other-refusal"),
        ):
            observed = guest.native_facts(
                json.dumps(
                    {
                        "action": "deploy",
                        "passed": False,
                        "phase": "stage",
                        "failure": failure,
                        "private": {private_marker: private_marker},
                    }
                ).encode(),
                1,
            )
            self.assertEqual(observed["nativeFailureLabel"], expected)
            self.assertEqual(observed["nativePhase"], "stage")
            self.assertNotIn(private_marker, json.dumps(observed))
        malformed = guest.native_facts(b'{"action":"deploy","action":"secret"}', 7)
        self.assertEqual(malformed["nativeReportState"], "invalid")
        self.assertEqual(malformed["nativeRawExit"], 7)

    def test_failure_decoder_rejects_hostile_shapes_and_unknown_fields(self) -> None:
        """Neither malformed keys, statuses nor nested secret objects reach the summary."""
        original = failure_facts()
        self.assertEqual(guest.failure_projection(original, TOKEN), original)
        for key, value in (
            ("phase", {"INERT_SECRET": "x"}),
            ("checkpoint", ["INERT_SECRET"]),
            ("firstFailureKind", {"INERT_SECRET": 1}),
            ("lastObservedRole", ["INERT_SECRET"]),
            ("nativeFailureLabel", {"INERT_SECRET": 1}),
            ("nativeRawExit", True),
            ("nativePassed", "true"),
            ("lastObservedOrdinal", True),
            ("completedTrials", 3),
            ("guestSignalStatus", True),
            ("nativeCommandOrdinal", 999),
            ("runId", "b" * 32),
        ):
            with self.subTest(key=key):
                candidate = dict(original)
                candidate[key] = value
                with self.assertRaises(ValueError):
                    _ = guest.failure_projection(candidate, TOKEN)
        extra = dict(original)
        extra["INERT_SECRET"] = {"value": "INERT_SECRET"}
        with self.assertRaises(ValueError):
            _ = guest.failure_projection(extra, TOKEN)

    def test_actual_owned_native_shape_child_exit_is_captured_before_status_rejection(self) -> None:
        """The real adapter snapshots raw7/complete JSON before raising, then closes all pipes."""
        children: list[subprocess.Popen[bytes]] = []
        original_spawn = subprocess.Popen
        native_json = json.dumps(
            {
                "action": "deploy",
                "passed": False,
                "phase": "preflight",
                "failure": "RuntimeProfileError",
            }
        )

        def spawn(  # noqa: PLR0913 -- Match the existing Popen boundary for this inert child only.
            _args: list[str],
            *,
            executable: str | None,
            pass_fds: tuple[int, ...],
            stdin: BinaryIO | int,
            stdout: int,
            stderr: int,
            env: Mapping[str, str],
            cwd: Path,
            start_new_session: bool,
        ) -> subprocess.Popen[bytes]:
            del executable
            child = original_spawn(
                [
                    sys.executable,
                    "-I",
                    "-B",
                    "-c",
                    "import sys;sys.stdout.write(" + repr(native_json) + ");sys.exit(7)",
                ],
                stdin=stdin,
                stdout=stdout,
                stderr=stderr,
                env=dict(env),
                cwd=cwd,
                pass_fds=pass_fds,
                start_new_session=start_new_session,
            )
            children.append(child)
            return child

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(guest, "ROOT", Path(temporary)),
        ):
            source_path = Path(temporary) / "inert-source"
            _ = source_path.write_bytes(b"not-a-native-binary")
            with source_path.open("rb") as source:
                pinned = cast(
                    "stage.Original",
                    cast(
                        "object",
                        SimpleNamespace(
                            file=source,
                            check=lambda: None,
                        ),
                    ),
                )
                control = guest.Control(time.monotonic())
                control.phase = "exercise"
                wire = cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None)))
                commands = guest.Commands(control, wire, [])
                commands.native = pinned
                with (
                    patch.object(subprocess, "Popen", side_effect=spawn),
                    patch.object(bounded_process, "stop", wraps=bounded_process.stop) as stop,
                    self.assertRaises(ValueError),
                ):
                    _ = commands.run(
                        ["simplestchat-ops", "release", "deploy", "--revision", CURRENT]
                    )
                observed = dict(commands.native_observation)
                facts = [
                    (
                        child.returncode,
                        child.stdout.closed if child.stdout else False,
                        child.stderr.closed if child.stderr else False,
                    )
                    for child in children
                ]
                stop_calls = stop.call_count
            source_closed = source.closed
        self.assertEqual(facts, [(7, True, True)])
        self.assertTrue(source_closed)
        self.assertEqual(stop_calls, 0)
        self.assertEqual(observed["nativeRawExit"], 7)
        self.assertIs(observed["nativePassed"], expr2=False)
        self.assertEqual(observed["nativePhase"], "preflight")
        self.assertEqual(observed["nativeFailureLabel"], "runtime-profile")

    def test_new_native_attempt_cannot_inherit_the_previous_trial_observation(self) -> None:
        """An early refused second attempt keeps unknown facts, not old raw0/pass/complete."""
        control = guest.Control(time.monotonic())
        wire = cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None)))
        commands = guest.Commands(control, wire, [])
        commands.native_observation = guest.native_facts(
            json.dumps(
                {
                    "action": "deploy",
                    "passed": True,
                    "phase": "complete",
                }
            ).encode(),
            0,
        )
        primary = ValueError("inert-pre-spawn")
        with (
            patch.object(commands, "check", side_effect=primary),
            self.assertRaises(ValueError) as got,
        ):
            _ = commands.run(["simplestchat-ops", "release", "deploy", "--revision", PREVIOUS])
        self.assertIs(got.exception, primary)
        self.assertEqual(commands.native_observation, guest.native_facts(None, None))

    def test_fixed_failure_reader_checks_marker_and_never_selects_a_stream(self) -> None:
        """Emit only the exact marker-bound record, never raw command evidence."""
        requested: list[tuple[Path, int, int]] = []
        row = failure_facts()

        def read(path: Path, maximum: int, mode: int) -> tuple[bytes, os.stat_result]:
            requested.append((path, maximum, mode))
            data = (
                (TOKEN + "\n").encode()
                if path.name == "simplestchat-vm-fixture"
                else json.dumps(row).encode()
            )
            return data, os.stat_result((0,) * 10)

        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(ops_guest, "read_fixed", side_effect=read),
        ):
            observed = guest.failure_diagnostic(TOKEN)
        self.assertEqual(guest.record(observed), row)
        self.assertEqual(
            requested,
            [
                (Path("/etc/simplestchat-vm-fixture"), 128, 0o600),
                (guest.ROOT / "deploy-observer-diagnostic.json", guest.MAX_FRAME, 0o600),
            ],
        )
        with (
            patch.object(os, "geteuid", return_value=0),
            patch.object(ops_guest, "read_fixed") as reading,
            self.assertRaises(ValueError),
        ):
            _ = guest.failure_diagnostic("../private-path")
        reading.assert_not_called()

    def test_failure_snapshot_precedes_cleanup_counter_and_status_overwrites(self) -> None:
        """Actual execute preserves the body's semantic anchor while every owned peer closes."""
        self._exercise_failure_boundary(late=False)

    def test_guest_exact_body_end_cannot_become_cleanup_window_success(self) -> None:
        """Actual execute refuses a previously complete body at exact B and still closes peers."""
        self._exercise_failure_boundary(late=True)

    def _exercise_failure_boundary(self, *, late: bool) -> None:
        clock = [100.0]
        control = guest.Control(clock[0])
        primary = ValueError("private-body-error")
        native_data = json.dumps(
            {
                "action": "deploy",
                "passed": False,
                "phase": "preflight",
                "failure": "RuntimeProfileError",
            }
        ).encode()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wire_file = tempfile.TemporaryFile()  # noqa: SIM115 -- Observe actual operation close.
            fixture_file = tempfile.TemporaryFile(  # noqa: SIM115 -- Observe operation close.
            )
            incoming = iter((frame(), {"schemaVersion": 1, "kind": "continue", "runId": TOKEN}))
            wire = cast(
                "guest.Wire",
                cast(
                    "object",
                    SimpleNamespace(
                        read=lambda: next(incoming),
                        no_extra=lambda: None,
                        close=wire_file.close,
                    ),
                ),
            )
            commands = guest.Commands(control, wire, [])
            commands.sequence, commands.last_status, commands.last_role = 7, 1, "native-deploy"
            commands.native_observation = guest.native_facts(native_data, 1)
            owner = object.__new__(guest.Fixture)
            owner.value, owner.control, owner.wire = frame(), control, wire
            owner.commands, owner.checkpoint, owner.trials = commands, "positive-native", []

            def exercise() -> dict[str, object]:
                control.advance()
                if late:
                    clock[0] = control.body_end
                    return result()
                raise primary

            def finish(first: BaseException | None) -> BaseException | None:
                commands.sequence, commands.last_status, commands.last_role = 99, 0, "trust-update"
                fixture_file.close()
                return first

            caught: BaseException | None = None
            with (
                patch.object(guest, "ROOT", root),
                patch.object(time, "monotonic", side_effect=lambda: clock[0]),
                patch.object(guest, "Fixture", return_value=owner),
                patch.object(owner, "prepare", return_value=prepared()),
                patch.object(owner, "exercise", side_effect=exercise),
                patch.object(owner, "finish", side_effect=finish),
                patch.object(sys, "stdout", SimpleNamespace(buffer=io.BytesIO())),
            ):
                try:
                    _ = guest.execute(control, wire)
                except ValueError as error:
                    caught = error
                finally:
                    # Gather real close facts before test fallback, then settle every known peer.
                    closed = (fixture_file.closed, wire_file.closed)
                    _ = ops_guest.settle((fixture_file.close, wire_file.close))
            diagnostic = guest.record((root / "deploy-observer-diagnostic.json").read_bytes())
        self.assertEqual(closed, (True, True))
        self.assertEqual(guest.failure_projection(diagnostic, TOKEN), diagnostic)
        self.assertTrue(control.settling)
        self.assertIsNotNone(caught)
        if not late:
            self.assertIs(caught, primary)
        self.assertEqual(diagnostic["checkpoint"], "body-completion" if late else "positive-native")
        self.assertEqual(diagnostic["lastObservedOrdinal"], 7)
        self.assertEqual(diagnostic["lastObservedExit"], 1)
        self.assertEqual(diagnostic["nativeRawExit"], 1)
        self.assertEqual(diagnostic["nativeFailureLabel"], "runtime-profile")
        self.assertNotIn("private-body-error", json.dumps(diagnostic))

    def test_guest_setup_final_fence_precedes_widening_after_real_debt_observation(self) -> None:
        """An elapsed original setup boundary cannot be hidden by changing the active phase."""
        control = guest.Control(100.0)
        clock = [100.0]
        with patch.object(time, "monotonic", side_effect=lambda: clock[0]):
            control.configure(frame())
            original = control.end
            admit = control.exercise_budget

            def observed() -> None:
                admit()
                clock[0] = control.setup_end

            with (
                patch.object(control, "exercise_budget", side_effect=observed),
                self.assertRaises(ValueError),
            ):
                control.advance()
        self.assertEqual(control.phase, "setup")
        self.assertEqual(control.end, original)

    def test_controller_body_fence_precedes_cleanup_and_preserves_known_input_close(self) -> None:
        """Even a prior body refusal cannot skip the original B check or known finalizers."""
        control = keeper.Control(started=100.0)
        control.setup = False
        modes: list[bool] = []
        original = control.check

        def checking() -> None:
            modes.append(control.settling)
            original()

        def check_input(_control: fetch_vm.Control) -> None:
            pass

        with tempfile.TemporaryFile() as stream:
            held = cast(
                "fetch_vm.Held",
                cast(
                    "object",
                    SimpleNamespace(
                        stream=stream,
                        check=check_input,
                        close=stream.close,
                    ),
                ),
            )
            owners = fetch_vm.Owners(inputs=[held])
            with (
                patch.object(keeper, "admit", side_effect=ToolError("inert-primary")),
                patch.object(fetch_vm, "Owners", return_value=owners),
                patch.object(control, "check", side_effect=checking),
                patch.object(time, "monotonic", return_value=control.body_end),
            ):
                observed = keeper.run(keeper.Options(), control)
                closed = stream.closed
        self.assertIs(modes[0], expr2=False)
        self.assertIn(member=True, container=modes)
        self.assertTrue(closed)
        self.assertIs(observed["passed"], expr2=False)
        self.assertEqual(observed["failure"], "guard")

    def test_original_fixture_setup_does_not_reset_clock(self) -> None:
        """The release boundary keeps the exact original end and both full calls."""
        with patch.object(time, "monotonic", return_value=100.0):
            control = keeper.Control(started=100.0)
        original = control.end
        with patch.object(time, "monotonic", return_value=200.0):
            control.release_setup()
            observed = (control.end, control.body_end)
        self.assertEqual(observed, (original, original - keeper.FINALIZATION_SECONDS))
        with (
            patch.object(time, "monotonic", return_value=701.0),
            self.assertRaises(ToolError),
        ):
            control.release_setup()

    def test_late_setup_cannot_admit_second_full_native_call(self) -> None:
        """Explicit entry epochs make strict setup refusal independent of host uptime."""
        for epoch in (0.0, 100.0, 10_000.0):
            with self.subTest(epoch=epoch):
                inside = keeper.Control(started=epoch)
                original_end = inside.end
                with patch.object(time, "monotonic", return_value=epoch + keeper.SETUP_SECONDS - 1):
                    inside.release_setup()
                self.assertFalse(inside.setup)
                self.assertEqual(inside.end, original_end)
                self.assertEqual(inside.body_end, original_end - keeper.FINALIZATION_SECONDS)

                boundary = keeper.Control(started=epoch)
                with (
                    patch.object(time, "monotonic", return_value=epoch + keeper.SETUP_SECONDS),
                    self.assertRaisesRegex(ToolError, "^fetch_vm_deadline$"),
                ):
                    boundary.release_setup()
                self.assertTrue(boundary.setup)
                self.assertEqual(boundary.started, epoch)
                self.assertEqual(boundary.end, original_end)

    def test_setup_final_fence_retains_phase_after_budget_observation(self) -> None:
        """A passed debt check cannot grant setup after its original strict ceiling."""

        def scenario(epoch: float) -> None:
            control = keeper.Control(started=epoch)
            original_end = control.end
            clock = [epoch + keeper.SETUP_SECONDS - 1]
            budget_observations: list[float] = []

            def after_budget(value: object, code: str) -> None:
                require(value, code)
                if code == "deploy_vm_two_full_envelopes":
                    budget_observations.append(control.whole_body_end - clock[0])
                    clock[0] = epoch + keeper.SETUP_SECONDS + 0.1

            with (
                patch.object(time, "monotonic", side_effect=lambda: clock[0]),
                patch.object(keeper, "require", side_effect=after_budget),
                self.assertRaisesRegex(ToolError, "^fetch_vm_deadline$"),
            ):
                control.release_setup()
            self.assertEqual(budget_observations, [keeper.CONTINUE_MINIMUM + 1])
            self.assertGreater(clock[0], control.setup_end)
            self.assertLess(control.whole_body_end - clock[0], keeper.CONTINUE_MINIMUM)
            self.assertTrue(control.setup)
            self.assertEqual(control.started, epoch)
            self.assertEqual(control.end, original_end)

        for epoch in (0.0, 100.0, 10_000.0):
            with self.subTest(epoch=epoch):
                scenario(epoch)

    def test_guest_continue_has_no_second_clock_or_replay(self) -> None:
        """Guest durations are subordinate to one entry epoch, never host instants."""
        with patch.object(time, "monotonic", return_value=10.0):
            control = guest.Control(10.0)
            control.configure(frame())
            before = (control.started, control.body, control.end)
            control.advance()
            with self.assertRaises(ValueError):
                control.advance()
        self.assertEqual(before, (control.started, control.body, control.end))

    def test_guest_expired_or_cancelled_before_selector_admission(self) -> None:
        """Neither cancellation nor an expired owner may acquire a new selector."""
        for cancelled in (False, True):
            control = guest.Control(0.0)
            control.status = 130 if cancelled else 0
            with (
                patch.object(time, "monotonic", return_value=0.0 if cancelled else 31.0),
                patch.object(selectors, "DefaultSelector") as opening,
                self.assertRaises(ValueError),
            ):
                _ = guest.Wire(control)
            opening.assert_not_called()

    def test_request_rejects_unknown_role_or_mismatched_revisions(self) -> None:
        """No arbitrary remote command, source role or same-image trial is admitted."""
        self.assertEqual(guest.request(frame()), frame())
        for change in (
            {"command": "PRIVATE"},
            {"previousRevision": CURRENT},
            {"bodySeconds": True},
        ):
            with self.assertRaises(ValueError):
                _ = guest.request(dict(frame(), **change))

    def test_prepared_projection_is_closed_and_not_final_authority(self) -> None:
        """Wrong binary and unknown strings refuse before continuation."""
        self.assertEqual(keeper.prepared_projection(prepared(), frame()), prepared())
        for change in ({"binarySha256": "0" * 64}, {"token": "PRIVATE"}, {"passed": False}):
            with self.assertRaises((ValueError, ToolError)):
                _ = keeper.prepared_projection(dict(prepared(), **change), frame())

    def test_public_projection_rejects_top_and_nested_secret_fields(self) -> None:
        """A trusted script's arbitrary dictionary cannot reach uploaded evidence."""
        self.assertEqual(
            guest.public_result(
                guest.record(json.dumps(result(), indent=2).encode()), frame(), prepared()
            ),
            result(),
        )
        variants = [dict(result(), opaque="PRIVATE")]
        nested = copy.deepcopy(result())
        cast("dict[str, object]", cast("list[object]", nested["trials"])[0])["opaque"] = "PRIVATE"
        variants.append(nested)
        for candidate in variants:
            with self.assertRaises(ValueError):
                _ = guest.public_result(candidate, frame(), prepared())

    def test_missing_injection_cannot_be_relabelled_as_rollback(self) -> None:
        """Native nonzero alone does not prove a deliberately stopped candidate."""
        for key, value in (
            ("manualStopObserved", False),
            ("exitStatus", 0),
            ("rollbackPassed", False),
            ("injectedCandidateId", None),
        ):
            changed = dict(trial(negative=True), **{key: value})
            with self.assertRaises(ValueError):
                _ = guest.public_trial(changed, CURRENT, negative=True)

    def test_no_native_isolation_or_restore_claim_from_other_observations(self) -> None:
        """Actual actor lifetime and dump list retain their explicit proof limits."""
        changed = dict(result(), nativeValidatorIsolationIndependentlyInspected=True)
        with self.assertRaises(ValueError):
            _ = guest.public_result(changed, frame(), prepared())
        changed_trial = trial(negative=False)
        cast("dict[str, object]", changed_trial["retainedBackup"])["restoreRehearsed"] = True
        with self.assertRaises(ValueError):
            _ = guest.public_trial(changed_trial, CURRENT, negative=False)

    def test_actor_history_uses_real_outputs_without_static_ordinals(self) -> None:
        """Real bulk roles omit stdout while stderr inventories every shifted command."""
        owners: list[ObservedFile] = []
        observed: dict[str, object] = {}
        missing_refused = False
        with tempfile.TemporaryDirectory() as temporary:
            attempt = Path(temporary)
            for ordinal in range(1, 13):
                identifier = {2: ACTORS[0], 5: ACTORS[1], 9: ACTORS[2], 12: ACTORS[3]}.get(
                    ordinal, "0"
                )
                _ = (attempt / f"{ordinal:03d}.stderr").write_bytes(b"")
                if ordinal not in {3, 6}:
                    _ = (attempt / f"{ordinal:03d}.stdout").write_text(identifier + "\n")
            _ = (attempt / "validation-name.txt").write_text("scpub-release-validate-" + "3" * 32)
            _ = (attempt / "asset-container-name.txt").write_text(
                "scpub-release-assets-" + "5" * 32
            )

            def retain_output(
                path: Path,
                _cap: int,
                _mode: int,
                _control: guest.Control,
                _originals: list[stage.Original],
            ) -> stage.Original:
                item = ObservedFile(path)
                owners.append(item)
                return cast("stage.Original", cast("object", item))

            try:
                with patch.object(guest, "retain", side_effect=retain_output):
                    observed = guest.actor_history(
                        events(),
                        attempt,
                        actor_binding(),
                        guest.Control(time.monotonic()),
                        [],
                    )
                    (attempt / "005.stderr").unlink()
                    try:
                        _ = guest.native_ids(attempt, guest.Control(time.monotonic()), [])
                    except ValueError:
                        missing_refused = True
            finally:
                for owner in owners:
                    owner.close()
            facts = [owner.closed and owner.file.closed for owner in owners]
        self.assertTrue(all(facts))
        self.assertEqual(observed["actorIds"], ACTORS)
        self.assertFalse(observed["nativeIsolationInspected"])
        self.assertTrue(missing_refused)

    def test_actor_history_refuses_missing_anchor_and_unbound_image(self) -> None:
        """Neither incomplete ring history nor a profile-only image is sufficient."""
        control = guest.Control(time.monotonic())
        for data in (
            events(missing="create"),
            events(wrong_image=True),
            events().replace(b"exec_die", b"die"),
            events().replace(POSTGRES_ID.encode(), ("8" * 64).encode()),
            events().replace(TOKEN.encode(), ("f" * 32).encode()),
            b"\n" * guest.HISTORY_LIMIT,
        ):
            with (
                patch.object(guest, "native_ids", return_value=set(ACTORS)),
                self.assertRaises(ValueError),
            ):
                _ = guest.actor_history(
                    data,
                    Path("/inert"),
                    actor_binding(),
                    control,
                    [],
                )

    def test_injected_event_proof_requires_exact_signed_actor(self) -> None:
        """A missing manual stop or wrong signed image cannot establish effect."""
        rows: list[str] = [
            json.dumps(
                {
                    "Type": "container",
                    "Action": action,
                    "Actor": {
                        "ID": ACTORS[0],
                        "Attributes": {"image": IMAGE_A, render.LABEL: TOKEN},
                    },
                }
            )
            for action in ("create", "start", "die", "stop", "destroy")
        ]
        data = ("\n".join(rows) + "\n").encode()
        guest.injected_sequence(data, ACTORS[0], IMAGE_A, TOKEN)
        for changed in (
            data.replace(b'"stop"', b'"inspect"'),
            data.replace(IMAGE_A.encode(), IMAGE_B.encode()),
        ):
            with self.assertRaises(ValueError):
                guest.injected_sequence(changed, ACTORS[0], IMAGE_A, TOKEN)

    def test_actual_inert_command_drains_closes_and_preserves_signed_status(self) -> None:
        """A real owned no-network child settles both pipes before assertions."""
        children: list[subprocess.Popen[bytes]] = []
        popen = subprocess.Popen[bytes]

        def admitted(  # noqa: PLR0913 -- Exact real Popen role boundary.
            args: Sequence[str],
            *,
            executable: str | None,
            pass_fds: tuple[int, ...],
            stdin: BinaryIO | int,
            stdout: int,
            stderr: int,
            env: Mapping[str, str],
            cwd: Path,
            start_new_session: bool,
        ) -> subprocess.Popen[bytes]:
            child = popen(
                args,
                executable=executable,
                pass_fds=pass_fds,
                stdin=stdin,
                stdout=stdout,
                stderr=stderr,
                env=env,
                cwd=cwd,
                start_new_session=start_new_session,
            )
            children.append(child)
            return child

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(guest, "ROOT", Path(temporary)),
        ):
            control = guest.Control(time.monotonic())
            control.phase = "exercise"
            wire = cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None)))
            runner = guest.Commands(control, wire, [])
            with patch.object(subprocess, "Popen", side_effect=admitted):
                observed = runner.run(
                    [
                        sys.executable,
                        "-I",
                        "-B",
                        "-c",
                        "import sys;sys.stdout.buffer.write(b'bounded');sys.stderr.write('quiet')",
                    ]
                )
            facts = [
                (
                    child.returncode,
                    child.stdout.closed if child.stdout else False,
                    child.stderr.closed if child.stderr else False,
                )
                for child in children
            ]
        self.assertEqual(observed.code, 0)
        self.assertEqual(observed.data, b"bounded")
        self.assertEqual(facts, [(0, True, True)])

    def test_real_body_refusal_survives_both_log_close_faults(  # noqa: C901 -- Owned peer faults.
        self,
    ) -> None:
        """An observed failure wins while both actual logs and all child pipes close."""
        logs: list[BinaryIO] = []
        children: list[subprocess.Popen[bytes]] = []
        primary = ValueError("fixed_inert_body")
        original_pump = bounded_process.pump

        @final
        class CloseFault:
            """Keep a real stream and raise only after its descriptor has closed."""

            def __init__(self, stream: BinaryIO) -> None:
                self.stream = stream

            @property
            def closed(self) -> bool:
                return self.stream.closed

            def write(self, data: bytes) -> int:
                return self.stream.write(data)

            def flush(self) -> None:
                self.stream.flush()

            def read(self, size: int = -1) -> bytes:
                return self.stream.read(size)

            def seek(self, offset: int, whence: int = 0) -> int:
                return self.stream.seek(offset, whence)

            def close(self) -> None:
                was_open = not self.closed
                self.stream.close()
                if was_open:
                    message = "fixed_inert_close"
                    raise OSError(message)

        def admit(fd: int, _mode: str) -> BinaryIO:
            raw = cast("BinaryIO", os.fdopen(fd, mode="w+b"))
            stream = cast("BinaryIO", cast("object", CloseFault(raw)))
            logs.append(stream)
            return stream

        def observe(
            child: subprocess.Popen[bytes],
            output: BinaryIO,
            error: BinaryIO,
            limits: bounded_process.Limits,
        ) -> int:
            children.append(child)
            _ = original_pump(child, output, error, limits)
            raise primary

        caught: BaseException | None = None
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(guest, "ROOT", Path(temporary)),
        ):
            control = guest.Control(time.monotonic())
            control.phase = "exercise"
            wire = cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None)))
            runner = guest.Commands(control, wire, [])
            try:
                with (
                    patch.object(stage, "adopt", side_effect=admit),
                    patch.object(bounded_process, "pump", side_effect=observe),
                ):
                    _ = runner.run([sys.executable, "-I", "-B", "-c", "pass"])
            except ValueError as error:
                caught = error
            facts = [
                (
                    child.returncode,
                    child.stdout.closed if child.stdout else False,
                    child.stderr.closed if child.stderr else False,
                )
                for child in children
            ]
            closed = [stream.closed for stream in logs]
        self.assertIs(caught, primary)
        self.assertEqual(closed, [True, True])
        self.assertEqual(facts, [(0, True, True)])

    def test_old_a_acquisition_does_not_borrow_current_checkout_identity(self) -> None:
        """The real adapter builds explicit old selector argv before any fake observation."""
        calls: list[Sequence[str]] = []
        marker = ToolError("expected_inert_boundary")

        def command(
            _label: str,
            argv: Sequence[str],
            _env: Mapping[str, str],
            _seconds: int,
            _accepted: tuple[int, ...],
        ) -> tuple[int, bytes]:
            calls.append(argv)
            raise marker

        control = keeper.Control()
        host = cast(
            "receive.Host", cast("object", SimpleNamespace(control=control, command=command))
        )

        def check(_control: fetch_vm.Control) -> None:
            pass

        tools = cast("fetch_vm.NativeTools", cast("object", SimpleNamespace(check=check)))
        options = keeper.Options(
            output=Path("/inert"),
            previous_revision=PREVIOUS,
            previous_run_id=123,
            previous_run_attempt=2,
            previous_artifact_id=456,
        )
        with self.assertRaises(ToolError) as caught:
            _ = keeper.acquire_previous(
                host, tools, Path("/inert/native"), fetch_vm.Owners(), [], options, {}
            )
        self.assertIs(caught.exception, marker)
        self.assertEqual(calls[0][1:5], ["release", "verify", "--repository", fetch_vm.REPOSITORY])
        self.assertIn(PREVIOUS, calls[0])
        self.assertNotIn(CURRENT, calls[0])
        self.assertNotIn("--artifact-dir", calls[0])

    def test_controller_late_restore_signal_remains_negative(self) -> None:
        """Actual main return adopts a signal latched during final handler restoration."""
        restore_count = 0
        control = keeper.Control()

        def install(_number: int, _handler: object) -> object:
            nonlocal restore_count
            restore_count += 1
            if restore_count > HANDLER_COUNT:
                control.signal(signal.SIGINT, None)
            return signal.SIG_DFL

        observed = {"passed": True}
        argv = [
            "keeper",
            "--run-id",
            "1",
            "--run-attempt",
            "1",
            "--artifact-id",
            "2",
            "--server-artifact-id",
            "3",
            "--previous-revision",
            PREVIOUS,
            "--previous-run-id",
            "4",
            "--previous-run-attempt",
            "1",
            "--previous-artifact-id",
            "5",
            "--output",
            "/inert",
        ]
        with (
            patch.object(keeper, "Control", return_value=control),
            patch.object(keeper, "run", return_value=observed),
            patch.object(keeper, "write_private"),
            patch.object(signal, "signal", side_effect=install),
            patch.object(os, "umask", return_value=0o077),
            patch.object(sys, "stdout", SimpleNamespace(buffer=io.BytesIO())),
            patch.object(sys, "argv", argv),
        ):
            status = keeper.main()
        self.assertEqual(status, 130)

    def test_actual_two_frame_pipe_retains_owner_and_refuses_replay(self) -> None:
        """Use one real pipe/selector, keeping the input open across both phases."""
        read_fd, write_fd = os.pipe()
        source = os.fdopen(read_fd, "rb")
        wire: guest.Wire | None = None
        control = guest.Control(time.monotonic())
        observed: list[dict[str, object]] = []
        rejected = False
        try:
            with patch.object(sys, "stdin", source):
                wire = guest.Wire(control)
                _ = os.write(write_fd, guest.encoded(frame()))
                observed.append(wire.read())
                wire.no_extra()
                continuation = {"schemaVersion": 1, "kind": "continue", "runId": TOKEN}
                _ = os.write(write_fd, guest.encoded(continuation))
                observed.append(wire.read())
                wire.no_extra()
                _ = os.write(write_fd, guest.encoded(continuation))
                try:
                    wire.no_extra()
                except ValueError:
                    rejected = True
        finally:
            actions = [source.close, lambda: os.close(write_fd)]
            if wire is not None:
                actions.insert(0, wire.close)
            cleanup = ops_guest.settle(actions)
        self.assertIsNone(cleanup)
        self.assertTrue(source.closed and wire is not None and wire.closed)
        self.assertTrue(rejected)
        self.assertEqual(observed, [frame(), continuation])

    def test_config_owner_refuses_expiry_before_new_directory(self) -> None:
        """An expired or signalled original control cannot admit config FD ownership."""
        for status in (0, 130):
            control = guest.Control(0.0)
            control.status = status
            with (
                patch.object(time, "monotonic", return_value=31.0),
                patch.object(ops_guest, "directory") as opening,
                self.assertRaises(ValueError),
            ):
                _ = guest.ConfigInputs(control)
            opening.assert_not_called()

    def test_guest_late_restore_signal_remains_negative(self) -> None:
        """The actual guest entry never promotes an earlier frame past late cancellation."""
        control = guest.Control(time.monotonic())
        closed: list[bool] = []
        wire = cast(
            "guest.Wire", cast("object", SimpleNamespace(close=lambda: closed.append(True)))
        )
        restores = 0

        def install(_number: int, _handler: object) -> object:
            nonlocal restores
            restores += 1
            if restores > HANDLER_COUNT:
                control.signal(signal.SIGINT, None)
            return signal.SIG_DFL

        with (
            patch.object(guest, "Control", return_value=control),
            patch.object(guest, "Wire", return_value=wire),
            patch.object(guest, "execute", return_value=result()),
            patch.object(signal, "signal", side_effect=install),
            patch.object(os, "umask", return_value=0o077),
            patch.object(sys, "stdout", SimpleNamespace(buffer=io.BytesIO())),
            patch.object(sys, "argv", ["guest"]),
        ):
            status = guest.main()
        self.assertEqual(closed, [True])
        self.assertEqual(status, 130)

    def test_config_metadata_refusal_closes_each_new_original(self) -> None:
        """Parent and current-stream metadata faults keep identity through close failures."""
        fstat = os.fstat
        close = os.close
        adopt = stage.adopt
        observations: list[tuple[bool, list[bool], bool]] = []

        def exercise(boundary: int) -> tuple[bool, list[bool], bool]:
            descriptors: list[int] = []
            streams: list[BinaryIO] = []
            primary = OSError("fixed_metadata_refusal")
            count = 0
            caught: BaseException | None = None
            with tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                _ = (directory / "migration.env").write_bytes(b"original")

                def parent(_path: Path) -> int:
                    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
                    descriptors.append(fd)
                    return fd

                def metadata(fd: int) -> os.stat_result:
                    nonlocal count
                    count += 1
                    if count == boundary:
                        raise primary
                    return fstat(fd)

                def admitted(fd: int, mode: str) -> BinaryIO:
                    stream = adopt(fd, mode)
                    streams.append(stream)
                    return stream

                def closing(fd: int) -> None:
                    close(fd)
                    if fd in descriptors:
                        message = "fixed_secondary_close"
                        raise OSError(message)

                try:
                    with (
                        patch.object(guest, "CONFIG", directory),
                        patch.object(ops_guest, "directory", side_effect=parent),
                        patch.object(os, "fstat", side_effect=metadata),
                        patch.object(os, "close", side_effect=closing),
                        patch.object(stage, "adopt", side_effect=admitted),
                    ):
                        _ = guest.ConfigInputs(guest.Control(time.monotonic()))
                except OSError as error:
                    caught = error
                parent_closed = False
                try:
                    _ = fstat(descriptors[0])
                except OSError:
                    parent_closed = True
                return caught is primary, [stream.closed for stream in streams], parent_closed

        observations = [exercise(1), exercise(2)]
        self.assertEqual(observations, [(True, [], True), (True, [True], True)])

    def test_new_attempt_namespace_requires_one_exact_private_directory(self) -> None:
        """The coordination seam cannot select a foreign/multiple native attempt."""
        actual_lstat = Path.lstat

        def namespace(path: Path) -> os.stat_result:
            fields = list(actual_lstat(path))
            fields[4] = fields[5] = 0
            return os.stat_result(fields)

        with tempfile.TemporaryDirectory() as temporary:
            public = Path(temporary)
            attempt = public / "results" / ("release." + TOKEN)
            attempt.mkdir(parents=True, mode=0o700)
            with (
                patch.object(guest, "PUBLIC", public),
                patch.object(Path, "lstat", autospec=True, side_effect=namespace),
            ):
                observed = guest.active_attempt(frozenset(), guest.Control(time.monotonic()))
                (attempt.parent / ("release." + "2" * 32)).mkdir(mode=0o700)
                with self.assertRaises(ValueError):
                    _ = guest.active_attempt(frozenset(), guest.Control(time.monotonic()))
        self.assertEqual(observed, attempt)

    def test_workflow_is_explicit_and_does_not_increase_product_timeout(self) -> None:
        """Only the new deliberate fixture uses a larger measured setup envelope."""
        workflow = (ROOT / ".github/workflows/release-deploy-runtime.yml").read_text()
        self.assertIn("timeout-minutes: 90", workflow)
        self.assertNotIn("id-token:", workflow)
        self.assertNotIn("--force", workflow)
        self.assertIn("--previous-artifact-id", workflow)
        self.assertIn("Compile trusted controllers before exposing", workflow)
        self.assertIn("rustup toolchain install 1.99.0 --profile minimal", workflow)
        self.assertNotIn("dtolnay/rust-toolchain@", workflow)


if __name__ == "__main__":
    _ = unittest.main()
