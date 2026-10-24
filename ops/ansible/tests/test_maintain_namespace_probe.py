"""Exercise inert frame, descriptor and original-clock boundaries without Linux execution."""

from __future__ import annotations

import errno
import io
import json
import os
import platform
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

from test_support import ROOT, obj, objects, yaml_value

# isort: split
import maintain_namespace_probe as controller
import maintain_namespace_probe_guest as guest
import security_vm_ops_guest as peers
from security_tools import write_private


def decoded_record(raw: bytes | str) -> dict[str, object]:
    """Validate the test sink as an object without treating arbitrary JSON as a typed receipt."""
    value = cast("object", json.loads(raw))
    if not isinstance(value, dict):
        message = "missing fixture object"
        raise TypeError(message)
    return cast("dict[str, object]", value)


class NamespaceProbeBoundaryTests(unittest.TestCase):
    """Check negative protocol ownership; these tests establish no namespace-positive proof."""

    def test_manual_companion_job_has_no_auth_selector_or_required_workflow_role(self) -> None:
        """Keep the added real-run entry fixed, read-only and separate from release authority."""
        raw = (ROOT / ".github/workflows/maintenance-prerequisites.yml").read_text()
        workflow = yaml_value(raw, scalars_as_strings=True)
        self.assertEqual(set(obj(workflow, "on")), {"workflow_dispatch"})
        self.assertEqual(obj(workflow, "permissions"), {"contents": "read"})
        self.assertEqual(
            set(obj(workflow, "jobs")), {"caddy-prerequisites", "namespace-prerequisites"}
        )
        job = obj(workflow, "jobs", "namespace-prerequisites")
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertEqual(job["timeout-minutes"], "45")
        self.assertNotIn("needs", job)
        self.assertNotIn("permissions", job)
        guard = str(job["if"])
        for identity in ("workflow_dispatch", "refs/heads/main", "swiftraccoon/simplestChat"):
            self.assertIn(identity, guard)
        steps = objects(job, "steps")
        commands = " ".join(
            " ".join(str(row.get("run", "")).replace("\\\n", " ").split()) for row in steps
        )
        self.assertIn(
            '"$RUNNER_TEMP/namespace-controller/bin/python" -I -B '
            + 'build/maintain_namespace_probe.py --output "$RUNNER_TEMP/maintain-namespace-probe"',
            commands,
        )
        for forbidden in (
            "GH_TOKEN",
            "id-token:",
            "artifact_id",
            "server_artifact_id",
            "secrets.",
            "workflow_call:",
            "schedule:",
            "cargo build",
            "--host",
            "--command",
            "--case",
        ):
            self.assertNotIn(forbidden, raw)
        for row in steps:
            if "run" in row:
                self.assertNotIn("if", row)
        uploads = [row for row in steps if "upload-artifact@" in str(row.get("uses", ""))]
        self.assertEqual(len(uploads), 2)
        self.assertEqual(uploads[0]["if"], "${{ success() }}")
        retained = obj(uploads[0], "with")
        self.assertEqual(
            retained["path"], "${{ runner.temp }}/maintain-namespace-probe/summary.json"
        )
        self.assertEqual(retained["if-no-files-found"], "error")
        self.assertEqual(retained["retention-days"], "14")
        self.assertEqual(uploads[1]["if"], "${{ always() }}")
        self.assertEqual(
            obj(uploads[1], "with")["path"],
            "${{ runner.temp }}/maintain-namespace-probe/failure-summary.json",
        )
        self.assertEqual(obj(uploads[1], "with")["if-no-files-found"], "ignore")
        inherited = {
            str(row["uses"])
            for row in objects(workflow, "jobs", "caddy-prerequisites", "steps")
            if "uses" in row
        }
        self.assertEqual({str(row["uses"]) for row in steps if "uses" in row}, inherited)

    def test_closed_frame_rejects_duplicates_nonfinite_and_oversize(self) -> None:
        """Reject ambiguous/overflowed scalar facts before using a received frame."""
        for raw in (
            b'{"kind":"ready","kind":"result"}',
            b'{"end":NaN}',
            b'{"end":Infinity}',
            b'{"end":1e999}',
            b"[]",
            b"{}" + b" " * guest.MAX_FRAME,
        ):
            with self.subTest(raw=raw[:50]), self.assertRaises(guest.ProbeError):
                _ = guest.object_json(raw)
        self.assertEqual(
            guest.object_json(b'{"kind":"ready","end":1.25}'), {"kind": "ready", "end": 1.25}
        )

    def test_fixed_debian_metadata_profile_refuses_other_distribution_or_architecture(self) -> None:
        """Check supplied metadata only, without claiming actual Debian or namespace observation."""
        for identity, machine, accepted in (
            ({"ID": "debian", "VERSION_ID": "13"}, "x86_64", True),
            ({"ID": "ubuntu", "VERSION_ID": "24.04"}, "x86_64", False),
            ({"ID": "debian", "VERSION_ID": "12"}, "x86_64", False),
            ({"ID": "debian", "VERSION_ID": "13"}, "aarch64", False),
        ):
            with (
                self.subTest(identity=identity, machine=machine),
                patch.object(platform, "freedesktop_os_release", return_value=identity),
                patch.object(
                    os, "uname", return_value=os.uname_result(("Linux", "", "", "", machine))
                ),
            ):
                if accepted:
                    self.assertEqual(
                        guest.debian_identity(),
                        {"id": "debian", "versionId": "13", "architecture": "amd64"},
                    )
                else:
                    with self.assertRaises(guest.ProbeError):
                        _ = guest.debian_identity()

    def test_original_rights_close_before_ebadf_observation(self) -> None:
        """Retire both actual pipe descriptors and gather all close facts before assertions."""
        descriptors = os.pipe()
        first, closed = guest.retire_rights(descriptors)
        facts: list[int | None] = []
        for descriptor in descriptors:
            try:
                _ = os.fstat(descriptor)
            except OSError as error:
                facts.append(error.errno)
            else:
                facts.append(None)
        self.assertIsNone(first)
        self.assertTrue(closed)
        self.assertEqual(facts, [errno.EBADF, errno.EBADF])

    def test_body_end_is_observed_before_known_finalizers_widen(self) -> None:
        """Use explicit epochs to reject a late body despite available cleanup reserve."""
        for epoch in (0.0, 100.0, 10000.0):
            with self.subTest(epoch=epoch):
                clock = [epoch]

                def now(values: list[float] = clock) -> float:
                    return values[0]

                with patch.object(time, "monotonic", side_effect=now):
                    control = peers.Control(controller.BODY_SECONDS)
                    clock[0] = epoch + controller.BODY_SECONDS
                    first = controller.begin_finalization(control, None)
                    # Its reserved finalizer window is still open; the body refusal must remain.
                    control.check()
                    settling, status = control.settling, control.status
                self.assertIsInstance(first, peers.OpsError)
                self.assertTrue(settling)
                self.assertNotEqual(status, 0)

    def test_known_body_cause_survives_final_boundary_refusal(self) -> None:
        """Keep the original body object while still latching a late cleanup transition."""
        clock = [100.0]
        body = ValueError("fixed-body")
        with patch.object(time, "monotonic", side_effect=lambda: clock[0]):
            control = peers.Control(controller.BODY_SECONDS)
            clock[0] += controller.BODY_SECONDS
            first = controller.begin_finalization(control, body)
            settling, status = control.settling, control.status
        self.assertIs(first, body)
        self.assertTrue(settling)
        self.assertNotEqual(status, 0)

    def test_registration_and_restore_faults_still_restore_all_state(self) -> None:
        """Attempt the remaining handler and mask after a second registration/restore refusal."""
        restored: list[int] = []
        masks: list[int] = []
        original = signal.SIG_DFL
        failure = OSError("fixed-registration")

        def handler(
            number: int,
            value: signal.Handlers | object,
        ) -> signal.Handlers:
            if value == original:
                restored.append(number)
                if number == signal.SIGTERM:
                    message = "fixed-restoration"
                    raise OSError(message)
            elif number == signal.SIGTERM:
                raise failure
            return original

        def mask(value: int) -> int:
            masks.append(value)
            return 0o022

        with (
            patch.object(signal, "getsignal", return_value=original),
            patch.object(signal, "signal", side_effect=handler),
            patch.object(os, "umask", side_effect=mask),
            patch.object(time, "monotonic", return_value=100.0),
            patch.object(sys, "stderr", io.StringIO()),
            patch.object(controller, "run") as run,
        ):
            status = controller.main(["--output", str(ROOT / "unused-inert-output")])
        self.assertEqual(status, 1)
        run.assert_not_called()
        self.assertEqual(restored, [signal.SIGTERM, signal.SIGINT])
        self.assertEqual(masks, [0o077, 0o022])

    def test_reap_disarms_owner_before_record_overflow_refusal(self) -> None:
        """Decode one supplied wait result before a later capacity refusal; run no child."""
        consumed: list[tuple[int, int]] = []
        retired: list[bool] = []
        first: BaseException | None = None
        with (
            patch.object(os, "waitpid", side_effect=[(123, 0), ChildProcessError()]),
            patch.object(guest, "MAX_EVENTS", 0),
        ):
            try:
                _ = guest.reap_all(
                    100.0,
                    lambda: retired.append(True),
                    lambda pid, status: consumed.append((pid, status)),
                )
            except guest.ProbeError as error:
                first = error
        self.assertIsInstance(first, guest.ProbeError)
        self.assertEqual(consumed, [(123, 0)])
        self.assertEqual(retired, [])

    def test_negative_case_projection_never_exports_unknown_secret_fields(self) -> None:
        """Retain only named negative case/enum and actual input bytes, without nested raw data."""
        sentinel = "INERT-UNKNOWN-SECRET"
        run_id = "a" * 32
        value: dict[str, object] = {
            "schemaVersion": 1,
            "scope": "TestOnlyMaintainerNamespaceProbe",
            "runId": run_id,
            "passed": False,
            "failure": sentinel,
            "exception": "policy",
            "outerSignalStatus": 0,
            "tools": [{"opaque": sentinel}],
            "finalToolsUnchanged": False,
            "packages": {},
            "distribution": {},
            "kernel": sentinel,
            "cases": [{"name": guest.CASES[0], "passed": False, "failure": sentinel}],
            "actualLauncherExecuted": False,
        }
        raw = json.dumps(value).encode()
        state: dict[str, object] = {"checkpoint": "guest", "sshStatus": -15}
        state.update(controller.guest_failure_record(raw, run_id))
        body = ValueError(sentinel)
        controller.observe_failure(state, body, "guest")
        result = controller.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
        self.assertEqual(result["guestFailedCase"], guest.CASES[0])
        self.assertEqual(result["observedSshStatus"], -15)
        self.assertTrue(result["guestRowAccepted"])
        self.assertNotIn(sentinel, json.dumps(result))
        value["extraSecret"] = sentinel
        rejected = controller.guest_failure_record(json.dumps(value).encode(), run_id)
        self.assertFalse(rejected["guestRowAccepted"])
        self.assertNotIn(sentinel, json.dumps(rejected))

    def test_sdk_refusal_without_return_never_manufactures_signed_status(self) -> None:
        """Unknown post-close status remains null while its fixed first class survives cleanup."""
        state: dict[str, object] = {"checkpoint": "guest"}
        body = OSError("INERT-PRIVATE")
        controller.observe_failure(state, body, "guest")
        cleanup = ValueError("INERT-LATER")
        controller.observe_failure(state, cleanup, "cleanup")
        state["cleanupException"] = controller.exception_kind(cleanup)
        result = controller.failure_summary(state, {"cleanupPassed": False}, None, restored=True)
        self.assertIs(controller.first_error(state, None), body)
        self.assertIsNone(result["observedSshStatus"])
        self.assertEqual(result["exception"], "io")
        self.assertEqual(result["controllerCheckpoint"], "guest")
        self.assertEqual(result["cleanupException"], "decode")

    def test_namespace_partial_metadata_close_keeps_original_negative(self) -> None:
        """Attempt closed console metadata after an actual partial file is closed and refused."""
        body = OSError("INERT-NEVER-EXPORT")
        state: dict[str, object] = {"checkpoint": "guest"}
        controller.observe_failure(state, body, "guest")
        metadata = controller.failure_summary(state, {"cleanupPassed": True}, None, restored=True)
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
            output = Path(temporary).resolve()
            stamp = controller.directory_stamp(output.lstat())
            with (
                patch.object(controller, "write_private", side_effect=partial),
                patch.object(sys, "stderr", console),
            ):
                first = controller.retain_failure(
                    (output, stamp, time.monotonic() + 10), metadata, body
                )
            retained = (output / "failure-summary.json").read_bytes()
            emitted = console.getvalue()
        self.assertEqual(closed, [True])
        self.assertIs(first, body)
        self.assertNotIn("INERT-NEVER-EXPORT", emitted)
        self.assertNotIn(b"INERT-NEVER-EXPORT", retained)
        projected = decoded_record(emitted.removeprefix("maintenance-prerequisite-failure "))
        self.assertFalse(projected["passed"])
        self.assertEqual(projected["exception"], "io")

    def test_late_passive_namespace_summary_stays_negative_after_final_restoration(self) -> None:
        """Retain only closed console metadata when the original final end forbids a new file."""
        clock = [100.0]
        controllers: list[peers.Control] = []
        error = io.StringIO()

        def body(
            output: Path, control: peers.Control, state: dict[str, object]
        ) -> dict[str, object]:
            output.mkdir(mode=0o700)
            state["_outputStamp"] = controller.directory_stamp(output.lstat())
            controllers.append(control)
            control.settling = True
            write_private(output / "summary.json", b'{"passed":true}\n', 0o600)
            return {"passed": True, "cleanupPassed": True}

        old_mask = 0o022

        def mask(value: int) -> int:
            if value == old_mask:
                clock[0] = controllers[0].end + controller.FINAL_SECONDS
            return old_mask

        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "new"
            with (
                patch.object(controller, "run", side_effect=body),
                patch.object(time, "monotonic", side_effect=lambda: clock[0]),
                patch.object(signal, "getsignal", return_value=signal.SIG_DFL),
                patch.object(signal, "signal", return_value=signal.SIG_DFL),
                patch.object(os, "umask", side_effect=mask),
                patch.object(sys, "stderr", error),
            ):
                status = controller.main(["--output", str(output)])
            passive = (output / "summary.json").read_bytes()
            late_file = (output / "failure-summary.json").exists()
        self.assertEqual(status, 1)
        self.assertTrue(decoded_record(passive)["passed"])
        self.assertFalse(late_file)
        projected = decoded_record(
            error.getvalue().removeprefix("maintenance-prerequisite-failure ")
        )
        self.assertFalse(projected["passed"])
        self.assertEqual(projected["controllerCheckpoint"], "final-fence")

    def test_returned_nonzero_precedes_recursive_metadata_without_hiding_zero_body_failure(
        self,
    ) -> None:
        """Exercise the same returned observation helper used by run; no guest or namespace role."""
        raw = b'{"cases":' + b"[" * 1200 + b"0" + b"]" * 1200 + b"}\n"
        for status in (-9, 0):
            with self.subTest(status=status):
                state: dict[str, object] = {"checkpoint": "guest"}
                payload = raw if status else raw[:-1] + b"X\n"
                failure: BaseException | None = None
                try:
                    _ = controller.observe_guest(status, payload, "a" * 32, state)
                except (peers.OpsError, ValueError, RecursionError) as error:
                    failure = error
                self.assertEqual(state["sshStatus"], status)
                self.assertFalse(state["guestRowAccepted"])
                self.assertEqual(state["rowBytes"], len(payload))
                if status:
                    self.assertIsInstance(failure, peers.OpsError)
                    self.assertEqual(str(failure), "ops_guest_unproven")
                    self.assertIs(controller.first_error(state, None), failure)
                    self.assertEqual(state["exception"], "policy")
                    self.assertEqual(state["failedCheckpoint"], "guest")
                else:
                    self.assertIsInstance(failure, (ValueError, RecursionError))
                    self.assertIsNone(controller.first_error(state, None))

    def test_returned_nonzero_survives_secondary_diagnostic_failure(self) -> None:
        """Preserve exact negative object while a diagnostic adapter independently refuses."""
        state: dict[str, object] = {"checkpoint": "guest"}
        diagnostic = OSError("INERT-SECONDARY")
        with (
            patch.object(controller, "guest_failure_record", side_effect=diagnostic),
            self.assertRaises(peers.OpsError) as captured,
        ):
            _ = controller.observe_guest(-9, b"{}\n", "a" * 32, state)
        self.assertEqual(str(captured.exception), "ops_guest_unproven")
        self.assertIs(controller.first_error(state, None), captured.exception)
        self.assertEqual(state["exception"], "policy")


if __name__ == "__main__":
    _ = unittest.main()
