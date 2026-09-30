"""Offline controller, archive and recovery tests; no SSH or service actions."""

from __future__ import annotations

import gzip
import io
import json
import os
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from contextlib import ExitStack, contextmanager, redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bootstrap_controller as bootstrap
import release_build as build
import vps_capacity_controller as controller
import vps_capacity_remote as remote
from bootstrap_access import BootstrapError

if TYPE_CHECKING:
    from collections.abc import Generator, Sequence

REVISION = "a" * 40
RUN = "b" * 32
SECRET = "PRIVATE_TRANSPORT_SENTINEL"  # noqa: S105 -- Public fixture tests data/argv separation.


@dataclass(frozen=True)
class Entry:
    """One deliberately small archive fixture, including invalid entry kinds."""

    name: str
    body: bytes = b"evidence"
    kind: bytes = tarfile.REGTYPE
    link: str = ""


def archive_at(path: Path, entries: Sequence[Entry]) -> None:
    """Write a local fixture archive; no extractor writes outside its test directory."""
    with tarfile.open(path, "w:gz") as archive:
        for entry in entries:
            member = tarfile.TarInfo(entry.name)
            member.type = entry.kind
            member.linkname = entry.link
            member.mode = 0o777
            member.size = len(entry.body) if entry.kind == tarfile.REGTYPE else 0
            archive.addfile(member, io.BytesIO(entry.body) if member.isfile() else None)


