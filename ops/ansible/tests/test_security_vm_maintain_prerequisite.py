"""Inert fixed launcher-prerequisite contracts; no namespaces, daemon, VM or900 launch."""

from __future__ import annotations

import copy
import fcntl
import hashlib
import json
import os
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
import caddy_probe_guest as caddy
import security_vm_fetch as fetch
import security_vm_maintain_prerequisite as keeper
import security_vm_maintain_prerequisite_guest as guest
import security_vm_stage_guest as stage
from caddy_probe_guest import IMAGE
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from typing import BinaryIO

    import security_vm_receive as receive

HASHES: dict[str, str] = dict.fromkeys(
    ("opsSha256", "unshareSha256", "launcherSha256", "timeoutSha256"), "a" * 64
)


def frame(scenario: str = "positive") -> dict[str, object]:
    """Supply source-shaped passive facts without claiming an actual signed native execution."""
    code = guest.EXPECTED_STATUS[scenario]
    facts: dict[str, object] = dict.fromkeys(guest.BOOLEAN_FACTS, True)
    facts.update(dict.fromkeys(guest.HASH_FACTS, "a" * 64))
    facts.update(
        {
            "nestedClientObserved": scenario not in {"positive", "dirty-source"},
            "launcherStopped": scenario not in {"positive", "dirty-source"},
            "termSeen": scenario == "cancel-owned-launcher",
            "workCapExpired": scenario == "timeout-stopped-launcher",
            "adoptedReaped": 0,
            "adoptedRawStatuses": [],
            "wrapperRawStatus": code,
            "launcherRawStatus": code,
            "stdoutBytes": 0,
            "stderrBytes": 0,
            "launcherWorkElapsedNs": (900 if scenario == "timeout-stopped-launcher" else 1) * 10**9,
        }
    )
    return {
        "schemaVersion": 1,
        "experiment": "maintain-launcher-prerequisite",
        "trial": guest.TRIALS[scenario],
        "passed": code == 0,
        "phase": "complete" if scenario == "positive" else "result",
        "failure": {
            "positive": None,
            "dirty-source": "command",
            "cancel-owned-launcher": "terminate",
            "timeout-stopped-launcher": "deadline",
        }[scenario],
        "facts": facts,
    }


def data(value: object) -> bytes:
    """Encode only finite JSON-shaped local fixtures."""
    return json.dumps(value, allow_nan=False).encode()


class NativeProjectionTests(unittest.TestCase):
    """Keep closed passive observations distinct from an accepted completed-case proof."""

    def test_structural_refusals_keep_null_hashes_and_actual_large_counts(self) -> None:
        """A refusal retains its counters and nulls rather than clamping or synthesizing hashes."""
        value = frame()
        value.update({"passed": False, "phase": "settle", "failure": "bounds"})
        facts = guest.dictionary(value["facts"])
        facts.update(
            {
                "stdoutSha256": None,
                "stdoutBytes": guest.MAX_STREAM + 1,
                "adoptedReaped": 257,
                "adoptedRawStatuses": [0] * 256,
            }
        )
        observed = guest.native_frame(data(value))
        self.assertEqual(observed, value)
        with self.assertRaises(ValueError):
            _ = guest.native_projection(data(value), 1, "dirty-source", HASHES)

    def test_four_exact_cases_require_their_completed_phase_and_observed_status(self) -> None:
        """Earlier refusal phases remain passive; none can satisfy a settled-case proof."""
        for scenario in guest.CASES:
            value = frame(scenario)
            code = guest.EXPECTED_STATUS[scenario]
            self.assertEqual(guest.native_projection(data(value), code, scenario, HASHES), value)
            for phase in ("admission", "result" if scenario == "positive" else "complete"):
                changed = copy.deepcopy(value)
                changed["phase"] = phase
                self.assertEqual(guest.native_frame(data(changed)), changed)
                with self.assertRaises(ValueError):
                    _ = guest.native_projection(data(changed), code, scenario, HASHES)
            with self.assertRaises(ValueError):
                _ = guest.native_projection(data(value), 7, scenario, HASHES)

    def test_unknown_fields_bool_statuses_incomplete_hashes_and_vector_mismatch_refuse(
        self,
    ) -> None:
        """Private extra data or malformed typed facts cannot enter the closed projection."""
        value = frame()
        for name, replacement in (
            ("wrapperRawStatus", True),
            ("adoptedReaped", -1),
            ("adoptedRawStatuses", [0]),
            ("stdoutSha256", "A" * 64),
            ("launcherWorkElapsedNs", True),
        ):
            changed = copy.deepcopy(value)
            guest.dictionary(changed["facts"])[name] = replacement
            with self.assertRaises(ValueError):
                _ = guest.native_frame(data(changed))
        value["PRIVATE_UNKNOWN"] = "PRIVATE_VALUE"
        with self.assertRaises(ValueError) as got:
            _ = guest.native_frame(data(value))
        self.assertNotIn("PRIVATE", str(got.exception))


class ClockTests(unittest.TestCase):
    """Every phase is clipped from one entry, including the last pre-widen check."""

    def test_exact510_and_original1800_are_independent_of_clock_epoch(self) -> None:
        """Remaining1170 cannot rescue expired setup or a later final live admission."""
        for origin in (0.0, 100.0, 10000.0):
            control = guest.Control(origin)
            control.setup_end, control.body, control.end = (
                origin + 510,
                origin + 1680,
                origin + 1800,
            )
            with patch.object(
                guest, "time", SimpleNamespace(monotonic=lambda origin=origin: origin + 509.9)
            ):
                control.advance()
            self.assertFalse(control.setup)
            self.assertEqual(control.end, origin + 1800)
            expired = guest.Control(origin)
            expired.setup_end, expired.body, expired.end = (
                origin + 510,
                origin + 1680,
                origin + 1800,
            )
            with (
                patch.object(
                    guest, "time", SimpleNamespace(monotonic=lambda origin=origin: origin + 510)
                ),
                self.assertRaises(ValueError),
            ):
                expired.advance()
            self.assertTrue(expired.setup)


@final
class Sink:
    """Borrow an actual inert original while introducing one checked-close failure or crossing."""

    def __init__(self, stream: BinaryIO, close: Callable[[], None]) -> None:
        """Retain the actual stream before a fault can be delivered."""
        self.stream, self.after = stream, close

    @property
    def closed(self) -> bool:
        """Observe the real stream state, not a test-assigned completion flag."""
        return self.stream.closed

    def write(self, value: bytes) -> int:
        """Let the unchanged process pump write to the actual private file."""
        return self.stream.write(value)

    def flush(self) -> None:
        """Finish the actual buffered evidence before returning."""
        self.stream.flush()

    def seek(self, offset: int, whence: int = 0) -> int:
        """Borrow the real stream cursor."""
        return self.stream.seek(offset, whence)

    def read(self, maximum: int = -1) -> bytes:
        """Read actual retained evidence bytes."""
        return self.stream.read(maximum)

    def close(self) -> None:
        """Close the real original before delivering the one later close observation."""
        if not self.stream.closed:
            self.stream.close()
            self.after()


