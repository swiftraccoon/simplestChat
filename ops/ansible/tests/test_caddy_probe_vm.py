"""Exercise prototype framing, real source-transfer files and inert outer ownership only."""

from __future__ import annotations

import ast
import io
import json
import os
import re
import shlex
import signal
import stat
import sys
import tarfile
import tempfile
import time
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch

from test_support import ROOT, obj, objects, yaml_value

# isort: split
import caddy_probe_guest as guest
import caddy_probe_vm as probe
import security_vm_fetch as fetch
import security_vm_receive as receive
from security_tools import ToolError, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from security_vm import Guest


def equal(observed: object, expected: object) -> None:
    """Keep assertion diagnostics free of raw guest-controlled values."""
    if observed != expected:
        message = "prototype fixture comparison failed"
        raise AssertionError(message)


def record() -> dict[str, object]:
    """Build the exact closed schema without suggesting a real guest has run."""
    return {
        "schemaVersion": 1,
        "scope": probe.GUEST_SCOPE,
        "passed": True,
        "cleanupPassed": True,
        "failure": None,
        "image": probe.GUEST_IMAGE,
        "sourceClosure": probe.SOURCE_CLOSURE,
        "materials": {},
        "observations": [],
        "containment": {},
        "qualification": probe.QUALIFICATION,
        "checkpoint": "complete",
        "exception": None,
        "cleanupException": None,
        "ioCheckpoint": None,
        "errnoCategory": None,
        "tlsCheckpoint": None,
        "tlsFailureCategory": None,
    }


def decoded_record(raw: bytes | str) -> dict[str, object]:
    """Validate the test sink as an object without treating arbitrary JSON as a typed receipt."""
    value = cast("object", json.loads(raw))
    if not isinstance(value, dict):
        message = "missing fixture object"
        raise TypeError(message)
    return cast("dict[str, object]", value)


def encoded(value: object) -> bytes:
    """Produce the compact row format used only by inert test inputs."""
    return json.dumps(value, separators=(",", ":")).encode() + b"\n"


def refused(action: Callable[[], object], code: str) -> None:
    """Require the exact fixed negative without echoing private exception data."""
    try:
        _ = action()
    except ToolError as error:
        equal(str(error), code)
    else:
        message = "prototype negative fixture admitted"
        raise AssertionError(message)


def arguments(output: Path) -> list[str]:
    """Select no host, image, key, inventory or arbitrary guest command."""
    return [
        "--project-root",
        str(ROOT),
        "--output",
        str(output),
        "--timeout-seconds",
        "1800",
        "--finalization-seconds",
        "120",
        "--guest-finalization-seconds",
        "60",
    ]


@final
class Console:
    """Retain only an in-memory public stdout sink; no actual descriptor or process."""

    def __init__(self) -> None:
        """Create the same buffer interface that main writes."""
        self.buffer = io.BytesIO()