class ControllerTests(unittest.TestCase):
    """Validate the public controller boundary using an isolated filesystem."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Declare fixture state before unittest setup."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.target: bootstrap.BootstrapTarget = bootstrap.BootstrapTarget("", "", 22, ROOT, ROOT)
        self.args: controller.Options = controller.Options()
        self.actions: list[str] = []
        self.commands: list[list[str]] = []
        self.lease: bool = False
        self.verdict: bool = True
        self.failed_action: str | None = None
        self.collected_request: dict[str, object] | None = None
        self.build_failed: bool = False

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve() / "checkout"
        (self.root / "build").mkdir(parents=True)
        (self.root / "ops/ansible/files").mkdir(parents=True)
        _ = (self.root / "build/vps_capacity_remote.py").write_text("# committed fixture\n")
        _ = (self.root / "ops/ansible/files/bounded_process.py").write_text(
            "# committed process fixture\n"
        )
        private = self.root.parent / "private"
        private.mkdir(mode=0o700)
        identity, known = private / "key", private / "known_hosts"
        bootstrap.write_new(identity, b"fixture key")
        bootstrap.write_new(known, b"fixture host trust")
        known.chmod(0o644)
        self.target = bootstrap.BootstrapTarget(
            "test.example.invalid", "debian", 22, identity, known
        )
        inventory = self.root / "inventory.yml"
        bootstrap.write_new(inventory, json.dumps(self.inventory()).encode())
        self.args = controller.Options(
            inventory=str(inventory),
            limit="test_vps",
            revision=REVISION,
            label="audio30",
            audio_only=True,
            chat_interval_ms=30000,
            runtime_seconds=1200,
            server_cpus=1,
            generator_cpus=4.8,
            app_cpus=5,
            port_mbps=2000,
        )
        self.actions = []
        self.commands = []
        self.lease = False
        self.verdict = True
        self.failed_action = None
        self.collected_request = None
        self.build_failed = False

    def cli(self, *extra: str) -> list[str]:
        """Return only public selectors for the isolated fixture inventory."""
        return [
            "--inventory",
            self.args.inventory,
            "--limit",
            "test_vps",
            "--revision",
            REVISION,
            *extra,
        ]

    def inventory(self, **overrides: object) -> dict[str, object]:
        """Model the sole accepted static one-host inventory without running Ansible."""
        variables: dict[str, object] = {
            "ansible_host": self.target.host,
            "ansible_user": self.target.user,
            "ansible_ssh_private_key_file": str(self.target.identity),
            "scbench_ssh_known_hosts_file": str(self.target.known_hosts),
            "scbench_revision": "c" * 40,
            "scpub_enabled": False,
            "scpub_port_mbps": 2000,
            **overrides,
        }
        return {"benchmark_hosts": {"hosts": {"test_vps": variables}}}

    def action(
        self,
        _transport: controller.Transport,
        request: dict[str, object],
        *,
        timeout: int = 60,
    ) -> dict[str, object]:
        """Simulate the remote protocol while recording all requested side effects."""
        action = remote.text(request["action"])
        self.actions.append(action)
        self.assertGreater(timeout, 0)
        if self.failed_action == action:
            message = "fixture_transport_failure"
            raise remote.CapacityControlError(message)
        if action == "status":
            return {
                "ActiveState": "inactive",
                "Result": "failed" if self.build_failed else "success",
                "ExecMainStatus": "1" if self.build_failed else "0",
            }
        if action == "collect":
            return {"passed": self.verdict, "request": self.collected_request or request}
        return {"privateHost": True, "finalized": True}

    @contextmanager
    def reserve(
        self,
        _transport: controller.Transport,
        _request: dict[str, object],
    ) -> Generator[None]:
        """Assert preparation uses a continuously held lease."""
        self.actions.append("reserve")
        self.lease = True
        try:
            yield
        finally:
            self.lease = False
            self.actions.append("release")

    def ansible(
        self,
        _runner: build.Runner,
        argv: Sequence[str],
        **_kwargs: object,
    ) -> tuple[int, str]:
        """Record the exact preparation invocation while requiring the lease."""
        self.assertTrue(self.lease)
        self.commands.append(list(argv))
        return 0, ""

    def download(
        self,
        _transport: controller.Transport,
        _request: dict[str, object],
        destination: Path,
    ) -> None:
        """Retain a real tiny archive so execute exercises extraction and hashing."""
        self.actions.append("archive")
        archive_at(destination, [Entry("result.json", b'{"retained":true}')])

    @contextmanager
    def fake_host(self) -> Generator[None]:
        """Replace all external boundaries while retaining actual local evidence IO."""

        def output_path(path: Path, _root: Path, _runner: build.RunnerProtocol) -> Path:
            return path

        with ExitStack() as stack:
            _ = stack.enter_context(patch.object(build, "clean_revision", return_value=REVISION))
            _ = stack.enter_context(patch.object(build, "validate_output", side_effect=output_path))
            _ = stack.enter_context(
                patch.object(controller.Transport, "call", autospec=True, side_effect=self.action)
            )
            _ = stack.enter_context(
                patch.object(
                    controller.Transport, "reserve", autospec=True, side_effect=self.reserve
                )
            )
            _ = stack.enter_context(
                patch.object(
                    controller.Transport, "download", autospec=True, side_effect=self.download
                )
            )
            _ = stack.enter_context(
                patch.object(build.Runner, "run", autospec=True, side_effect=self.ansible)
            )
            yield

    def test_actual_help_entrypoint_resolves_imports_without_test_support(self) -> None:
        """The command must work outside the test suite's modified module search path."""
        result = subprocess.run(  # noqa: S603 -- Local help-only command.
            [sys.executable, str(ROOT / "build/run-vps-capacity.py"), "--help"],
            cwd=self.root.parent,
            env={key: value for key, value in os.environ.items() if key != "PYTHONPATH"},
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--prepare", result.stdout)
        self.assertIn("--recover", result.stdout)

    def test_bounds_are_checked_before_missing_inventory_is_opened(self) -> None:
        """Invalid targets/workloads cannot trigger transport or source preparation."""
        cases = (
            ("--limit", "all:*"),
            ("--revision", "main"),
            ("--steps", "0"),
            ("--steps", "13"),
            ("--runtime-seconds", "119"),
            ("--runtime-seconds", "21601"),
            ("--first-size", "10001"),
            ("--speakers", "65"),
            ("--server-cpus", "nan"),
            ("--generator-cpus", "inf"),
            ("--port-mbps", "0"),
            ("--label", "name;command"),
            ("--collect", "../unsafe"),
        )
        for extra in cases:
            with self.subTest(extra=extra), self.assertRaises(remote.CapacityControlError):
                _ = controller.options(self.cli(*extra))

    def test_collect_and_recover_cannot_prepare_or_build(self) -> None:
        """Recovery selectors never authorize a new image build or source mutation."""
        for recovery in ("--collect", "--recover"):
            for mutation in ("--prepare", "--build-images"):
                with (
                    self.subTest(recovery=recovery, mutation=mutation),
                    self.assertRaisesRegex(
                        remote.CapacityControlError, "recovery_cannot_start_work"
                    ),
                ):
                    _ = controller.options(self.cli(recovery, RUN, mutation))

    def test_audio_thirty_acceptance_shape_and_cpu_network_controls_survive(self) -> None:
        """The request keeps thirty simultaneous speakers and a thirty-second chat interval."""
        args = controller.options(
            self.cli(
                "--audio-only",
                "--meeting-size",
                "30",
                "--first-size",
                "30",
                "--speakers",
                "30",
                "--chat-interval-ms",
                "30000",
                "--server-cpus",
                "1",
                "--generator-cpus",
                "4.8",
                "--app-cpus",
                "5",
                "--port-mbps",
                "2000",
            )
        )
        request = controller.workload_request(args, RUN)
        for key, expected in {
            "meetingSize": 30,
            "firstSize": 30,
            "speakers": 30,
            "chatIntervalMs": 30000,
            "serverCpus": 1,
            "generatorCpus": 4.8,
            "appCpus": 5,
            "portMbps": 2000,
        }.items():
            self.assertEqual(request[key], expected)
        self.assertTrue(request["audioOnly"] is True)
        with self.assertRaisesRegex(remote.CapacityControlError, "chat_window_too_short"):
            _ = controller.options(self.cli("--quick", "--chat-interval-ms", "30000"))

    def test_inventory_resolution_refuses_public_hosts_and_transport_overrides(self) -> None:
        """Only the explicit private SSH host may be used, even with an old source pin."""
        with patch.object(build.Runner, "run") as execute:
            self.assertEqual(controller.inventory_target(self.args, self.root), self.target)
            execute.assert_not_called()
        for overrides in (
            {"scpub_enabled": True},
            {"ansible_ssh_extra_args": "untrusted"},
            {"ansible_connection": "local"},
            {"ansible_host": "host;command"},
        ):
            with (
                self.subTest(overrides=overrides),
                self.assertRaises((remote.CapacityControlError, BootstrapError)),
            ):
                _ = Path(self.args.inventory).write_text(json.dumps(self.inventory(**overrides)))
                _ = controller.inventory_target(self.args, self.root)

    def test_inventory_refuses_private_key_symlink_and_shared_permissions(self) -> None:
        """Transport consumes the same protected identity contract as bootstrap."""
        link = self.target.identity.with_name("key-link")
        link.symlink_to(self.target.identity)
        _ = Path(self.args.inventory).write_text(
            json.dumps(self.inventory(ansible_ssh_private_key_file=str(link)))
        )
        with self.assertRaises(BootstrapError):
            _ = controller.inventory_target(self.args, self.root)
        _ = Path(self.args.inventory).write_text(json.dumps(self.inventory()))
        self.target.identity.chmod(0o640)
        with self.assertRaises(BootstrapError):
            _ = controller.inventory_target(self.args, self.root)

    def test_static_inventory_rejects_executable_aliases_duplicates_and_templates(self) -> None:
        """No inventory program or templated value reaches a subprocess boundary."""
        path = Path(self.args.inventory)
        cases = (
            (b"#!/bin/sh\nexit 0\n", 0o700),
            (b"benchmark_hosts: &anchor {}\nother: *anchor\n", 0o600),
            (b"benchmark_hosts: {}\nbenchmark_hosts: {}\n", 0o600),
            (json.dumps(self.inventory(ansible_host="{{ fixture }}")).encode(), 0o600),
            (b"benchmark_hosts: [", 0o600),
        )
        for data, mode in cases:
            with self.subTest(data=data):
                _ = path.write_bytes(data)
                path.chmod(mode)
                with (
                    patch.object(build.Runner, "run") as execute,
                    self.assertRaises(BootstrapError),
                ):
                    _ = controller.inventory_target(self.args, self.root)
                execute.assert_not_called()

    def test_preparation_uses_validated_snapshot_when_operator_inventory_changes(self) -> None:
        """Every later Ansible operation uses the same bytes as endpoint validation."""
        self.args.prepare = True
        expected = self.inventory()
        original_action = self.action

        def change_inventory(
            transport: controller.Transport, request: dict[str, object], *, timeout: int = 60
        ) -> dict[str, object]:
            _ = Path(self.args.inventory).write_text("changed after validation")
            return original_action(transport, request, timeout=timeout)

        with (
            self.fake_host(),
            patch.object(controller.Transport, "call", new=change_inventory),
        ):
            result = controller.execute(self.args, self.root)
        self.assertTrue(result["passed"])
        snapshot = Path(self.commands[0][self.commands[0].index("-i") + 1])
        self.assertEqual(json.loads(snapshot.read_bytes()), expected)

    def test_lease_loss_cancels_a_quiet_preparation_before_its_deadline(self) -> None:
        """The same health check used by preparation observes remote EOF during execution."""
        with subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import time; print('{\"privateHost\":true}',flush=True); time.sleep(.4)",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        ) as child:
            if child.stdout is None or child.stderr is None:
                self.fail("fixture pipes missing")
            os.set_blocking(child.stdout.fileno(), False)
            os.set_blocking(child.stderr.fileno(), False)
            lease = controller.PreparationLease(child, io.BytesIO())
            lease.ready()
            with self.assertRaisesRegex(build.BuildError, "command_lease_lost"):
                _ = build.Runner(healthy=lease.healthy).run(
                    [sys.executable, "-c", "import time; time.sleep(30)"], cwd=self.root, timeout=5
                )

    def test_transport_keeps_request_and_helper_out_of_ssh_arguments(self) -> None:
        """User-selected data travels in bounded JSON stdin, not remote shell text."""
        transport = controller.Transport(self.target, "# " + SECRET, self.root)
        request: dict[str, object] = {"action": "collect", "marker": SECRET}
        self.assertNotIn(SECRET, " ".join(transport.argv()))
        self.assertIn(SECRET.encode(), transport.payload(request))
        self.assertIn("StrictHostKeyChecking=yes", transport.argv())
        with (
            patch.object(controller, "MAX_REQUEST", 10),
            self.assertRaisesRegex(remote.CapacityControlError, "remote_request_too_large"),
        ):
            _ = transport.payload(request)

    def test_prepare_holds_lease_uses_revision_override_and_keeps_maintenance_off(self) -> None:
        """Ansible source/helper advancement is serialized and uses the same strict SSH trust."""
        self.args.prepare = True
        self.args.build_images = True
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertTrue(result["passed"] is True)
        self.assertEqual(self.actions[:4], ["preflight", "reserve", "release", "build"])
        self.assertEqual(len(self.commands), 1)
        command = self.commands[0]
        self.assertIn("source,benchmark", command)
        snapshot = Path(command[command.index("-i") + 1])
        self.assertNotEqual(snapshot, Path(self.args.inventory))
        self.assertEqual(json.loads(snapshot.read_bytes()), self.inventory())
        overrides: dict[str, object] = {}
        for index, value in enumerate(command):
            if value == "--extra-vars":
                overrides.update(remote.obj(cast("object", json.loads(command[index + 1]))))
        self.assertEqual(overrides["scbench_revision"], REVISION)
        self.assertTrue(overrides["scbench_upgrade_packages"] is False)
        self.assertTrue(overrides["scbench_reboot"] is False)
        self.assertEqual(
            overrides["ansible_ssh_common_args"], bootstrap.inventory_ssh_common_args(self.target)
        )
        self.assertFalse(self.lease)
        evidence = Path(remote.text(result["evidence"]))
        self.assertEqual((evidence / "remote/result.json").read_bytes(), b'{"retained":true}')
        self.assertRegex(remote.text(result["archiveSha256"]), r"^[a-f0-9]{64}$")

    def test_successful_unit_does_not_mask_failed_workload_verdict(self) -> None:
        """The report remains failed while all useful trial artifacts are collected."""
        self.verdict = False
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertTrue(result["passed"] is False)
        self.assertEqual(result["remoteOutcome"], "collected")
        self.assertIn("archive", self.actions)
        self.assertEqual(remote.obj(result["unit"])["ExecMainStatus"], "0")

    def test_collect_uses_saved_request_without_starting_work(self) -> None:
        """Current CLI defaults must not relabel the profile of an existing run."""
        self.args.collect = RUN
        self.collected_request = dict(
            controller.workload_request(self.args, RUN),
            firstSize=150,
            meetingSize=30,
            speakers=30,
            label="retained-audio-profile",
        )
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertEqual(self.actions, ["status", "collect", "archive"])
        self.assertEqual(result["run"], RUN)
        self.assertEqual(result["request"], self.collected_request)

    def test_recover_waits_then_collects_without_restarting_run(self) -> None:
        """Recovery remains an explicit action on one retained run ID."""
        self.args.recover = RUN
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertEqual(self.actions, ["status", "recover", "collect", "archive"])
        self.assertTrue(result["passed"] is True)

    def test_start_transport_failure_retains_request_for_later_collection(self) -> None:
        """A lost start response is not retried; the generated run ID remains recoverable."""
        self.failed_action = "start"
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertEqual(self.actions, ["preflight", "start"])
        self.assertTrue(result["passed"] is False)
        self.assertEqual(result["remoteOutcome"], "start_requested")
        self.assertEqual(result["failureClass"], "fixture_transport_failure")
        self.assertRegex(remote.text(result["run"]), r"^[a-f0-9]{32}$")
        saved = remote.read_json(Path(remote.text(result["evidence"])) / "report.json")
        self.assertEqual(saved["request"], result["request"])
        self.assertEqual(saved["run"], result["run"])

    def test_failed_image_build_never_starts_capacity(self) -> None:
        """A failed canonical build is retained, not retried or bypassed."""
        self.args.build_images = True
        self.build_failed = True
        with self.fake_host():
            result = controller.execute(self.args, self.root)
        self.assertEqual(self.actions, ["preflight", "build", "status"])
        self.assertEqual(result["failureClass"], "image_build_failed")
        self.assertTrue(result["passed"] is False)

    def test_wait_finishes_retained_exited_unit_without_another_poll(self) -> None:
        """RemainAfterExit preserves a successful result while ActiveState remains active."""
        transport = controller.Transport(self.target, "# helper", self.root)
        retained: dict[str, object] = {
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "exited",
            "MainPID": "0",
            "Result": "success",
            "ExecMainStatus": "0",
        }
        with (
            patch.object(controller.Transport, "call", side_effect=[retained]) as called,
            patch("vps_capacity_controller.time.sleep") as sleep,
        ):
            self.assertEqual(
                controller.wait_for_unit(transport, controller.workload_request(self.args, RUN)),
                retained,
            )
            self.assertEqual(called.call_count, 1)
            sleep.assert_not_called()

    def test_wait_does_not_finish_running_or_deactivating_service(self) -> None:
        """An exited-looking state still requires no main process and no stopping job."""
        transport = controller.Transport(self.target, "# helper", self.root)
        finished: dict[str, object] = {
            "LoadState": "loaded",
            "ActiveState": "inactive",
            "SubState": "dead",
            "MainPID": "0",
            "Result": "success",
            "ExecMainStatus": "0",
        }
        cases = (
            {"ActiveState": "active", "SubState": "running", "MainPID": "17"},
            {"ActiveState": "active", "SubState": "exited", "MainPID": "17"},
            {"ActiveState": "deactivating", "SubState": "exited", "MainPID": "0"},
        )
        for active in cases:
            with (
                self.subTest(active=active),
                patch.object(
                    controller.Transport, "call", side_effect=[active, finished]
                ) as called,
                patch("vps_capacity_controller.time.sleep") as sleep,
            ):
                self.assertEqual(
                    controller.wait_for_unit(
                        transport, controller.workload_request(self.args, RUN)
                    ),
                    finished,
                )
                self.assertEqual(called.call_count, 2)
                sleep.assert_called_once_with(5)

    def test_wait_deadline_refuses_to_restart_an_active_unit(self) -> None:
        """A unit beyond its observation deadline stays an explicit recovery operation."""
        transport = controller.Transport(self.target, "# helper", self.root)
        active: dict[str, object] = {
            "ActiveState": "active",
            "SubState": "running",
            "MainPID": "17",
        }
        with (
            patch.object(controller.Transport, "call", return_value=active) as called,
            patch("vps_capacity_controller.time.monotonic", side_effect=[0, 2000]),
            patch("vps_capacity_controller.time.sleep") as sleep,
            self.assertRaisesRegex(
                remote.CapacityControlError, "unit_wait_expired_collect_before_retry"
            ),
        ):
            _ = controller.wait_for_unit(transport, controller.workload_request(self.args, RUN))
        self.assertEqual(called.call_count, 1)
        sleep.assert_not_called()

    def test_main_returns_failed_verdict_as_nonzero(self) -> None:
        """Shell callers cannot confuse successful evidence collection with workload acceptance."""
        with (
            patch.object(controller, "options", return_value=self.args),
            patch.object(controller, "execute", return_value={"passed": False}),
            redirect_stdout(io.StringIO()),
            redirect_stderr(io.StringIO()),
        ):
            original = os.umask(0o077)
            try:
                self.assertEqual(controller.main([]), 1)
            finally:
                _ = os.umask(original)

    def test_safe_archive_ignores_untrusted_modes(self) -> None:
        """Collected data creates only private ordinary files in a new directory."""
        archive, destination = self.root / "safe.tar.gz", self.root / "safe"
        archive_at(archive, [Entry("nested/result.json")])
        controller.extract_archive(archive, destination)
        self.assertEqual((destination / "nested/result.json").read_bytes(), b"evidence")
        self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((destination / "nested/result.json").stat().st_mode), 0o600)

    def test_archive_rejects_paths_links_devices_and_directories(self) -> None:
        """Malicious member names or kinds cannot escape or alter the evidence tree."""
        entries = [
            Entry("../escape"),
            Entry("/absolute"),
            Entry("a\\b"),
            Entry("link", kind=tarfile.SYMTYPE, link="../escape"),
            Entry("hard", kind=tarfile.LNKTYPE, link="outside"),
            Entry("device", kind=tarfile.CHRTYPE),
            Entry("directory", kind=tarfile.DIRTYPE),
        ]
        for index, entry in enumerate(entries):
            archive = self.root / f"unsafe-{index}.tar.gz"
            archive_at(archive, [entry])
            with (
                self.subTest(entry=entry),
                self.assertRaisesRegex(remote.CapacityControlError, "unsafe_archive_entry"),
            ):
                controller.extract_archive(archive, self.root / f"unsafe-{index}")
        self.assertFalse((self.root / "escape").exists())

    def test_archive_rejects_duplicates_counts_and_expanded_size(self) -> None:
        """Small fixtures exercise each bound without allocating large test artifacts."""
        scenarios: list[tuple[str, int, list[Entry], str]] = [
            ("MAX_FILES", 1, [Entry("one"), Entry("two")], "duplicate_or_excess_archive_entries"),
            (
                "MAX_FILES",
                10,
                [Entry("same"), Entry("same")],
                "duplicate_or_excess_archive_entries",
            ),
            ("MAX_FILE", 3, [Entry("large", b"1234")], "artifact_too_large"),
            (
                "MAX_EXPANDED",
                5,
                [Entry("one", b"123"), Entry("two", b"456")],
                "artifacts_too_large",
            ),
            ("MAX_ARCHIVE", 1, [Entry("one")], "archive_too_large"),
        ]
        for index, (limit, value, entries, error) in enumerate(scenarios):
            archive = self.root / f"bound-{index}.tar.gz"
            archive_at(archive, entries)
            with (
                self.subTest(limit=limit, error=error),
                patch.object(remote, limit, value),
                self.assertRaisesRegex(remote.CapacityControlError, error),
            ):
                controller.extract_archive(archive, self.root / f"bound-{index}")

    def test_archive_destination_must_be_new(self) -> None:
        """Collection never overwrites another attempt's retained local evidence."""
        archive = self.root / "existing.tar.gz"
        archive_at(archive, [Entry("result")])
        destination = self.root / "existing"
        destination.mkdir()
        original = destination / "result"
        _ = original.write_text("preserve")
        with self.assertRaises(FileExistsError):
            controller.extract_archive(archive, destination)
        self.assertEqual(original.read_text(), "preserve")

    def test_archive_bounds_include_padding_and_reject_concatenated_gzip(self) -> None:
        """Compressed metadata and trailing streams cannot bypass expanded byte accounting."""
        archive = self.root / "padding.tar.gz"
        _ = archive.write_bytes(gzip.compress(bytes(2048)))
        with (
            patch.object(remote, "MAX_EXPANDED", 1024),
            self.assertRaisesRegex(remote.CapacityControlError, "artifacts_too_large"),
        ):
            controller.extract_archive(archive, self.root / "padding")
        _ = archive.write_bytes(gzip.compress(bytes(1024)) + gzip.compress(bytes(1024)))
        with self.assertRaisesRegex(remote.CapacityControlError, "trailing_gzip_data"):
            controller.extract_archive(archive, self.root / "concatenated")


if __name__ == "__main__":
    _ = unittest.main()