class NativeCallTests(unittest.TestCase):
    """Use one actual inert Python child with only the native executable seam substituted."""

    def call_case(  # noqa: PLR0915 -- All actual owners settle before assertions.
        self,
        status: int,
        *,
        late_wait: bool = False,
        late_close: bool = False,
        close_error: bool = False,
        late_installed: bool = False,
    ) -> tuple[BaseException | None, guest.NativeCall]:
        """Execute the unchanged pump, retainedFD inheritance, pipes and checked-close paths."""
        clock = [0.0]
        control = guest.Control(0)
        control.setup, control.body, control.end = False, 1680, 1800
        real_spawn = cast("type[subprocess.Popen[bytes]]", subprocess.Popen)
        real_pump, real_adopt = bounded_process.pump, stage.adopt
        children: list[subprocess.Popen[bytes]] = []
        sinks: list[BinaryIO] = []
        secondary = OSError("PRIVATE_SECONDARY_CLOSE")
        failure: BaseException | None = None
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "installed"
            descriptor = os.open(source, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with real_adopt(descriptor, "wb") as output:
                _ = output.write(b"owned-inert-payload")
                output.flush()
            original = source.open("rb")

            crossings = iter((False, late_installed))

            def check() -> None:
                self.assertEqual(os.pread(original.fileno(), 100, 0), b"owned-inert-payload")
                clock[0] = 1050 if next(crossings) else clock[0]

            candidate = cast(
                "stage.Original", cast("object", SimpleNamespace(file=original, check=check))
            )
            wire = cast("guest.Wire", cast("object", SimpleNamespace(no_extra=lambda: None)))
            call = guest.NativeCall(control, candidate, wire)

            def launch(arguments: Sequence[str], **options: object) -> subprocess.Popen[bytes]:
                self.assertEqual(
                    arguments[:4], [str(guest.INSTALLED), "release", "maintain-prerequisite", "run"]
                )
                self.assertEqual(options["executable"], "/proc/self/fd/" + str(original.fileno()))
                self.assertEqual(options["pass_fds"], (original.fileno(),))
                code = (
                    "import os,sys; "
                    "assert os.read(int(sys.argv[1]),100)==b'owned-inert-payload'; "
                    "print('actual stdout'); print('actual stderr',file=sys.stderr); "
                    "sys.exit(int(sys.argv[2]))"
                )
                child = real_spawn(
                    [sys.executable, "-c", code, str(original.fileno()), str(status)],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    pass_fds=(original.fileno(),),
                    start_new_session=True,
                )
                children.append(child)
                return child

            def pump(
                child: subprocess.Popen[bytes],
                output: BinaryIO,
                error: BinaryIO,
                limits: bounded_process.Limits,
            ) -> int:
                observed = real_pump(child, output, error, limits)
                if late_wait:
                    clock[0] = 1050
                return observed

            def adopt(number: int, mode: str) -> BinaryIO:
                actual = real_adopt(number, mode)
                sinks.append(actual)

                def closed() -> None:
                    if late_close:
                        clock[0] = 1050
                    if close_error:
                        raise secondary

                return cast("BinaryIO", cast("object", Sink(actual, closed)))

            try:
                with (
                    patch.object(guest, "PRIVATE", root),
                    patch.object(guest, "ROOT", root),
                    patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
                    patch.object(subprocess, "Popen", side_effect=launch),
                    patch.object(bounded_process, "pump", side_effect=pump),
                    patch.object(stage, "adopt", side_effect=adopt),
                ):
                    call.invoke(
                        "positive"
                        if status in (0, 7)
                        else {
                            1: "dirty-source",
                            143: "cancel-owned-launcher",
                            124: "timeout-stopped-launcher",
                        }[status]
                    )
            except (OSError, ValueError) as error:
                failure = error
            finally:
                original.close()
            self.assertTrue(original.closed and all(stream.closed for stream in sinks))
            self.assertTrue(children and all(child.returncode is not None for child in children))
            self.assertTrue(
                all(
                    stream is None or stream.closed
                    for child in children
                    for stream in (child.stdin, child.stdout, child.stderr)
                )
            )
            self.assertTrue(call.pipes_closed and call.sinks_closed)
            self.assertEqual(call.status, status)
        return failure, call

    def test_actual_four_expected_statuses_are_observations_not_automatic_failures(self) -> None:
        """Dirty1/cancel143/timeout124 remain expected raw facts in this invocation layer."""
        for status in (0, 1, 143, 124):
            failure, call = self.call_case(status)
            self.assertIsNone(failure)
            self.assertTrue(call.output_complete)

    def test_actual_unexpected_status_precedes_later_close_and_deadline_failure(self) -> None:
        """Current raw7 is latched before a real later checked-close or observation crossing."""
        for late in (False, True):
            failure, _ = self.call_case(7, close_error=not late, late_wait=late)
            self.assertIsInstance(failure, guest.NativeStatusError)
            self.assertEqual(
                cast("BaseException", failure).args, ("maintain_prerequisite_native_status",)
            )

    def test_original_invocation_end_fences_consuming_wait_and_actual_last_close(self) -> None:
        """Actual child0/EOF cannot authorize late success before the outer keeper body expires."""
        for late in (False, True):
            failure, _ = self.call_case(0, late_wait=not late, late_close=late)
            self.assertIsInstance(failure, guest.NativeDeadlineError)

    def test_late_final_installed_check_precedes_actual_secondary_sink_close(self) -> None:
        """The same invocation fence observes a late original before a later close can fail."""
        failure, call = self.call_case(0, late_installed=True, close_error=True)
        self.assertIsInstance(failure, guest.NativeDeadlineError)
        self.assertFalse(call.output_complete)


class RenderTests(unittest.TestCase):
    """Render actual templates without an engine or installed-input changes."""

    def test_source_caddyfile_and_all_seven_private_secret_roles_are_preserved(self) -> None:
        """Preserve natural TLS source and supply the actual coordinated seed secret."""
        paths = [
            "Caddyfile",
            "docker-compose.yml",
            *[
                "ops/ansible/templates/public-" + template + ".j2"
                for _, template in guest.TEMPLATE_NAMES
            ],
        ]
        sources = {name: (ROOT / name).read_bytes() for name in paths}
        secrets = {
            name: format(index + 1, "x") * 64
            for index, name in enumerate(sorted(guest.SECRET_NAMES))
        }
        identity = {
            "image": "sha256:" + "a" * 64,
            "revision": "b" * 40,
            "runId": "c" * 32,
            "postgres": "docker.io/library/postgres:18.6-bookworm@sha256:" + "d" * 64,
            "caddy": "ghcr.io/swiftraccoon/simplestchat-caddy:v2.11.7@sha256:" + "e" * 64,
            "bridge": "sc" + "c" * 12,
        }
        # The actual source pin is fixed by the checked Caddy dependency, not a new image selector.
        identity["caddy"] = IMAGE
        rendered = guest.rendered_inputs(sources, identity, secrets)
        self.assertEqual(rendered["Caddyfile"], sources["Caddyfile"])
        self.assertEqual(json.loads(rendered["secrets.json"]), secrets)
        self.assertIn(b"ALLOWED_ORIGINS=https://maintain.localhost", rendered["app.env"])
        self.assertIn(b"WEBAUTHN_RP_ID=maintain.localhost", rendered["app.env"])
        self.assertIn(secrets["proxy_secret"].encode(), rendered["proxy.env"])
        self.assertIn(secrets["database_password"].encode(), rendered["app.env"])
        self.assertIn(secrets["migration_password"].encode(), rendered["init-database.sql"])
        self.assertNotIn(b"fixture.crt", rendered["compose.public.yml"])
        self.assertNotIn(b"fixture.key", rendered["Caddyfile"])


class ControllerClockTests(unittest.TestCase):
    """The controller retains its own original interval before any guest GO authority."""

    def test_controller_healthy_setup_keeps_one_original1800_and_full1170_debt(self) -> None:
        """A live setup admission widens only this same owner, with no new start or end."""
        control = keeper.Control(started=100.0)
        with patch.object(time, "monotonic", return_value=609.9):
            control.advance()
        self.assertFalse(control.setup)
        self.assertEqual((control.started, control.end, control.whole_body_end), (100, 1900, 1780))

    def test_controller_exact_setup_end_refuses_without_widening(self) -> None:
        """The same1170 remaining debt cannot rescue elapsed510 setup."""
        control = keeper.Control(started=100.0)
        with (
            patch.object(time, "monotonic", return_value=610.0),
            self.assertRaises(ToolError),
        ):
            control.advance()
        self.assertTrue(control.setup)
        self.assertEqual(control.end, 1900)

    def test_controller_post_debt_check_crossing_keeps_setup_and_primary_refusal(self) -> None:
        """The final live check runs after successful debt observation and before widening."""
        control = keeper.Control(started=100.0)
        with (
            patch.object(time, "monotonic", side_effect=[609.9, 609.9, 610.0]),
            self.assertRaises(ToolError),
        ):
            control.advance()
        self.assertTrue(control.setup)
        self.assertEqual(keeper.failure_kind(ToolError("maintain_vm_deadline")), "deadline")
        self.assertEqual(keeper.failure_kind(ToolError("PRIVATE_CONTROLLER_SECRET")), "refused")
        self.assertEqual(keeper.failure_kind(OSError("PRIVATE_CONTROLLER_SECRET")), "io")


def controller_file(path: Path, value: bytes, mode: int) -> None:
    """Close one exclusive private fixture writer before applying its final role mode."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    output = stage.adopt(descriptor, "wb")
    first: BaseException | None = None
    try:
        guest.require(output.write(value) == len(value))
        output.flush()
        os.fchmod(output.fileno(), mode)
    except BaseException as error:  # noqa: BLE001 -- Settle this admitted writer first.
        first = error
    first = fetch.settle((output.close,), first)
    if first is not None:
        raise first


class ControllerBundleTests(unittest.TestCase):
    """Use actual own-temp originals with only fixed Git command observations inert."""

    def bundle_case(self, edge: str) -> dict[str, object]:
        """Retain inputs/output until all source witnesses and original closes settle."""
        calls: list[tuple[str, list[str]]] = []
        revision, expected_tree = "a" * 40, "b" * 40
        original_bytes = b"inert-bundle-original"
        first: BaseException | None = None
        returned: tuple[fetch.Held, str] | None = None
        owners = fetch.Owners()
        control = keeper.Control(started=time.monotonic())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            try:
                controller_file(root / "git", b"inert-retained-git", 0o755)
                git = fetch.Held.open(root / "git", 1024, control, system=True)
                owners.inputs.append(git)
                controller_file(root / "source", b"inert-source", 0o600)
                original = fetch.Held.open(root / "source", 1024, control)
                owners.inputs.append(original)

                def command(
                    name: str, arguments: Sequence[str], **_options: object
                ) -> tuple[int, bytes]:
                    calls.append((name, list(arguments)))
                    guest.require(arguments[:3] == [str(git.path), "-C", str(ROOT)])
                    if name.startswith("source-head-"):
                        return 0, (revision + "\n").encode()
                    if name.startswith("source-tree-"):
                        return 0, (expected_tree + "\n").encode()
                    if name.startswith("source-clean-"):
                        return 0, b"?? inert\n" if edge == "dirty" else b""
                    guest.require(name == "source-bundle")
                    guest.require(
                        list(arguments[3:])
                        == ["bundle", "create", str(root / "maintain-source.bundle"), "HEAD"]
                    )
                    controller_file(
                        root / "maintain-source.bundle",
                        original_bytes,
                        0o644 if edge == "mode" else 0o600,
                    )
                    if edge == "changed":
                        with original.path.open("r+b") as output:
                            _ = output.write(b"changed")
                    return 0, b""

                host = cast(
                    "receive.Host",
                    cast(
                        "object",
                        SimpleNamespace(
                            output=root,
                            control=control,
                            run=command,
                        ),
                    ),
                )
                returned = keeper.source_bundle(host, git, revision, {"source": original}, owners)
            except BaseException as error:  # noqa: BLE001 -- All admitted originals close first.
                first = error
            finally:
                control.settling = True
                cleanup = owners.finish(control, {})
            return {
                "first": first,
                "cleanup": cleanup,
                "calls": calls,
                "allClosed": all(item.stream.closed for item in owners.inputs),
                "tree": None if returned is None else returned[1],
                "sha256": None if returned is None else returned[0].sha256,
                "expectedSha256": hashlib.sha256(original_bytes).hexdigest(),
            }

    def test_bundle_uses_exact_before_after_roles_and_retained_private_output(self) -> None:
        """Keep post-bundle tree/clean checks and the actual retained output hash."""
        actual = self.bundle_case("healthy")
        self.assertIsNone(actual["first"])
        self.assertIsNone(actual["cleanup"])
        self.assertIs(actual["allClosed"], expr2=True)
        self.assertEqual(actual["tree"], "b" * 40)
        self.assertEqual(actual["sha256"], actual["expectedSha256"])
        calls = cast("list[tuple[str, list[str]]]", actual["calls"])
        self.assertEqual(
            [role for role, _ in calls],
            [
                "source-head-before",
                "source-tree-before",
                "source-clean-before",
                "source-bundle",
                "source-head-after",
                "source-tree-after",
                "source-clean-after",
            ],
        )

    def test_dirty_source_refuses_before_any_bundle_creation_and_closes_inputs(self) -> None:
        """A clean selection cannot be inferred from HEAD alone or a saved tree hash."""
        actual = self.bundle_case("dirty")
        self.assertIsInstance(actual["first"], ToolError)
        self.assertIs(actual["allClosed"], expr2=True)
        self.assertIsNone(actual["sha256"])
        calls = cast("list[tuple[str, list[str]]]", actual["calls"])
        self.assertEqual(
            [role for role, _ in calls],
            [
                "source-head-before",
                "source-tree-before",
                "source-clean-before",
            ],
        )

    def test_bundle_public_mode_refuses_and_closes_the_already_admitted_output(self) -> None:
        """A matching bundle hash cannot authorize an output with the wrong private mode."""
        actual = self.bundle_case("mode")
        self.assertIsInstance(actual["first"], ToolError)
        self.assertIs(actual["allClosed"], expr2=True)
        self.assertIsNone(actual["sha256"])

    def test_post_bundle_source_change_refuses_before_output_adoption_and_closes_inputs(
        self,
    ) -> None:
        """Changed same-inode source bytes cannot be hidden by later stable Git observations."""
        actual = self.bundle_case("changed")
        self.assertIsInstance(actual["first"], ToolError)
        self.assertIs(actual["allClosed"], expr2=True)
        self.assertIsNone(actual["sha256"])
        self.assertIsInstance(actual["cleanup"], ToolError)


class FinalProjectionTests(unittest.TestCase):
    """Keep a completed guest's passive source-shaped row separate from runtime authority."""

    def test_complete_guest_status_phase_sources_and_unknown_nested_secrets_cannot_contradict(
        self,
    ) -> None:
        """Retain raw fixtures and exact safe refusals while rejecting contradictory passed rows."""
        expected: dict[str, object] = {
            "runId": "c" * 32,
            "revision": "b" * 40,
            "scenario": "positive",
            "sourcePins": dict.fromkeys(guest.GUEST_SOURCES, "a" * 64),
            "binary": {"sha256": "a" * 64, "bytes": 1},
        }
        observations: dict[str, object] = dict.fromkeys(guest.OBSERVATION_FIELDS, True)
        observations.update({"dirtyReadmeObserved": False, "daemonEffectsUncertain": False})
        value = {
            "schemaVersion": 1,
            "scope": "MaintainLauncherPrerequisiteGuest",
            "runId": expected["runId"],
            "revision": expected["revision"],
            "scenario": "positive",
            "sourcePins": expected["sourcePins"],
            "passed": True,
            "failure": None,
            "checkpoint": "complete",
            "finalStatus": 0,
            "cleanupPassed": True,
            "nativeRawExit": 0,
            "nativeFrame": frame(),
            "observations": observations,
        }
        self.assertEqual(guest.final_projection(copy.deepcopy(value), expected), value)
        for path, replacement, message in (
            (("nativeRawExit",), 7, "ops_guest_unproven"),
            (("finalStatus",), 143, "ops_guest_unproven"),
            (("nativeFrame", "phase"), "admission", "ops_guest_unproven"),
            (("sourcePins", "ops/ansible/files/deploy-public.sh"), "d" * 64, "ops_guest_unproven"),
            (
                ("nativeFrame", "facts", "PRIVATE_UNKNOWN"),
                "PRIVATE_SECRET",
                "stage_vm_guest_unproven",
            ),
            (("observations", "PRIVATE_UNKNOWN"), "PRIVATE_SECRET", "stage_vm_guest_unproven"),
        ):
            changed = copy.deepcopy(value)
            target = changed
            for name in path[:-1]:
                target = guest.dictionary(target[name])
            target[path[-1]] = replacement
            raw = data(changed)
            with self.subTest(path=path), self.assertRaises(ValueError) as got:
                _ = guest.final_projection(changed, expected)
            self.assertEqual(got.exception.args, (message,))
            self.assertEqual(data(changed), raw)
            self.assertNotIn("PRIVATE", str(got.exception))


@final
class CollectorFaults:
    """Borrow the real collector operations and move only one fixed observation boundary."""

    def __init__(self, collector: guest.LiveEvents, clock: list[float], edge: str) -> None:
        """Keep actual operations before any patches are admitted."""
        self.collector, self.clock, self.edge = collector, clock, edge
        self.actual_check, self.actual_drain = collector.check, collector.drain
        self.actual_wait, self.actual_stop = bounded_process.wait_exited, bounded_process.stop
        self.actual_fcntl, self.actual_read = fcntl.fcntl, os.read
        self.actual_dictionary = guest.dictionary
        self.expected_flags = {pipe.fileno(): flags for pipe, flags in collector.flags}
        self.restored: set[int] = set()
        self.read_failed = False
        self.primary = OSError("PRIVATE_FIRST_COLLECTOR_READ")
        self.secondary = OSError("PRIVATE_SECONDARY_COLLECTOR_CLOSE")

    def checked(self) -> None:
        """Observe actual parsed rows before the simulated return crosses the marker end."""
        self.actual_check()
        if self.edge == "marker" and self.collector.rows:
            self.clock[0] = 6

    def lookup(self, value: object) -> dict[str, object]:
        """Cross the same marker end only when scanning the actual parsed Actor object."""
        result = self.actual_dictionary(value)
        if self.edge == "marker-scan" and result.get("ID") == "f" * 64:
            self.clock[0] = 6
        return result

    def drain(self, *, final: bool = False) -> None:
        """Keep actual pipe reads and then cross only the selected EOF-return boundary."""
        self.actual_drain(final=final)
        if self.edge == "eof" and all(self.collector.eof):
            self.clock[0] = 16

    def wait(self, pid: int, timeout: float) -> bool:
        """Return only the real original leader observation, then move the selected clock."""
        result = self.actual_wait(pid, timeout)
        if self.edge == "wait" and result:
            self.clock[0] = 16
        return result

    def stop(self, child: subprocess.Popen[bytes], *, grace: float = 5) -> None:
        """Complete actual group retirement and consuming wait before a late return."""
        self.actual_stop(child, grace=grace)
        if self.edge == "stop":
            self.clock[0] = 16

    def flags(self, fd: int, command: int, argument: int = 0) -> int:
        """Record a real successful original flag restoration before moving its clock."""
        result = self.actual_fcntl(fd, command, argument)
        if command == fcntl.F_SETFL and self.expected_flags.get(fd) == argument:
            self.restored.add(fd)
            if self.edge in {"flags", "flags-close"}:
                self.clock[0] = 16
        return result

    def read(self, fd: int, maximum: int) -> bytes:
        """Inject only one earlier read refusal while every admitted peer remains real."""
        child = self.collector.child
        if (
            self.edge == "paired"
            and not self.read_failed
            and child is not None
            and child.stdout is not None
            and fd == child.stdout.fileno()
        ):
            self.read_failed = True
            raise self.primary
        return self.actual_read(fd, maximum)

    def closed(self) -> None:
        """Observe the late/error boundary only after the actual sink close."""
        if self.edge in {"close", "paired"}:
            self.clock[0] = 16
        if self.edge in {"paired", "flags-close"}:
            raise self.secondary


def collector_child(
    spawn: type[subprocess.Popen[bytes]],
    children: list[subprocess.Popen[bytes]],
    arguments: Sequence[str],
    options: dict[str, object],
) -> subprocess.Popen[bytes]:
    """Spawn the inert event role or the SDK's exact ps role for its still-owned child."""
    if arguments and arguments[0] == "/bin/ps":
        guest.require(
            any(
                list(arguments) == ["/bin/ps", "-g", str(child.pid), "-o", "pid=,pgid=,stat="]
                for child in children
                if child.returncode is None
            )
        )
        guest.require(
            options
            == {
                "stdin": subprocess.DEVNULL,
                "stdout": subprocess.PIPE,
                "stderr": subprocess.PIPE,
                "env": {"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
                "start_new_session": True,
            }
        )
        return spawn(
            list(arguments),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
            start_new_session=True,
        )
    guest.require(
        list(arguments[1:])
        == [
            "--host",
            "unix:///var/run/docker.sock",
            "events",
            "--since",
            "1.000000000",
            "--format",
            "{{json .}}",
        ]
    )
    code = (
        "import json,signal,sys,time; "
        "signal.signal(signal.SIGINT,lambda *_:sys.exit(0)); "
        "print(json.dumps({'Type':'network','Action':'create',"
        "'Actor':{'ID':sys.argv[1]}}),flush=True); "
        "print('actual stderr',file=sys.stderr,flush=True); time.sleep(20)"
    )
    child = spawn(
        [sys.executable, "-u", "-c", code, "f" * 64],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    children.append(child)
    return child


def collector_reap(child: subprocess.Popen[bytes]) -> None:
    """Attempt the exact original consuming wait even after a checked group-cleanup refusal."""
    _ = child.wait(timeout=5)


class CollectorDeadlineTests(unittest.TestCase):
    """Own an actual inert event child and every pipe/sink through the fixed collector caps."""

    def collector_case(self, edge: str) -> dict[str, object]:
        """Keep real process/EOF/close observations while moving only the chosen clock boundary."""
        clock = [0.0]
        control = guest.Control(0)
        control.setup, control.body, control.end = False, 1680, 1800
        spawn, actual_sleep = cast("type[subprocess.Popen[bytes]]", subprocess.Popen), time.sleep
        actual_stop = bounded_process.stop
        children: list[subprocess.Popen[bytes]] = []
        collector: guest.LiveEvents | None = None
        faults: CollectorFaults | None = None
        first: BaseException | None = None
        identifier = "f" * 64

        def sleep(seconds: float) -> None:
            clock[0] += seconds
            actual_sleep(min(seconds, 0.005))

        def tool_check(owner: guest.Control) -> None:
            owner.check()

        def launch(arguments: Sequence[str], **options: object) -> subprocess.Popen[bytes]:
            return collector_child(spawn, children, arguments, options)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            commands = cast(
                "caddy.Commands",
                cast(
                    "object",
                    SimpleNamespace(
                        control=control,
                        tools={"docker": "fixed-inert-docker"},
                        tool_owners={"docker": SimpleNamespace(check=tool_check)},
                        watch=None,
                    ),
                ),
            )
            with (
                patch.object(guest, "PRIVATE", root),
                patch.object(guest, "ROOT", root),
                patch.object(
                    guest, "time", SimpleNamespace(monotonic=lambda: clock[0], sleep=sleep)
                ),
                patch.object(subprocess, "Popen", side_effect=launch),
            ):
                try:
                    collector = guest.LiveEvents(commands, "1.000000000")
                    if not edge.startswith("marker"):
                        collector.join_marker(identifier)
                    faults = CollectorFaults(collector, clock, edge)
                    original_sink = collector.streams[0]
                    collector.streams[0] = cast(
                        "BinaryIO", cast("object", Sink(original_sink, faults.closed))
                    )
                    if edge.startswith("marker"):
                        with (
                            patch.object(collector, "check", side_effect=faults.checked),
                            patch.object(guest, "dictionary", side_effect=faults.lookup),
                        ):
                            collector.join_marker(identifier)
                    else:
                        with (
                            patch.object(collector, "drain", side_effect=faults.drain),
                            patch.object(bounded_process, "wait_exited", side_effect=faults.wait),
                            patch.object(bounded_process, "stop", side_effect=faults.stop),
                            patch.object(fcntl, "fcntl", side_effect=faults.flags),
                            patch.object(os, "read", side_effect=faults.read),
                        ):
                            collector.finish()
                except BaseException as error:  # noqa: BLE001 -- Settle every actual child/peer.
                    first = error
                finally:
                    if collector is not None:
                        first = fetch.settle((collector.finish,), first)
                    for child in children:
                        if child.returncode is None:
                            first = fetch.settle(
                                (lambda child=child: actual_stop(child, grace=1),), first
                            )
                        first = fetch.settle((lambda child=child: collector_reap(child),), first)
                        first = fetch.settle(
                            tuple(
                                stream.close
                                for stream in (child.stdin, child.stdout, child.stderr)
                                if stream is not None
                            ),
                            first,
                        )
            return {
                "failure": first,
                "primary": None if faults is None else faults.primary,
                "marker": None if collector is None else collector.marker,
                "eof": False if collector is None else all(collector.eof),
                "closed": collector is not None
                and collector.closed
                and all(stream.closed for stream in collector.streams),
                "pipesClosed": all(
                    stream is None or stream.closed
                    for child in children
                    for stream in (child.stdin, child.stdout, child.stderr)
                ),
                "reaped": bool(children)
                and all(child.returncode is not None for child in children),
                "actualRestoreCalls": 0 if faults is None else len(faults.restored),
            }

    def test_actual_marker_drain_cannot_succeed_after_original_five_second_end(self) -> None:
        """A real received create row remains passive after the same marker join end."""
        for edge in ("marker", "marker-scan"):
            actual = self.collector_case(edge)
            self.assertIsInstance(actual["failure"], ValueError)
            self.assertIsNone(actual["marker"])
            self.assertTrue(actual["closed"] and actual["pipesClosed"] and actual["reaped"])

    def test_original_finish_end_covers_eof_wait_stop_flags_and_actual_sink_close(self) -> None:
        """Successful settlement cannot promote observations after the saved15-second end."""
        observations = [
            self.collector_case(edge)
            for edge in ("healthy", "eof", "wait", "stop", "flags", "close", "flags-close")
        ]
        self.assertIsNone(observations[0]["failure"])
        self.assertTrue(observations[0]["eof"])
        for actual in observations:
            self.assertTrue(actual["closed"] and actual["pipesClosed"] and actual["reaped"])
            self.assertEqual(actual["actualRestoreCalls"], 2)
        for actual in observations[1:]:
            self.assertIsInstance(actual["failure"], ValueError)

    def test_first_collector_read_error_survives_late_actual_close_and_all_peers_settle(
        self,
    ) -> None:
        """Retain the actual read refusal object when a later successful close crosses the phase."""
        actual = self.collector_case("paired")
        self.assertIs(actual["failure"], actual["primary"])
        self.assertTrue(actual["closed"] and actual["pipesClosed"] and actual["reaped"])
        self.assertEqual(actual["actualRestoreCalls"], 2)


class EventHistoryTests(unittest.TestCase):
    """Exercise closed passive actor/exec joins without claiming real daemon history."""

    def test_postgres_exec_requires_each_exact_create_start_die_and_zero_status(self) -> None:
        """Completed health and workload execs count only after their own full ID joins."""
        command, health = "psql fixed-ledger", "pg_isready fixed-health"
        rows: list[dict[str, object]] = [{"Action": "health_status: healthy"}]
        for identifier, actual in (("a" * 64, command), ("b" * 64, command), ("c" * 64, health)):
            for action in ("exec_create: " + actual, "exec_start: " + actual, "exec_die"):
                attributes = {"execID": identifier, "exitCode": "0"}
                rows.append({"Action": action, "Actor": {"Attributes": attributes}})
        raw = data(rows)
        guest.pg_exec_history(rows, {command: 2}, health)
        self.assertEqual(data(rows), raw)
        changes = [rows[:missing] + rows[missing + 1 :] for missing in range(1, len(rows))]
        changes.extend(([*rows, rows[1]], [*rows, rows[2]], [*rows, rows[3]]))
        for key, replacement in (("execID", "d" * 64), ("exitCode", "7"), ("exitCode", 0)):
            changed = copy.deepcopy(rows)
            attributes = guest.dictionary(guest.dictionary(changed[3]["Actor"])["Attributes"])
            attributes[key] = replacement
            changes.append(changed)
        changed = copy.deepcopy(rows)
        changed[2]["Action"] = "exec_start: PRIVATE_UNREVIEWED_COMMAND"
        changes.append(changed)
        for changed in changes:
            with self.subTest(rows=changed), self.assertRaises(ValueError) as got:
                guest.pg_exec_history(changed, {command: 2}, health)
            self.assertEqual(got.exception.args, ("ops_guest_unproven",))
        with self.assertRaises(ValueError):
            guest.pg_exec_history(rows, {command: 1}, health)

    def test_unreviewed_actor_actions_and_mismatched_term_stop_order_refuse(self) -> None:
        """Unknown mutations cannot disappear inside an otherwise completed actor generation."""
        guest.actor_action("kill", "simplestchat", {"signal": "15"}, ["create", "start"])
        guest.actor_action("stop", "migrate", {}, ["create", "start", "die"])
        for action in ("create", "start", "die", "destroy"):
            guest.actor_action(action, "postgres", {}, [])
        cases: list[tuple[str, str, dict[str, object], list[str]]] = [
            (action, "simplestchat", {}, ["create", "start"])
            for action in ("update", "rename", "pause", "restart", "PRIVATE_UNKNOWN")
        ]
        cases.extend(
            (
                ("kill", "postgres", {"signal": "15"}, ["create", "start"]),
                ("kill", "migrate", {"signal": 15}, ["create", "start"]),
                ("kill", "migrate", {"signal": "9"}, ["create", "start"]),
                ("kill", "migrate", {"signal": "15"}, ["create", "start", "die"]),
                ("stop", "migrate", {}, ["create", "start"]),
                ("stop", "postgres", {}, ["create", "start", "die"]),
            )
        )
        for arguments in cases:
            with self.subTest(arguments=arguments), self.assertRaises(ValueError) as got:
                guest.actor_action(*arguments)
            self.assertEqual(got.exception.args, ("ops_guest_unproven",))


@final
class LifecycleFixture:
    """Own fixed mocked roles and actual file peers independently of orchestration verdicts."""

    def __init__(
        self, root: Path, control: guest.Control, clock: list[float], *, body: bool
    ) -> None:
        """Register each actual stream before its first write or any injected refusal."""
        self.control, self.clock, self.body = control, clock, body
        self.primary, self.secondary = (
            OSError("PRIVATE_BODY_IO"),
            OSError("PRIVATE_FINALIZER_CLOSE"),
        )
        self.streams: list[BinaryIO] = []
        self.closed: list[int] = []
        self.unknown_effect = False
        self.observed: dict[str, object] = dict.fromkeys(guest.OBSERVATION_FIELDS, False)
        self.observed["observedExpectedDaemonJoins"] = True
        self.raw = data(frame())
        self.native = cast(
            "guest.NativeCall",
            cast(
                "object",
                SimpleNamespace(status=7, called=True, output_complete=True, output=self.raw),
            ),
        )
        try:
            for index in range(3):
                descriptor = os.open(root / str(index), os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
                stream = stage.adopt(descriptor, "w+b")
                self.streams.append(stream)
                guest.require(stream.write(b"inert-owner") == len(b"inert-owner"))
                stream.flush()
        except BaseException as error:  # noqa: BLE001 -- Settle every admitted fixture original.
            first = fetch.settle(tuple(stream.close for stream in self.streams), error)
            raise guest.required(first) from None

    def prepare(self) -> None:
        """Exercise the actual control's one setup-to-body admission."""
        self.control.checkpoint = "ready"
        self.control.advance()

    def invoke(self) -> None:
        """Keep the first body IO distinct from a later lifecycle refusal."""
        self.control.checkpoint = "native-result"
        if self.body:
            raise self.primary

    def observe(self) -> None:
        """Run only the fixed inert observation boundary."""
        self.control.checkpoint = "source-observe"
        self.control.check()

    def close(self, index: int) -> None:
        """Deliver faults only after the original stream has physically closed."""
        self.streams[index].close()
        self.closed.append(index)
        if index == 0:
            raise self.secondary
        if self.body and index == len(self.streams) - 1:
            self.clock[0] = 1801

    def finish(self) -> None:
        """Attempt every close and the original final fence even after an earlier refusal."""
        first = fetch.settle((self.control.check,))
        self.control.settling = True
        first = fetch.settle(
            (lambda: self.close(0), lambda: self.close(1), lambda: self.close(2)), first
        )
        first = fetch.settle((self.control.check,), first)
        if first is not None:
            raise first


class ExecuteLifecycleTests(unittest.TestCase):
    """Test fixed orchestration with real owned files; no auth, native or engine execution."""

    def lifecycle_case(self, *, body_error: bool) -> dict[str, object]:
        """Retain the body classification across every actual file close and late final fence."""
        clock = [0.0]
        control = guest.Control(0)
        errors: list[BaseException] = []
        value: dict[str, object] = {
            "runId": "c" * 32,
            "revision": "b" * 40,
            "scenario": "positive",
            "sourcePins": dict.fromkeys(guest.GUEST_SOURCES, "a" * 64),
            "binary": {"sha256": "a" * 64, "bytes": 1},
            "setupSeconds": 510,
            "bodySeconds": 1680,
            "endSeconds": 1800,
        }
        original_classification = guest.classification

        def classification(error: BaseException, status: int) -> str:
            errors.append(error)
            return original_classification(error, status)

        def no_extra() -> None:
            control.check()

        with tempfile.TemporaryDirectory() as temporary:
            fixture = LifecycleFixture(Path(temporary), control, clock, body=body_error)
            try:
                wire = cast(
                    "guest.Wire",
                    cast("object", SimpleNamespace(line=lambda: b"", no_extra=no_extra)),
                )
                with (
                    patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
                    patch.object(guest, "request", return_value=value),
                    patch.object(guest, "Fixture", return_value=fixture),
                    patch.object(guest, "classification", side_effect=classification),
                ):
                    report = guest.execute(control, wire)
            finally:
                cleanup = fetch.settle(tuple(stream.close for stream in fixture.streams))
            return {
                "report": report,
                "errors": errors,
                "first": fixture.primary if body_error else fixture.secondary,
                "closed": all(stream.closed for stream in fixture.streams),
                "closeOrder": fixture.closed,
                "cleanup": cleanup,
                "nativeRaw": fixture.raw,
                "nativeAfter": data(guest.native_frame(fixture.native.output)),
            }

    def test_successful_body_cannot_survive_an_actual_finalizer_close_refusal(self) -> None:
        """A successful fixed body is insufficient when an admitted original refuses its close."""
        actual = self.lifecycle_case(body_error=False)
        report = guest.dictionary(actual["report"])
        self.assertEqual(actual["closeOrder"], [0, 1, 2])
        self.assertIs(actual["closed"], expr2=True)
        self.assertIsNone(actual["cleanup"])
        self.assertEqual(actual["errors"], [actual["first"]])
        self.assertEqual((report["passed"], report["cleanupPassed"]), (False, False))
        self.assertEqual((report["failure"], report["finalStatus"]), ("io", 1))
        self.assertEqual(report["nativeRawExit"], 7)
        self.assertIsNotNone(report["nativeFrame"])
        self.assertEqual(actual["nativeRaw"], actual["nativeAfter"])

    def test_first_body_io_survives_secondary_close_and_original_clock_refusal(self) -> None:
        """Later deadline status is separate from the first body IO classification."""
        actual = self.lifecycle_case(body_error=True)
        report = guest.dictionary(actual["report"])
        self.assertEqual(actual["closeOrder"], [0, 1, 2])
        self.assertIs(actual["closed"], expr2=True)
        self.assertIsNone(actual["cleanup"])
        self.assertEqual(actual["errors"], [actual["first"]])
        self.assertEqual((report["passed"], report["cleanupPassed"]), (False, False))
        self.assertEqual((report["failure"], report["finalStatus"]), ("io", 124))
        self.assertEqual(report["checkpoint"], "native-result")
        self.assertEqual(report["nativeRawExit"], 7)
        self.assertIsNotNone(report["nativeFrame"])
        self.assertEqual(actual["nativeRaw"], actual["nativeAfter"])


class MainLifecycleTests(unittest.TestCase):
    """Keep entry restoration and output fences real while mocking every global-state setter."""

    def main_case(self, edge: str) -> dict[str, object]:
        """Latch the actual Control through mock restoration; own one real inert Wire peer."""
        clock = [0.0]
        controls: list[guest.Control] = []
        emitted: list[dict[str, object]] = []
        restored: list[int] = []
        masks: list[int] = []
        original_mask = 0o022
        handlers = {signal.SIGINT: object(), signal.SIGTERM: object()}
        expected: dict[str, object] = {
            "runId": "c" * 32,
            "revision": "b" * 40,
            "scenario": "positive",
            "sourcePins": dict.fromkeys(guest.GUEST_SOURCES, "a" * 64),
            "binary": {"sha256": "a" * 64, "bytes": 1},
            "setupSeconds": 510,
            "bodySeconds": 1680,
            "endSeconds": 1800,
        }
        observed: dict[str, object] = dict.fromkeys(guest.OBSERVATION_FIELDS, True)
        observed.update({"dirtyReadmeObserved": False, "daemonEffectsUncertain": False})
        completed: dict[str, object] = {
            "schemaVersion": 1,
            "scope": "MaintainLauncherPrerequisiteGuest",
            "runId": expected["runId"],
            "revision": expected["revision"],
            "scenario": "positive",
            "sourcePins": expected["sourcePins"],
            "passed": True,
            "failure": None,
            "checkpoint": "complete",
            "finalStatus": 0,
            "cleanupPassed": True,
            "nativeRawExit": 0,
            "nativeFrame": frame(),
            "observations": observed,
        }

        def signal_set(number: signal.Signals, handler: object) -> object:
            if handler is handlers[number]:
                restored.append(number)
                if edge == "handler" and number == signal.SIGINT:
                    controls[0].signal(signal.SIGTERM, None)
            return handlers[number]

        def mask_set(mask: int) -> int:
            masks.append(mask)
            if edge == "mask" and mask == original_mask:
                controls[0].signal(signal.SIGTERM, None)
            return original_mask

        def execute(control: guest.Control, _wire: guest.Wire) -> dict[str, object]:
            controls.append(control)
            control.configure(expected)
            control.advance()
            control.settling = True
            return guest.final_projection(copy.deepcopy(completed), expected)

        def emit(value: object) -> None:
            emitted.append(copy.deepcopy(guest.dictionary(value)))
            if edge == "output":
                clock[0] = 1800

        with tempfile.TemporaryDirectory() as temporary:
            descriptor = os.open(
                Path(temporary) / "wire", os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600
            )
            peer = stage.adopt(descriptor, "w+b")
            try:
                wire = SimpleNamespace(close=peer.close, emit=emit)
                with (
                    patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
                    patch.object(signal, "signal", side_effect=signal_set),
                    patch.object(os, "umask", side_effect=mask_set),
                    patch.object(guest, "Wire", return_value=wire),
                    patch.object(guest, "execute", side_effect=execute),
                ):
                    status = guest.main()
            finally:
                peer.close()
            return {
                "status": status,
                "controlStatus": controls[0].status,
                "emitted": emitted,
                "restored": restored,
                "masks": masks,
                "closed": peer.closed,
            }

    def test_sigterm_during_each_mock_restoration_invalidates_completed_metadata(self) -> None:
        """Handler or umask restoration cancellation cannot preserve an earlier passed row."""
        for edge in ("handler", "mask"):
            actual = self.main_case(edge)
            self.assertEqual((actual["status"], actual["controlStatus"]), (143, 143))
            self.assertEqual(actual["restored"], [signal.SIGINT, signal.SIGTERM])
            self.assertEqual(actual["masks"], [0o077, 0o022])
            self.assertIs(actual["closed"], expr2=True)
            rows = cast("list[dict[str, object]]", actual["emitted"])
            self.assertEqual(len(rows), 1)
            self.assertEqual((rows[0]["passed"], rows[0]["cleanupPassed"]), (False, False))
            self.assertEqual((rows[0]["finalStatus"], rows[0]["failure"]), (143, "cancelled"))

    def test_post_output_original_end_refusal_keeps_actual_final_status_negative(self) -> None:
        """A completed write cannot authorize exit0 after the same original N has expired."""
        actual = self.main_case("output")
        self.assertEqual((actual["status"], actual["controlStatus"]), (124, 124))
        self.assertEqual(actual["restored"], [signal.SIGINT, signal.SIGTERM])
        self.assertEqual(actual["masks"], [0o077, 0o022])
        self.assertIs(actual["closed"], expr2=True)
        rows = cast("list[dict[str, object]]", actual["emitted"])
        self.assertEqual(len(rows), 1)
        self.assertIs(rows[0]["passed"], expr2=True)


@final
class InputFault:
    """Keep actual input writes and close before delivering distinct retained refusals."""

    def __init__(self, stream: BinaryIO, clock: list[float], first: BaseException) -> None:
        """Borrow an admitted actual stream, keeping ownership with the test's registry."""
        self.stream, self.clock, self.first = stream, clock, first
        self.close_count = 0

    def write(self, value: bytes) -> int:
        """Complete the actual buffered write before the injected original refusal."""
        guest.require(self.stream.write(value) == len(value))
        raise self.first

    def close(self) -> None:
        """Cross setup only after the original physically closes and before secondary refusal."""
        self.stream.close()
        self.close_count += 1
        self.clock[0] = 510
        message = "PRIVATE_SECONDARY_INPUT_CLOSE"
        raise OSError(message)


class RetainedFirstCauseTests(unittest.TestCase):
    """Run actual create_input/execute/main with lower inert roles and no global mutation."""

    def retained_case(self, primary: BaseException) -> dict[str, object]:
        """Keep the original write refusal through actual close, setup expiry and entry report."""
        clock = [0.0]
        controls: list[guest.Control] = []
        fixtures: list[LifecycleFixture] = []
        streams: list[BinaryIO] = []
        faults: list[InputFault] = []
        retained: list[BaseException] = []
        emitted: list[dict[str, object]] = []
        value: dict[str, object] = {
            "runId": "c" * 32,
            "revision": "b" * 40,
            "scenario": "positive",
            "sourcePins": dict.fromkeys(guest.GUEST_SOURCES, "a" * 64),
            "binary": {"sha256": "a" * 64, "bytes": 1},
            "setupSeconds": 510,
            "bodySeconds": 1680,
            "endSeconds": 1800,
        }
        adopt = stage.adopt

        def input_stream(descriptor: int, mode: str) -> BinaryIO:
            stream = adopt(descriptor, mode)
            streams.append(stream)
            fault = InputFault(stream, clock, primary)
            faults.append(fault)
            return cast("BinaryIO", cast("object", fault))

        def no_extra() -> None:
            controls[0].check()

        def emit(row: object) -> None:
            emitted.append(copy.deepcopy(guest.dictionary(row)))

        def make_wire(control: guest.Control) -> guest.Wire:
            controls.append(control)
            return cast(
                "guest.Wire",
                cast(
                    "object",
                    SimpleNamespace(
                        line=lambda: b"",
                        no_extra=no_extra,
                        close=peer.close,
                        emit=emit,
                    ),
                ),
            )

        def make_fixture(
            _value: object, control: guest.Control, _wire: guest.Wire
        ) -> guest.Fixture:
            fixture = LifecycleFixture(root, control, clock, body=False)
            fixtures.append(fixture)
            return cast(
                "guest.Fixture",
                cast(
                    "object",
                    SimpleNamespace(
                        prepare=prepare,
                        invoke=fixture.invoke,
                        observe=fixture.observe,
                        finish=fixture.finish,
                        native=fixture.native,
                        observed=fixture.observed,
                        unknown_effect=False,
                    ),
                ),
            )

        def prepare() -> None:
            control = controls[0]
            control.checkpoint = "render"
            try:
                with patch.object(stage, "adopt", side_effect=input_stream):
                    guest.create_input(root / "input", b"fixed-fixture-input", control)
            except BaseException as error:
                retained.append(error)
                raise

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            descriptor = os.open(root / "wire", os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
            peer = adopt(descriptor, "w+b")
            try:
                with (
                    patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
                    patch.object(signal, "signal", return_value=signal.SIG_DFL),
                    patch.object(os, "umask", return_value=0o022),
                    patch.object(guest, "Wire", side_effect=make_wire),
                    patch.object(guest, "request", return_value=value),
                    patch.object(guest, "Fixture", side_effect=make_fixture),
                ):
                    status = guest.main()
            finally:
                cleanup = fetch.settle(
                    (
                        peer.close,
                        *(stream.close for stream in streams),
                        *(stream.close for fixture in fixtures for stream in fixture.streams),
                    )
                )
            return {
                "status": status,
                "controlStatus": controls[0].status,
                "retained": retained,
                "emitted": emitted,
                "closeCount": [fault.close_count for fault in faults],
                "inputBytes": (root / "input").read_bytes(),
                "closed": peer.closed
                and all(stream.closed for stream in streams)
                and all(stream.closed for fixture in fixtures for stream in fixture.streams),
                "ownerCloseOrder": [fixture.closed for fixture in fixtures],
                "cleanup": cleanup,
            }

    def test_retained_input_io_or_policy_survives_secondary_close_and_setup_expiry(self) -> None:
        """Later status124 changes final status, never the already retained IO/policy cause."""
        for first, classification in (
            (OSError("PRIVATE_IO"), "io"),
            (ValueError("PRIVATE_POLICY"), "refused"),
        ):
            actual = self.retained_case(first)
            self.assertEqual(actual["retained"], [first])
            self.assertEqual((actual["status"], actual["controlStatus"]), (124, 124))
            self.assertEqual(actual["closeCount"], [1])
            self.assertEqual(actual["ownerCloseOrder"], [[0, 1, 2]])
            self.assertEqual(actual["inputBytes"], b"fixed-fixture-input")
            self.assertIs(actual["closed"], expr2=True)
            self.assertIsNone(actual["cleanup"])
            rows = cast("list[dict[str, object]]", actual["emitted"])
            self.assertEqual(len(rows), 1)
            self.assertEqual((rows[0]["passed"], rows[0]["cleanupPassed"]), (False, False))
            self.assertEqual((rows[0]["failure"], rows[0]["finalStatus"]), (classification, 124))
            self.assertNotIn("PRIVATE", data(rows).decode())


class ActorMutationTests(unittest.TestCase):
    """Keep final selection and repeated mutating effects separate from actor phase syntax."""

    def test_current_app_kill_stop_and_duplicate_actions_refuse_without_mutating_seen(self) -> None:
        """Only distinct old-generation action pairs may enter the bounded passive inventory."""
        current, prior = "a" * 64, "b" * 64
        for action in ("kill", "stop"):
            seen: set[tuple[str, str]] = set()
            with self.assertRaises(ValueError) as got:
                guest.actor_mutation(current, action, current, seen)
            self.assertEqual(got.exception.args, ("ops_guest_unproven",))
            self.assertEqual(seen, set())
        seen = set()
        for action in ("create", "start"):
            guest.actor_mutation(current, action, current, seen)
        for action in ("kill", "stop", "die", "destroy"):
            guest.actor_mutation(prior, action, current, seen)
        before = set(seen)
        for identifier, action in sorted(before):
            with self.subTest(identifier=identifier, action=action), self.assertRaises(ValueError):
                guest.actor_mutation(identifier, action, current, seen)
            self.assertEqual(seen, before)


def empty_fixture(control: guest.Control) -> guest.Fixture:
    """Construct only the inert fixed fixture's initial ownership registry."""
    value = {"runId": "c" * 32, "revision": "b" * 40, "scenario": "positive"}
    wire = cast("guest.Wire", cast("object", SimpleNamespace()))
    fixture = guest.Fixture(value, control, wire)
    fixture.commands = cast(
        "caddy.Commands",
        cast("object", SimpleNamespace(owned=[], tool_owners={}, create_uncertain=False)),
    )
    return fixture


class ObservationDebtTests(unittest.TestCase):
    """Verify fixed debt arithmetic and actual owner phase placement without daemon effects."""

    def guard_case(self, *, lead: int, late: bool) -> dict[str, object]:
        """Move only lower query return times under the same nested original ceilings."""
        clock = [100.0]
        control = guest.Control(0)
        control.setup, control.body, control.end = False, 1680, 1800
        fixture = empty_fixture(control)
        calls: list[tuple[str, float | None, bool]] = []
        first: BaseException | None = None

        def query(_commands: caddy.Commands, family: str, _table: str) -> None:
            control.check()
            calls.append((family, control.phase_end, control.settling))
            if lead:
                clock[0] += 3 if late else 2
            else:
                clock[0] += 6 if late else 4

        with (
            patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
            patch.object(caddy, "check_guard_retained", side_effect=query),
        ):
            try:
                with control.observation(guest.OBSERVATION):
                    clock[0] += lead
                    fixture.retained_guard()
            except BaseException as error:  # noqa: BLE001 -- Return actual phase refusal facts.
                first = error
        return {"failure": first, "calls": calls, "status": control.status, "end": control.end}

    def test_guard_queries_share_ten_seconds_clipped_by_original_observation120(self) -> None:
        """A second query cannot acquire another ten seconds or survive a late return."""
        expected = {
            "daemon": 15,
            "database": 15,
            "tls": 10,
            "application": 5,
            "source": 10,
            "collector": 15,
            "actors": 15,
            "network": 15,
            "guard": 10,
        }
        self.assertEqual(guest.OBSERVATION_CAPS, expected)
        self.assertEqual(sum(expected.values()) + guest.LOCAL_OBSERVATION_MARGIN, 120)
        self.assertEqual((guest.DEBT, guest.NATIVE, guest.OBSERVATION), (1170, 1050, 120))
        for lead, end in ((0, 110), (115, 220)):
            for late in (False, True):
                actual = self.guard_case(lead=lead, late=late)
                self.assertEqual(actual["calls"], [("inet", end, False), ("netdev", end, False)])
                self.assertEqual(actual["end"], 1800)
                if late:
                    self.assertIsInstance(actual["failure"], guest.ControlRefusalError)
                    self.assertEqual(actual["status"], 124)
                else:
                    self.assertIsNone(actual["failure"])
                    self.assertEqual(actual["status"], 0)

    def test_local_capture_retires_under_original_n_without_repeating_live_guard_queries(
        self,
    ) -> None:
        """Even an expired B cannot skip the actual local close or borrow M for daemon queries."""
        clock = [100.0]
        control = guest.Control(0)
        control.setup, control.body, control.end = False, 1680, 1800
        fixture = empty_fixture(control)
        guard_calls: list[tuple[str, bool]] = []
        retire_calls: list[tuple[bool, float, float]] = []
        first: BaseException | None = None

        def query(_commands: caddy.Commands, family: str, _table: str) -> None:
            control.check()
            guard_calls.append((family, control.settling))
            clock[0] += 4

        def retire() -> None:
            retire_calls.append((control.settling, control.end, control.remaining(120)))
            peer.close()
            control.check()

        with tempfile.TemporaryDirectory() as temporary:
            descriptor = os.open(
                Path(temporary) / "capture", os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600
            )
            peer = stage.adopt(descriptor, "w+b")
            fixture.watch = cast("caddy.Watch", cast("object", SimpleNamespace(finish=retire)))
            try:
                with (
                    patch.object(guest, "time", SimpleNamespace(monotonic=lambda: clock[0])),
                    patch.object(caddy, "check_guard_retained", side_effect=query),
                ):
                    with control.observation(guest.OBSERVATION):
                        fixture.retained_guard()
                    clock[0] = 1681
                    first = fetch.settle((fixture.finish,))
            finally:
                peer.close()
            closed = peer.closed
        self.assertIsInstance(first, guest.ControlRefusalError)
        self.assertEqual(control.status, 124)
        self.assertEqual(guard_calls, [("inet", False), ("netdev", False)])
        self.assertEqual(retire_calls, [(True, 1800, 119)])
        self.assertIs(closed, expr2=True)
        self.assertIs(fixture.observed["noObservedExternalPackets"], expr2=True)
        self.assertTrue(control.settling)
        self.assertEqual(control.end, 1800)