@final
class InertGuest:
    """Model already-settled VM metadata only; never stand in for an actual runtime proof."""

    def __init__(self, status: object) -> None:
        """Keep signed status separate from the boolean closure facts."""
        self.status = status
        self.failure: str | None = None
        self.close_returned = False

    def close(self) -> None:
        """Record the actual shared finalizer's call without a process or VM."""
        self.close_returned = True

    def diagnostics(self) -> dict[str, object]:
        """Supply the existing owner's closed metadata schema."""
        return {
            "cleanupReturnCode": self.status,
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
class WrapperTests(unittest.TestCase):
    """Use no KVM, engine, SSH, network, subprocess or real signal dispatch."""

    def test_manual_workflow_is_fixed_readonly_and_not_a_required_or_auth_role(self) -> None:
        """Keep optional real execution distinct from artifact, host or native-product authority."""
        workflow_path = ROOT / ".github/workflows/maintenance-prerequisites.yml"
        raw = workflow_path.read_text()
        workflow = yaml_value(raw, scalars_as_strings=True)
        equal(set(obj(workflow, "on")), {"workflow_dispatch"})
        equal(obj(workflow, "permissions"), {"contents": "read"})
        job = obj(workflow, "jobs", "caddy-prerequisites")
        equal(job["runs-on"], "ubuntu-24.04")
        equal(job["timeout-minutes"], "45")
        steps = objects(job, "steps")
        commands = "\n".join(str(row.get("run", "")) for row in steps)
        for argument in (
            "--timeout-seconds 1800",
            "--finalization-seconds 120",
            "--guest-finalization-seconds 60",
        ):
            self.assertIn(argument, commands)
        for forbidden in (
            "GH_TOKEN",
            "id-token:",
            "artifact_id",
            "server_artifact_id",
            "cargo build",
            "secrets.",
            "workflow_call:",
            "schedule:",
        ):
            self.assertNotIn(forbidden, raw)
        uploads = [row for row in steps if "upload-artifact@" in str(row.get("uses", ""))]
        equal(len(uploads), 2)
        equal(
            obj(uploads[0], "with")["path"],
            "${{ runner.temp }}/maintenance-prerequisites/summary.json",
        )
        equal(uploads[0]["if"], "${{ success() }}")
        equal(
            obj(uploads[1], "with")["path"],
            "${{ runner.temp }}/maintenance-prerequisites/failure-summary.json",
        )
        equal(uploads[1]["if"], "${{ always() }}")
        equal(obj(uploads[1], "with")["if-no-files-found"], "ignore")
        actions = [str(row["uses"]) for row in steps if "uses" in row]
        existing = (ROOT / ".github/workflows/release-deploy-runtime.yml").read_text()
        for action in actions:
            self.assertIn(action, existing)

    def test_closed_guest_row_projects_only_validated_flags_and_hash(self) -> None:
        """Even nested unknown data is never copied to the public summary."""
        value = record()
        value["materials"] = {"private": "INERT-NEVER-PROJECT"}
        observed = probe.guest_record(encoded(value), probe.GUEST_IMAGE)
        equal(set(observed), {"passed", "cleanupPassed", "rowSha256"})
        equal("INERT-NEVER-PROJECT" in json.dumps(observed), expected=False)

    def test_guest_row_rejects_unknown_missing_duplicate_and_multiple_frames(self) -> None:
        """Strict framing and exact keys precede any guest-positive interpretation."""
        extra = record()
        extra["INERT-SECRET"] = "INERT-SECRET"
        missing = record()
        del missing["failure"]
        for raw in (encoded(extra), encoded(missing), encoded(record()) * 2, b"{}", b"{}\n\n"):
            refused(
                lambda raw=raw: probe.guest_record(raw, probe.GUEST_IMAGE), "caddy_vm_guest_record"
            )
        try:
            _ = probe.guest_record(b'{"passed":true,"passed":true}\n', probe.GUEST_IMAGE)
        except ValueError:
            pass
        else:
            message = "duplicate guest JSON admitted"
            raise AssertionError(message)

    def test_guest_identity_and_typed_statuses_cannot_be_forged(self) -> None:
        """Schema bools, wrong images and changed closure hashes cannot authorize success."""
        variants: list[tuple[str, object, str]] = [
            ("schemaVersion", True, "caddy_vm_guest_identity"),
            ("image", "inert-other", "caddy_vm_guest_identity"),
            ("scope", "inert-other", "caddy_vm_guest_identity"),
            ("sourceClosure", "0" * 64, "caddy_vm_guest_identity"),
            ("passed", 1, "caddy_vm_guest_refused"),
            ("cleanupPassed", False, "caddy_vm_guest_refused"),
            ("failure", "INERT-PRIVATE", "caddy_vm_guest_refused"),
            ("observations", {}, "caddy_vm_guest_refused"),
        ]
        for key, value, code in variants:
            changed = record()
            changed[key] = value
            refused(
                lambda changed=changed: probe.guest_record(encoded(changed), probe.GUEST_IMAGE),
                code,
            )

    def test_guest_row_bound_is_applied_before_json_decoding(self) -> None:
        """An oversized positive-looking frame remains a bounded refusal."""
        raw = b" " * probe.MAX_RECORD + b"\n"
        refused(lambda: probe.guest_record(raw, probe.GUEST_IMAGE), "caddy_vm_guest_record")

    def test_required_explicit_budget_domain_preserves_original_scaffold_ceiling(self) -> None:
        """No default budget silently creates a new whole clock or longer QEMU role."""
        with tempfile.TemporaryDirectory() as temporary:
            argv = arguments(Path(temporary) / "new")
            selected = probe.options(argv)
            equal((selected.timeout_seconds, selected.finalization_seconds), (1800, 120))
            for index, value in ((5, "1801"), (7, "1800"), (9, "1680"), (5, "nan"), (7, "0")):
                changed = list(argv)
                changed[index] = value
                with patch.object(sys, "stderr", io.StringIO()):
                    try:
                        _ = probe.options(changed)
                    except SystemExit as error:
                        equal(error.code, 2)
                    else:
                        message = "invalid explicit budget accepted"
                        raise AssertionError(message)

    def test_body_and_final_windows_share_original_entry_and_latched_signal(self) -> None:
        """Body work cannot borrow M; settling cannot clear a previously observed signal."""
        control = probe.Control(started=10, whole_seconds=100, finalization_seconds=20)
        with patch.object(time, "monotonic", return_value=90):
            refused(control.check, "fetch_vm_deadline")
            control.settling = True
            control.check()
            control.signal(signal.SIGTERM, None)
            control.check()
            equal(control.status, 143)
        with patch.object(time, "monotonic", return_value=110):
            refused(control.check, "fetch_vm_deadline")

    def test_command_settlement_reserve_is_inside_original_body(self) -> None:
        """A fixture too small for existing TERM/reap refuses before Host makes directories."""
        control = probe.Control(started=10, whole_seconds=100, finalization_seconds=20)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary).resolve() / "absent"
            with patch.object(time, "monotonic", return_value=80):
                refused(lambda: probe.Host(output, control), "caddy_vm_body_budget")
            exists = output.exists()
        equal(exists, expected=False)

    def test_final_live_body_fence_precedes_settlement_and_preserves_epoch(self) -> None:
        """A late actual body completion remains negative while every finalizer still runs."""
        control = probe.Control(started=10, whole_seconds=100, finalization_seconds=20)
        report: dict[str, object] = {"passed": False, "failure": None}
        with patch.object(time, "monotonic", return_value=90):
            probe.finish_run(fetch.Owners(), control, report, None)
        equal(report["passed"], expected=False)
        equal(report["cleanupPassed"], expected=True)
        equal(report["failure"], "body-completion")
        equal((control.started, control.body_end, control.end), (10, 90, 110))

    def test_actual_command_boundary_uses_held_ssh_after_ambient_alias_change(self) -> None:
        """Bootstrap/probe argv B cannot replace the retained original A at dispatch."""
        commands: list[list[str]] = []

        def lower(_name: str, argv: Sequence[str], **_options: object) -> tuple[int, bytes]:
            commands.append(list(argv))
            return 0, b"inert"

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            output = root / "output"
            output.mkdir(mode=0o700)
            program = root / "ssh"
            descriptor = os.open(program, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                _ = stream.write(b"inert never executed")
            control = probe.Control(whole_seconds=100, finalization_seconds=20)
            original = fetch.Held.open(program, probe.MAX_TOOL, control)
            try:
                host = probe.Host(output, control)
                host.fixed_tools["ssh"] = original
                with patch.object(receive.Host, "run", side_effect=lower):
                    result = host.run("guest-probe", ["/inert/replacement/ssh", "fixed-argument"])
                original.check(control)
            finally:
                original.close()
            closed = original.stream.closed
        equal(commands, [[str(program), "fixed-argument"]])
        equal(result, (0, b"inert"))
        equal(closed, expected=True)

    def test_transfer_archive_has_only_exact_four_private_source_members(self) -> None:
        """Real USTAR data uses retained original files and explicit private creation mode."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            control = probe.Control(whole_seconds=100, finalization_seconds=20)
            originals: dict[str, fetch.Held] = {}
            facts: list[tuple[str, int, bytes]] = []
            with ExitStack() as stack:
                for name in probe.TRANSFER_NAMES:
                    path = root / name
                    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    with os.fdopen(descriptor, "wb") as stream:
                        _ = stream.write(("inert source " + name).encode())
                    held = fetch.Held.open(path, probe.MAX_SOURCE, control)
                    _ = stack.callback(held.close)
                    key = probe.TRANSFER_SOURCES[name]
                    originals[key] = held
                output = root / "sources.tar"
                probe.transfer_archive(output, originals, control)
                with tarfile.open(output, "r:") as archive:
                    for member in archive:
                        stream = archive.extractfile(member)
                        if stream is None:
                            message = "source member missing"
                            raise TypeError(message)
                        with stream:
                            facts.append((member.name, member.mode, stream.read()))
                original_checks = [held.check(control) for held in originals.values()]
                mode = stat.S_IMODE(output.stat().st_mode)
            closed = [held.stream.closed for held in originals.values()]
        equal([name for name, _, _ in facts], list(probe.TRANSFER_NAMES))
        equal([mode for _, mode, _ in facts], [0o600] * 4)
        equal(mode, 0o600)
        equal(closed, [True] * 4)
        equal(original_checks, [None] * 4)

    def test_shared_vm_finalizer_requires_signed_zero_and_still_closes_inputs_and_work(
        self,
    ) -> None:
        """Late nonzero/signaled/unknown QEMU status cannot authorize this wrapper's success."""
        for status in (0, 7, -9, True, None):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                path = root / "input"
                descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(descriptor, "wb") as stream:
                    _ = stream.write(b"inert original input")
                control = probe.Control(whole_seconds=100, finalization_seconds=20)
                held = fetch.Held.open(path, probe.MAX_SOURCE, control)
                work = fetch.Work.create(root)
                guest = InertGuest(status)
                owners = fetch.Owners(
                    guest=cast("Guest", cast("object", guest)), inputs=[held], work=work
                )
                control.settling = True
                report: dict[str, object] = {}
                failure = owners.finish(control, report)
                closed = held.stream.closed
                work_removed = not work.path.exists()
                lifecycle = report["vmLifecycle"]
            equal(failure is None, type(status) is int and status == 0)
            equal(guest.close_returned, expected=True)
            equal(closed, expected=True)
            equal(work_removed, expected=True)
            if not isinstance(lifecycle, dict):
                message = "missing fixed VM lifecycle facts"
                raise TypeError(message)
            facts = cast("dict[str, object]", lifecycle)
            equal(facts["qemuReturnCode"], status if type(status) is int else None)

    def test_outer_summary_follows_handler_and_mask_restoration(self) -> None:
        """The actual main wrapper publishes only after all modeled outer restores return."""
        events: list[str] = []
        console = Console()

        def handler(_number: int, value: object) -> int:
            events.append("restore" if value == signal.SIG_DFL else "install")
            return signal.SIG_DFL

        def mask(_value: int) -> int:
            events.append("mask")
            return 0o027

        def body(_selected: probe.Options, control: probe.Control) -> dict[str, object]:
            events.append("body-owners-closed")
            control.settling = True
            return {"passed": True, "cleanupPassed": True}

        with tempfile.TemporaryDirectory() as temporary:
            with (
                patch.object(probe, "run", side_effect=body),
                patch.object(signal, "signal", side_effect=handler),
                patch.object(os, "umask", side_effect=mask),
                patch.object(sys, "stdout", console),
            ):
                status = probe.main(arguments(Path(temporary) / "new"))
            emitted = console.buffer.getvalue()
        equal(status, 0)
        equal(
            events,
            ["mask", "install", "install", "body-owners-closed", "restore", "restore", "mask"],
        )
        equal(cast("object", json.loads(emitted)), {"passed": True, "cleanupPassed": True})

    def test_restore_time_signal_invalidates_earlier_modeled_positive_without_public_row(
        self,
    ) -> None:
        """A latched late INT/TERM is rejected after every remaining restore attempt."""
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers: list[Callable[[int, object], None]] = []
            events: list[str] = []
            console = Console()

            def handler(
                _number: int,
                value: object,
                handlers: list[Callable[[int, object], None]] = handlers,
                events: list[str] = events,
            ) -> int:
                if callable(value):
                    handlers.append(cast("Callable[[int, object], None]", value))
                else:
                    events.append("restore")
                return signal.SIG_DFL

            def mask(
                _value: int,
                events: list[str] = events,
                handlers: list[Callable[[int, object], None]] = handlers,
                number: int = number,
            ) -> int:
                events.append("mask")
                if len(events) > 1:
                    handlers[0](number, None)
                return 0o027

            def body(_selected: probe.Options, control: probe.Control) -> dict[str, object]:
                control.settling = True
                return {"passed": True, "cleanupPassed": True}

            with (
                tempfile.TemporaryDirectory() as temporary,
                patch.object(probe, "run", side_effect=body),
                patch.object(signal, "signal", side_effect=handler),
                patch.object(os, "umask", side_effect=mask),
                patch.object(sys, "stdout", console),
                patch.object(sys, "stderr", io.StringIO()),
            ):
                status = probe.main(arguments(Path(temporary) / "new"))
            equal(status, 1)
            equal(events, ["mask", "restore", "restore", "mask"])
            equal(console.buffer.getvalue(), b"")

    def test_restore_failure_still_attempts_other_handler_and_mask(self) -> None:
        """Outer failure text remains fixed and no summary is promoted."""
        events: list[str] = []
        console = Console()

        def handler(_number: int, value: object) -> int:
            if not callable(value):
                events.append("restore")
                if events.count("restore") == 1:
                    message = "INERT-PRIVATE-FAILURE"
                    raise OSError(message)
            return signal.SIG_DFL

        def mask(_value: int) -> int:
            events.append("mask")
            return 0o027

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(probe, "run", return_value={"passed": True}),
            patch.object(signal, "signal", side_effect=handler),
            patch.object(os, "umask", side_effect=mask),
            patch.object(sys, "stdout", console),
            patch.object(sys, "stderr", io.StringIO()) as error,
        ):
            status = probe.main(arguments(Path(temporary) / "new"))
            message = error.getvalue()
        equal(status, 1)
        equal(events, ["mask", "restore", "restore", "mask"])
        equal(console.buffer.getvalue(), b"")
        self.assertNotIn("INERT-PRIVATE-FAILURE", message)
        prefix = "maintenance-prerequisite-failure "
        self.assertTrue(message.startswith(prefix))
        value = decoded_record(message.removeprefix(prefix))
        equal(value["passed"], expected=False)
        equal(value["restorationPassed"], expected=False)

    def test_negative_guest_projection_preserves_phase_and_signed_status_without_secrets(
        self,
    ) -> None:
        """Keep fixed negative facts while withholding unknown nested data."""
        sentinel = "INERT-UNKNOWN-SECRET"
        value = record()
        value.update(
            {
                "passed": False,
                "cleanupPassed": False,
                "failure": sentinel,
                "checkpoint": "natural-tls",
                "exception": "policy",
                "materials": {"opaque": sentinel},
                "observations": [{"opaque": sentinel}],
            }
        )
        raw = encoded(value)
        state: dict[str, object] = {"checkpoint": "guest"}
        admitted: list[tuple[int, ...]] = []

        def command(
            _name: str, _argv: Sequence[str], *, timeout: float, accepted: tuple[int, ...]
        ) -> tuple[int, bytes]:
            del timeout
            admitted.append(accepted)
            return -9, raw

        host = cast(
            "probe.Host",
            cast(
                "object",
                SimpleNamespace(
                    control=probe.Control(whole_seconds=100, finalization_seconds=20),
                    fixed_tools={"ssh": SimpleNamespace(path=Path("/never-executed"))},
                    diagnostic=state,
                    run=command,
                ),
            ),
        )
        selected = probe.Options(guest_finalization_seconds=10)
        with self.assertRaises(ToolError) as refused_status:
            _ = probe.invoke_guest(host, Path("/unused-inert"), 22, selected, "a" * 32)
        self.assertIn(-9, admitted[0])
        probe.observe_failure(state, refused_status.exception, "guest")
        projected = probe.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
        emitted = encoded(projected)
        equal(projected["observedSshStatus"], -9)
        equal(projected["guestCheckpoint"], "natural-tls")
        equal(projected["guestException"], "policy")
        equal(projected["guestRowAccepted"], expected=True)
        self.assertNotIn(sentinel.encode(), emitted)
        value["unknownSecret"] = sentinel
        refused_row = probe.guest_failure_record(encoded(value), probe.GUEST_IMAGE)
        equal(refused_row["guestRowAccepted"], expected=False)
        self.assertNotIn(sentinel.encode(), encoded(refused_row))

    def test_body_error_survives_actual_all_peer_vm_cleanup_refusal(self) -> None:
        """Keep body identity through actual input/work closes and a modeled QEMU refusal."""
        body = ToolError("INERT-PRIVATE-BODY")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            path = root / "input"
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                _ = stream.write(b"inert original")
            control = probe.Control(whole_seconds=100, finalization_seconds=20)
            held = fetch.Held.open(path, probe.MAX_SOURCE, control)
            work = fetch.Work.create(root)
            guest = InertGuest(-9)
            owners = fetch.Owners(
                guest=cast("Guest", cast("object", guest)), inputs=[held], work=work
            )
            state: dict[str, object] = {"checkpoint": "guest", "sshStatus": 1}
            report: dict[str, object] = {"failure": "guest"}
            probe.finish_run(owners, control, report, body, state)
            facts = (held.stream.closed, not work.path.exists(), guest.close_returned)
            original = probe.first_error(state, None)
            projected = probe.failure_summary(state, report, None, restored=True)
        equal(facts, (True, True, True))
        self.assertIs(original, body)
        equal(projected["controllerCheckpoint"], "guest")
        equal(projected["exception"], "policy")
        equal(projected["cleanupPassed"], expected=False)
        self.assertIsNotNone(projected["cleanupException"])

    def test_failure_writer_partial_close_keeps_body_and_attempts_closed_console(self) -> None:
        """Close actual partial metadata before a secondary refusal; retain the body object."""
        body = ToolError("INERT-NEVER-EXPORT")
        state: dict[str, object] = {"checkpoint": "guest"}
        probe.observe_failure(state, body, "guest")
        metadata = probe.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
        closed: list[bool] = []
        console = io.StringIO()

        def partial(path: Path, data: bytes, mode: int) -> None:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
            stream = os.fdopen(descriptor, "wb")
            try:
                _ = stream.write(data[:12])
                stream.flush()
            finally:
                stream.close()
                closed.append(stream.closed)
            message = "INERT-NEVER-EXPORT"
            raise OSError(message)

        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve()
            stamp = fetch.directory_stamp(path.lstat())
            with (
                patch.object(probe, "write_private", side_effect=partial),
                patch.object(sys, "stderr", console),
            ):
                first = probe.retain_failure((path, stamp, time.monotonic() + 10), metadata, body)
            partial_bytes = (path / "failure-summary.json").read_bytes()
            emitted = console.getvalue()
        equal(closed, [True])
        self.assertIs(first, body)
        self.assertNotIn("INERT-NEVER-EXPORT", emitted)
        self.assertNotIn(b"INERT-NEVER-EXPORT", partial_bytes)
        projected = decoded_record(emitted.removeprefix("maintenance-prerequisite-failure "))
        equal(projected["passed"], expected=False)

    def test_late_positive_output_never_becomes_success_or_unbounded_failure_io(self) -> None:
        """Earlier positive bytes stay passive when the original end expires during output flush."""
        clock = [100.0]
        console = Console()
        error = io.StringIO()
        control_holder: list[probe.Control] = []
        original_write = write_private

        def body(selected: probe.Options, control: probe.Control) -> dict[str, object]:
            selected.output.mkdir(mode=0o700)
            selected.output_stamp = fetch.directory_stamp(selected.output.lstat())
            control_holder.append(control)
            control.settling = True
            return {"passed": True, "cleanupPassed": True}

        def write(path: Path, data: bytes, mode: int) -> None:
            original_write(path, data, mode)
            clock[0] = control_holder[0].end

        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "new"
            with (
                patch.object(probe, "run", side_effect=body),
                patch.object(probe, "write_private", side_effect=write),
                patch.object(time, "monotonic", side_effect=lambda: clock[0]),
                patch.object(signal, "signal", return_value=signal.SIG_DFL),
                patch.object(os, "umask", return_value=0o027),
                patch.object(sys, "stdout", console),
                patch.object(sys, "stderr", error),
            ):
                status = probe.main(arguments(output))
            passive = (output / "summary.json").read_bytes()
            late_file = (output / "failure-summary.json").exists()
        equal(status, 1)
        equal(decoded_record(passive)["passed"], expected=True)
        equal(late_file, expected=False)
        projected = decoded_record(
            error.getvalue().removeprefix("maintenance-prerequisite-failure ")
        )
        equal(projected["passed"], expected=False)
        equal(projected["controllerCheckpoint"], "final-fence")
        equal(projected["finalFencePassed"], expected=False)

    def test_returned_nonzero_precedes_deep_metadata_but_zero_still_validates_body(self) -> None:
        """Exercise the actual invoke boundary with supplied status/bytes and no child execution."""
        raw = b'{"materials":' + b"[" * 1200 + b"0" + b"]" * 1200 + b"}\n"
        for status in (-9, 0):
            with self.subTest(status=status):
                state: dict[str, object] = {"checkpoint": "guest"}
                payload = raw if status else raw[:-1] + b"X\n"

                def command(
                    _name: str,
                    _argv: Sequence[str],
                    *,
                    timeout: float,
                    accepted: tuple[int, ...],
                    observation: tuple[int, bytes] = (status, payload),
                ) -> tuple[int, bytes]:
                    del timeout, accepted
                    return observation

                host = cast(
                    "probe.Host",
                    cast(
                        "object",
                        SimpleNamespace(
                            control=probe.Control(whole_seconds=100, finalization_seconds=20),
                            fixed_tools={"ssh": SimpleNamespace(path=Path("/never-executed"))},
                            diagnostic=state,
                            run=command,
                        ),
                    ),
                )
                failure: BaseException | None = None
                try:
                    _ = probe.invoke_guest(
                        host,
                        Path("/unused"),
                        22,
                        probe.Options(guest_finalization_seconds=10),
                        "a" * 32,
                    )
                except (ToolError, ValueError, RecursionError) as error:
                    failure = error
                self.assertEqual(state["sshStatus"], status)
                self.assertFalse(state["guestRowAccepted"])
                self.assertEqual(state["rowBytes"], len(payload))
                if status:
                    self.assertIsInstance(failure, ToolError)
                    self.assertEqual(str(failure), "caddy_vm_guest_status")
                    self.assertIs(probe.first_error(state, None), failure)
                    self.assertEqual(state["exception"], "policy")
                    self.assertEqual(state["failedCheckpoint"], "guest")
                else:
                    self.assertIsInstance(failure, (ValueError, RecursionError))
                    self.assertIsNone(probe.first_error(state, None))

    def test_returned_nonzero_survives_secondary_diagnostic_failure(self) -> None:
        """A diagnostic adapter refusal never replaces the already observed negative status."""
        state: dict[str, object] = {"checkpoint": "guest"}

        def command(
            _name: str, _argv: Sequence[str], *, timeout: float, accepted: tuple[int, ...]
        ) -> tuple[int, bytes]:
            del timeout, accepted
            return -9, b"{}\n"

        host = cast(
            "probe.Host",
            cast(
                "object",
                SimpleNamespace(
                    control=probe.Control(whole_seconds=100, finalization_seconds=20),
                    fixed_tools={"ssh": SimpleNamespace(path=Path("/never-executed"))},
                    diagnostic=state,
                    run=command,
                ),
            ),
        )
        diagnostic = OSError("INERT-SECONDARY")
        with (
            patch.object(probe, "guest_failure_record", side_effect=diagnostic),
            self.assertRaises(ToolError) as captured,
        ):
            _ = probe.invoke_guest(
                host,
                Path("/unused"),
                22,
                probe.Options(guest_finalization_seconds=10),
                "a" * 32,
            )
        self.assertEqual(str(captured.exception), "caddy_vm_guest_status")
        self.assertIs(probe.first_error(state, None), captured.exception)
        self.assertEqual(state["exception"], "policy")

    def test_short_or_refused_output_keeps_unsampled_final_fence_false(self) -> None:
        """Exercise actual publication branches after a modeled body, without any VM execution."""

        class Sink:
            """Keep real in-memory bytes while injecting one fixed output refusal."""

            def __init__(self, mode: str) -> None:
                """Select only a local test fault, never a program/host/runtime role."""
                self.mode: str = mode
                self.bytes: io.BytesIO = io.BytesIO()

            def write(self, data: bytes) -> int:
                """Perform a real short/full write or fail after retaining a partial prefix."""
                count = len(data) - 1 if self.mode in {"short", "write"} else len(data)
                written = self.bytes.write(data[:count])
                if self.mode == "write":
                    message = "INERT-OUTPUT-REFUSAL"
                    raise OSError(message)
                return written

            def flush(self) -> None:
                """Flush before a possible fixed failure; no close/fence fact is fabricated."""
                self.bytes.flush()
                if self.mode == "flush":
                    message = "INERT-FLUSH-REFUSAL"
                    raise OSError(message)

        for mode in ("short", "write", "flush"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "new"
                sink = Sink(mode)
                error = io.StringIO()

                def body(selected: probe.Options, control: probe.Control) -> dict[str, object]:
                    selected.output.mkdir(mode=0o700)
                    selected.output_stamp = fetch.directory_stamp(selected.output.lstat())
                    control.settling = True
                    return {"passed": True, "cleanupPassed": True}

                with (
                    patch.object(probe, "run", side_effect=body),
                    patch.object(signal, "signal", return_value=signal.SIG_DFL),
                    patch.object(os, "umask", return_value=0o027),
                    patch.object(sys, "stdout", SimpleNamespace(buffer=sink)),
                    patch.object(sys, "stderr", error),
                ):
                    status = probe.main(arguments(output))
                retained = decoded_record((output / "failure-summary.json").read_bytes())
                actual_bytes = sink.bytes.getvalue()
                sink.bytes.close()
                closed = sink.bytes.closed
            self.assertEqual(status, 1)
            self.assertTrue(closed)
            self.assertTrue(actual_bytes)
            self.assertFalse(retained["passed"])
            self.assertFalse(retained["finalFencePassed"])
            self.assertEqual(retained["controllerCheckpoint"], "summary-output")
            self.assertNotIn("INERT-OUTPUT-REFUSAL", error.getvalue())
            self.assertNotIn("INERT-FLUSH-REFUSAL", error.getvalue())

    def test_bootstrap_explicitly_installs_each_required_tool_provider_without_recommends(
        self,
    ) -> None:
        """Exercise stage_guest argv construction without SSH, APT or package execution."""
        installed: set[str] = set()
        observed: list[str] = []

        def command(name: str, argv: Sequence[str], *, timeout: float = 120) -> tuple[int, bytes]:
            del timeout
            observed.append(name)
            if name == "guest-install":
                remote = shlex.split(argv[-1])
                words = shlex.split(remote[-1])
                self.assertIn("--no-install-recommends", words)
                start = words.index("install") + 1
                installed.update(word for word in words[start:] if not word.startswith("-"))
                return 0, b""
            message = "inert stop before source/VM admission"
            raise ToolError(message)

        host = cast(
            "probe.Host",
            cast(
                "object",
                SimpleNamespace(
                    fixed_tools={"ssh": SimpleNamespace(path=Path("/never-executed"))},
                    run=command,
                ),
            ),
        )
        with self.assertRaises(ToolError):
            _ = probe.stage_guest(host, Path("/unused"), 22, "a" * 32, {})
        self.assertEqual(observed, ["guest-install", "guest-directory"])
        providers = {
            "docker": "docker-cli",
            "nft": "nftables",
            "tcpdump": "tcpdump",
            "ip": "iproute2",
            "update-ca-certificates": "ca-certificates",
        }
        self.assertEqual(
            {role for role, package in providers.items() if package in installed}, set(providers)
        )
        self.assertIn("docker.io", installed)

    def test_reviewed_static_refusal_codes_project_and_unknown_error_text_never_does(self) -> None:
        """Bind the literal source catalog and admit only its exact members to output."""
        tree = ast.parse((ROOT / "build/caddy_probe_guest.py").read_bytes())
        literals: set[str] = set()
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Name)
                and node.func.id in {"require", "refuse"}
            ):
                for argument in node.args:
                    if isinstance(argument, ast.Constant) and isinstance(argument.value, str):
                        literals.add(argument.value)
        self.assertEqual(probe.GUEST_REFUSAL_CODES, literals)
        self.assertIn("tool_missing", literals)
        for code, expected in (("tool_missing", "tool_missing"), ("INERT-PRIVATE-UNKNOWN", None)):
            value = record()
            value.update(
                {
                    "passed": False,
                    "failure": code,
                    "checkpoint": "guest-prerequisites",
                    "exception": "policy",
                }
            )
            state: dict[str, object] = {"checkpoint": "guest", "sshStatus": 1}
            state.update(probe.guest_failure_record(encoded(value), probe.GUEST_IMAGE))
            projected = probe.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
            self.assertEqual(projected["guestRefusalCode"], expected)
            self.assertNotIn("INERT-PRIVATE-UNKNOWN", json.dumps(projected))
        self.assertEqual(guest.PHASES, probe.GUEST_PHASES)

    def test_experiment_choice_is_closed_defaultboth_and_unknown_cannot_admit_a_job(self) -> None:
        """Parse exact fixed guard vocabulary without evaluating arbitrary expressions."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/maintenance-prerequisites.yml").read_text(),
            scalars_as_strings=True,
        )
        choice = obj(workflow, "on", "workflow_dispatch", "inputs", "experiment")
        self.assertEqual(choice["type"], "choice")
        self.assertEqual(choice["required"], "true")
        self.assertEqual(choice["default"], "both")
        self.assertEqual(choice["options"], ["both", "caddy", "namespace"])
        prefix = (
            "${{ github.event_name == 'workflow_dispatch' && "
            + "github.ref == 'refs/heads/main' && "
            + "github.repository == 'swiftraccoon/simplestChat' && "
        )
        admitted: dict[str, set[str]] = {}
        for name, expected in (
            ("caddy-prerequisites", "caddy"),
            ("namespace-prerequisites", "namespace"),
        ):
            guard = " ".join(str(obj(workflow, "jobs", name)["if"]).split())
            suffix = f"(inputs.experiment == 'both' || inputs.experiment == '{expected}') }}}}"
            self.assertEqual(guard, prefix + suffix)
            values = re.findall(r"inputs\.experiment == '(both|caddy|namespace)'", guard)
            admitted[name] = set(values)
        self.assertEqual(
            {name for name, values in admitted.items() if "both" in values}, set(admitted)
        )
        for invalid in ("", "unknown", "caddy;exit", "../namespace"):
            self.assertFalse(any(invalid in values for values in admitted.values()))

    def test_containment_io_checkpoint_and_errno_are_closed_not_strerror_or_paths(self) -> None:
        """Expose fixed kernel categories/local steps without ever projecting hostile strings."""
        for checkpoint, category, expected in (
            ("containment-marker-send", "EPERM", ("containment-marker-send", "EPERM")),
            ("INERT-PRIVATE-PATH", "EPERM:/INERT/PRIVATE", (None, None)),
        ):
            value = record()
            value.update(
                {
                    "passed": False,
                    "checkpoint": "containment",
                    "exception": "io",
                    "failure": "INERT-PRIVATE",
                    "ioCheckpoint": checkpoint,
                    "errnoCategory": category,
                }
            )
            state: dict[str, object] = {"checkpoint": "guest", "sshStatus": 1}
            state.update(probe.guest_failure_record(encoded(value), probe.GUEST_IMAGE))
            projected = probe.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
            self.assertEqual(
                (projected["guestIoCheckpoint"], projected["guestErrnoCategory"]), expected
            )
            self.assertNotIn("INERT-PRIVATE", json.dumps(projected))
        self.assertEqual(guest.IO_CHECKPOINTS, probe.GUEST_IO_CHECKPOINTS)
        self.assertEqual(
            set(guest.ERRNO_CATEGORIES.values()) | {"UNKNOWN", "OTHER"},
            probe.GUEST_ERRNO_CATEGORIES,
        )


class TlsFailureProjectionTests(unittest.TestCase):
    """Keep transport diagnostics closed even if guest metadata is hostile."""

    def test_only_fixed_tls_categories_survive_and_only_in_tls_phases(self) -> None:
        """Never project arbitrary addresses, names, certificate details or exception messages."""
        for phase, stage, category, expected in (
            ("natural-tls", "connect", "connection-refused", ("connect", "connection-refused")),
            (
                "fresh-process",
                "handshake",
                "certificate-verification",
                ("handshake", "certificate-verification"),
            ),
            ("natural-tls", "INERT-PRIVATE-IP", {"secret": "INERT-PRIVATE"}, (None, None)),
            ("containment", "connect", "timeout", (None, None)),
        ):
            value = record()
            value.update(
                {
                    "passed": False,
                    "checkpoint": phase,
                    "exception": "io",
                    "failure": "tls_not_ready",
                    "tlsCheckpoint": stage,
                    "tlsFailureCategory": category,
                }
            )
            state: dict[str, object] = {"checkpoint": "guest", "sshStatus": 1}
            state.update(probe.guest_failure_record(encoded(value), probe.GUEST_IMAGE))
            observed = probe.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
            self.assertEqual(
                (observed["guestTlsCheckpoint"], observed["guestTlsFailureCategory"]), expected
            )
            self.assertNotIn("INERT-PRIVATE", json.dumps(observed))
        self.assertEqual(guest.TLS_CHECKPOINTS, probe.GUEST_TLS_CHECKPOINTS)
        self.assertEqual(guest.TLS_FAILURE_CATEGORIES, probe.GUEST_TLS_FAILURE_CATEGORIES)


if __name__ == "__main__":
    _ = unittest.main()
