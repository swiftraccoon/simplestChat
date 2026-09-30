"""Keep owned boot diagnostics separate from release deployment proof, using no real VM."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_vm as vm
from security_findings import object_value
from security_tools import ToolError
from test_security_vm import (
    REVISION,
    FixtureObserver,
    RecordedHost,
    fixture_executable,
    object_json,
)

# isort: split
# Import only after the canonical security module installs the operations-helper path.
import bounded_process


class BootModeTests(unittest.TestCase):
    """Only an explicit no-artifact mode can skip the signed deployment path."""

    def test_boot_input_is_clean_source_only_and_rejects_any_artifact(self) -> None:
        """An app archive can neither be loaded nor accidentally certified by boot mode."""
        host = RecordedHost(Path("/unused"), time.monotonic() + 30)
        with patch.object(vm, "artifact_input") as artifact:
            self.assertEqual(vm.inputs(host, None, boot_only=True), {"revision": REVISION})
            self.assertEqual([name for name, _ in host.calls], ["source-revision", "source-clean"])
            with self.assertRaisesRegex(ToolError, "vm_boot_only_forbids_artifact"):
                _ = vm.inputs(host, Path("/artifact"), boot_only=True)
            with self.assertRaisesRegex(ToolError, "vm_deployment_requires_artifact"):
                _ = vm.inputs(host, None, boot_only=False)
            artifact.assert_not_called()
        with (
            patch.object(
                RecordedHost, "run", side_effect=[(0, REVISION.encode()), (0, b" M file")]
            ),
            self.assertRaisesRegex(ToolError, "vm_requires_clean_checkout"),
        ):
            _ = vm.inputs(host, None, boot_only=True)

    def test_boot_summary_never_claims_application_deployment(self) -> None:
        """A real tiny owned process tests cleanup; modeled boot is not actual guest evidence."""
        observed = FixtureObserver()
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(vm, "preflight"),
            patch.object(vm, "source_revision", return_value=REVISION),
            patch.object(vm, "artifact_input") as artifact,
            patch.object(vm, "prepare_disk"),
            patch.object(vm, "create_seed"),
            patch.object(vm, "fixture_inventory") as inventory,
            patch.object(
                vm,
                "qemu_command",
                return_value=[sys.executable, "-c", "import time; time.sleep(30)"],
            ),
            patch.object(vm, "boot") as boot,
            patch.object(vm, "exercise") as exercise,
            patch.object(vm, "Observer", return_value=observed),
        ):
            output = Path(temporary).resolve() / "result"
            result = vm.run(None, output, boot_only=True)
            self.assertTrue(result["passed"])
            self.assertTrue(result["cleanupPassed"])
            self.assertTrue(result["bootOnly"])
            self.assertEqual(result["scope"], "boot")
            self.assertFalse(result["fullDeploymentValidated"])
            self.assertNotIn("imageArchiveSha256", result)
            self.assertNotIn("phases", result)
            self.assertEqual(result, object_json((output / "summary.json").read_bytes()))
            boot.assert_called_once()
            artifact.assert_not_called()
            inventory.assert_not_called()
            exercise.assert_not_called()
            for failure, cleanup in (("vm_qmp_json", True), ("vm_qmp_cleanup_incomplete", False)):
                observed.failure = failure
                failed_output = output.with_name(failure)
                with self.subTest(failure=failure), self.assertRaisesRegex(ToolError, failure):
                    _ = vm.run(None, failed_output, boot_only=True)
                failed = object_json((failed_output / "summary.json").read_bytes())
                self.assertFalse(failed["passed"])
                self.assertFalse(failed["fullDeploymentValidated"])
                self.assertEqual(failed["cleanupPassed"], cleanup)

    def test_cli_requires_exactly_one_scope(self) -> None:
        """Missing mode or combined diagnostic/artifact arguments fail before any guest action."""
        for args in (
            ["--output", "/unused"],
            ["--boot-only", "--artifact-dir", "/artifact", "--output", "/unused"],
        ):
            with (
                self.subTest(args=args),
                patch.object(sys, "argv", ["security_vm.py", *args]),
                patch.object(sys, "stderr", io.StringIO()),
                patch.object(vm, "run") as run,
                self.assertRaises(SystemExit) as exited,
            ):
                _ = vm.main()
            self.assertEqual(exited.exception.code, 2)
            run.assert_not_called()

    def test_failed_exact_child_stop_still_closes_diagnostic_readers(self) -> None:
        """An injected stop error cannot leave the new observer or output pipes active."""
        observed = FixtureObserver()
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(vm, "Observer", return_value=observed),
            patch.object(FixtureObserver, "close", wraps=observed.close) as closed,
        ):
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(
                host,
                [sys.executable, "-c", "import time; time.sleep(30)"],
                qmp_path=Path(temporary) / "qmp.sock",
            )
            try:
                with (
                    patch.object(bounded_process, "stop", side_effect=ToolError("fixture_stop")),
                    self.assertRaisesRegex(ToolError, "fixture_stop"),
                ):
                    guest.close()
                closed.assert_called_once()
                self.assertFalse(guest.thread.is_alive())
                self.assertFalse(guest.closed)
                for stream in (guest.child.stdout, guest.child.stderr):
                    self.assertIsNotNone(stream)
                    if stream is not None:
                        self.assertTrue(stream.closed)
            finally:
                guest.close()

    def test_workflow_manual_mode_cannot_replace_scheduled_signed_validation(self) -> None:
        """Default/scheduled runs retain acquisition and full validation; boot has no artifact."""
        path = Path(__file__).resolve().parents[3] / ".github/workflows/infrastructure-security.yml"
        raw = cast("dict[object, object]", yaml.safe_load(path.read_text()))
        # PyYAML's YAML 1.1 safe loader decodes the workflow's `on` key as True.
        trigger = object_value(raw[True])
        manual = object_value(object_value(trigger["workflow_dispatch"])["inputs"])
        option = object_value(manual["boot_only"])
        self.assertEqual(option["type"], "boolean")
        self.assertIs(type(option["default"]), bool)
        self.assertFalse(option["default"])
        self.assertIn("schedule", trigger)
        job = object_value(object_value(raw["jobs"])["infrastructure"])
        self.assertIn("boot diagnostics", cast("str", job["name"]))
        steps = cast("list[object]", job["steps"])
        by_name = {
            cast("str", item["name"]): item
            for raw_step in steps
            if "name" in (item := object_value(raw_step))
        }
        for name in (
            "Acquire and verify the exact signed main release",
            "Apply twice and verify host policy and isolated restore",
        ):
            self.assertEqual(by_name[name]["if"], "${{ !inputs.boot_only }}")
        boot = by_name["Diagnose only the owned Debian guest boot"]
        self.assertEqual(
            boot["if"], "${{ github.event_name == 'workflow_dispatch' && inputs.boot_only }}"
        )
        self.assertIn("--boot-only", cast("str", boot["run"]))
        self.assertNotIn("--artifact-dir", cast("str", boot["run"]))


def cloud_report(**changes: object) -> bytes:
    """Model the exact four-stage cloud-init JSON shape with inert private-field canaries."""
    raw: dict[str, object] = {
        "status": "done",
        "extended_status": "done",
        "stage": None,
        "errors": [],
        "recoverable_errors": {},
        "detail": "private-fixture-datasource",
        "datasource": "private-fixture-key",
        **{
            name: {"errors": [], "recoverable_errors": {}, "start": 1, "finished": 2}
            for name in vm.CLOUD_STAGES
        },
        **changes,
    }
    return json.dumps(raw).encode()


class CloudInitTests(unittest.TestCase):
    """Retain actionable initialization counts without exposing raw cloud-init output."""

    def test_only_exit_zero_complete_healthy_state_passes(self) -> None:
        """Degraded, incomplete, disabled and stage errors remain blocking even with exit zero."""
        self.assertTrue(vm.cloud_status(cloud_report(), 0)["passed"])
        cases: tuple[tuple[int, dict[str, object]], ...] = (
            (2, {}),
            (1, {}),
            (0, {"status": "disabled", "extended_status": "disabled"}),
            (0, {"stage": "modules-final"}),
            (0, {"extended_status": "degraded done"}),
            (0, {"modules-final": {"errors": ["private-fixture"], "recoverable_errors": {}}}),
        )
        for status, changes in cases:
            with self.subTest(status=status, changes=changes):
                self.assertFalse(vm.cloud_status(cloud_report(**changes), status)["passed"])
        raw = object_json(cloud_report())
        del raw["init-local"]
        self.assertFalse(vm.cloud_status(json.dumps(raw).encode(), 0)["passed"])

    def test_degraded_report_keeps_only_classes_counts_and_known_status(self) -> None:
        """Invalid-seed and unknown warnings remain failures without publishing message text."""
        warning = "Invalid cloud-config provided: schema ssh_genkeytypes private-fixture-path"
        recoverable = {"WARNING": [warning, "private-fixture-other"]}
        data = cloud_report(
            extended_status="degraded done",
            recoverable_errors=recoverable,
            init={"errors": [], "recoverable_errors": recoverable},
        )
        result = vm.cloud_status(data, 2)
        self.assertFalse(result["passed"])
        self.assertEqual(result["extendedStatus"], "degraded done")
        aggregate = object_value(result["aggregate"])
        self.assertEqual(aggregate["recoverableErrors"], 2)
        self.assertEqual(aggregate["failureClasses"], ["schema_validation", "ssh"])
        self.assertEqual(aggregate["unclassifiedErrors"], 1)
        self.assertNotIn("private-fixture", json.dumps(result))
        self.assertNotIn("ssh_genkeytypes", json.dumps(result))

    def test_malformed_oversized_or_ambiguous_status_is_rejected(self) -> None:
        """Never scan through arbitrary prefixes or accept ambiguous/mistyped protocol fields."""
        for data in (
            b"..." + cloud_report(),
            b'{"status":"done","status":"private-fixture"}',
            cloud_report(status="private-fixture"),
            cloud_report(recoverable_errors={"WARNING": "private-fixture"}),
            cloud_report(errors=[True]),
            b" " * (vm.MAX_CLOUD_STATUS + 1),
        ):
            with self.subTest(size=len(data)), self.assertRaises((ToolError, ValueError)):
                _ = vm.cloud_status(data, 0)
        raw = object_json(cloud_report())
        del raw["stage"]
        with self.assertRaisesRegex(ToolError, "vm_cloud_init_stage"):
            _ = vm.cloud_status(json.dumps(raw).encode(), 0)

    def test_boot_retains_safe_status_before_raising_for_exit_two_or_invalid_json(self) -> None:
        """The final lifecycle receipt can diagnose failure after authenticated SSH."""
        cases = (
            (
                cloud_report(
                    extended_status="degraded done",
                    recoverable_errors={"WARNING": ["schema private-fixture"]},
                ),
                "vm_cloud_init_unhealthy",
                True,
            ),
            (b"private-fixture-invalid-json", "vm_cloud_init_report_invalid", False),
        )
        for data, failure, parsed in cases:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                host = vm.Host(work, time.monotonic() + 30)
                guest = vm.Guest(host, [sys.executable, "-c", "import time; time.sleep(30)"])
                try:
                    with (
                        patch.object(vm, "executable", side_effect=fixture_executable),
                        patch.object(vm.Host, "run", side_effect=[(0, b""), (2, data)]) as command,
                        self.assertRaisesRegex(ToolError, failure),
                    ):
                        vm.boot(host, guest, work, 22345)
                    command.assert_called_with(
                        "cloud-init",
                        [
                            "/usr/bin/ssh",
                            *vm.ssh_options(work),
                            "-i",
                            str(work / "client"),
                            "-p",
                            "22345",
                            "fixture@127.0.0.1",
                            "sudo -n cloud-init status --wait --format=json",
                        ],
                        timeout=180,
                        accepted=(0, 1, 2),
                    )
                    report = guest.diagnostics()
                    cloud = object_value(report["cloudInit"])
                    self.assertEqual(cloud["parsed"], parsed)
                    self.assertEqual(cloud["exitStatus"], 2)
                    self.assertTrue(report["authenticatedSsh"])
                    self.assertNotIn("private-fixture", json.dumps(report))
                finally:
                    guest.close()


class SerialPrivacyTests(unittest.TestCase):
    """Observe bounded boot stages without exposing log contents or accepting them as proof."""

    def test_split_serial_milestones_retain_no_private_values_and_end_at_authentication(
        self,
    ) -> None:
        """Chunk boundaries preserve fixed labels; SSH closes every public serial-data path."""
        with tempfile.TemporaryDirectory() as temporary:
            host = vm.Host(Path(temporary), time.monotonic() + 30)
            guest = vm.Guest(host, [sys.executable, "-c", "import time; time.sleep(30)"])
            try:
                guest.serial_capture(b"\x1b[H\x1b[JBooting `Debian GNU/")
                guest.serial_capture(b"Linux'\r\n")
                banner = object_value(guest.diagnostics()["startupSerial"])
                self.assertEqual(banner["milestones"], ["grub"])
                guest.serial_capture(b"SeaBIOS private-fixture-key\nBooting from hard disk\nGR")
                guest.serial_capture(b"UB\nLinux version private-host\ngrowroot resize2fs\n")
                guest.serial_capture(b"Kernel panic\n(initramfs)\nreboot: Restarting system\n")
                guest.serial_capture(b"Powering off\nCloud-init private-password\nssh.service\n")
                report = guest.diagnostics()
                serial = object_value(report["startupSerial"])
                for name in (
                    "firmware",
                    "boot_disk",
                    "grub",
                    "linux",
                    "disk_resize",
                    "kernel_panic",
                    "initramfs",
                    "reboot",
                    "poweroff",
                    "cloud_init",
                    "ssh",
                ):
                    self.assertIn(name, cast("list[str]", serial["milestones"]))
                self.assertNotIn("private-", json.dumps(report))
                self.assertLessEqual(len(guest.serial_tail), vm.BOOT_OVERLAP)
                guest.authenticated_ssh()
                guest.serial_capture(b"Linux version post-auth-private\n")
                self.assertEqual(guest.serial_tail, b"")
                self.assertFalse(guest.serial_milestones)
                self.assertNotIn("startupSerial", guest.diagnostics())
            finally:
                guest.close()


if __name__ == "__main__":
    _ = unittest.main()
