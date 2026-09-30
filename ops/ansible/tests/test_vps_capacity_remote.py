"""Offline private-capacity worker invariants; no SSH, Docker or systemd actions."""

from __future__ import annotations

import io
import json
import os
import signal
import subprocess
import sys
import tarfile
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path
from typing import override
from unittest.mock import ANY, MagicMock, call, patch

from test_support import ROOT

# isort: split

import bounded_process
import vps_capacity_remote as remote

REVISION = "a" * 40
RUN = "b" * 32
OWNERSHIP_RUN = "c" * 16
SERVER = "sha256:" + "d" * 64
GENERATOR = "sha256:" + "e" * 64
IDENTITY = "f" * 64
FOREIGN = "1" * 64
SECRET = "PRIVATE_INSPECTION_SENTINEL"  # noqa: S105 -- Deliberately public sentinel verifies redaction.


def request(**changes: object) -> dict[str, object]:
    """Return the bounded thirty-speaker request used by the new VPS experiment."""
    return {
        "revision": REVISION,
        "run": RUN,
        "workload": "meetings",
        "label": "audio30",
        "firstSize": 30,
        "steps": 1,
        "meetingSize": 30,
        "speakers": 30,
        "runtimeSeconds": 1200,
        "quick": False,
        "audioOnly": True,
        "chatIntervalMs": 30000,
        "serverCpus": 1,
        "generatorCpus": 4.8,
        "monthlyPrice": None,
        "portMbps": 2000,
        "appCpus": None,
        **changes,
    }


def report(*, passed: bool = True, valid: bool = True) -> dict[str, object]:
    """Return a calibration using the actual capacity.py image and profile schema."""
    return {
        "schemaVersion": 1,
        "measurement": {
            "meetingSize": 30,
            "browser": {"audioOnly": True, "speakers": 30, "chatIntervalMs": 30000},
        },
        "images": {
            "server": {"id": SERVER, "reference": SERVER, "revision": REVISION},
            "generator": {"id": GENERATOR, "reference": GENERATOR, "revision": REVISION},
        },
        "steps": [
            {
                "workload": "meetings",
                "size": 30,
                "audioOnly": True,
                "speakers": 30,
                "meetingSize": 30,
                "chatIntervalMs": 30000,
                "valid": valid,
                "passed": passed,
            }
        ],
        "ceilings": {"meetings": {"size": 30, "lowerBound": True}},
        "projection": {"lowerBound": True},
    }


def image_manifest() -> dict[str, object]:
    """Return immutable identities for both committed runtime images."""
    return {
        "schemaVersion": 1,
        "passed": True,
        "revision": REVISION,
        "serverImage": SERVER,
        "generatorImage": GENERATOR,
    }


def unit_report(**changes: object) -> dict[str, object]:
    """Return the explicit systemd fields used to distinguish terminal and live units."""
    return {
        "LoadState": "loaded",
        "ActiveState": "inactive",
        "SubState": "dead",
        "MainPID": "0",
        "Result": "success",
        "ExecMainCode": "exited",
        "ExecMainStatus": "0",
        **changes,
    }


class RemoteTests(unittest.TestCase):
    """Use real private temporary files and replace all external command boundaries."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Declare fixture state before unittest creates the isolated temporary tree."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.state: Path = ROOT
        self.directory: Path = ROOT
        self.source: Path = ROOT
        self.command: MagicMock = MagicMock()
        self.ids: MagicMock = MagicMock()
        self.unit: MagicMock = MagicMock()

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        self.directory = self.root / "results" / f"capacity-controller.{RUN}"
        self.directory.mkdir(parents=True, mode=0o700)
        self.source = self.root / "sources" / REVISION
        (self.source / "build").mkdir(parents=True)
        self.addCleanup(patch.stopall)
        _ = patch.object(remote, "ROOT", self.root).start()
        _ = patch.object(remote, "STATE", self.state).start()
        # Filesystem shape is real; the controller runs as root on Debian while
        # these tests deliberately run as the developer's unprivileged account.
        _ = patch.object(remote, "protected").start()
        self.command = patch.object(remote, "command", return_value="").start()
        self.ids = patch.object(remote, "container_ids", return_value=[]).start()
        self.unit = patch.object(
            remote,
            "unit_state",
            return_value=unit_report(),
        ).start()

    def write(self, path: Path, value: dict[str, object]) -> None:
        """Write through the same atomic JSON helper used by the remote worker."""
        path.parent.mkdir(parents=True, exist_ok=True)
        remote.save(path, value)

    def ownership(self, *, identity: str | None = IDENTITY) -> None:
        """Record one exact expected server identity, including an interrupted create."""
        self.write(
            self.directory / "workload/ownership.json",
            {
                "schemaVersion": 1,
                "runId": OWNERSHIP_RUN,
                "containers": [{"name": "capacity-server", "image": SERVER, "id": identity}],
            },
        )

    def inspection(self, **changes: object) -> dict[str, object]:
        """Include private Docker fields so the evidence-redaction assertion is meaningful."""
        return {
            "Id": IDENTITY,
            "Name": "/capacity-server",
            "Image": SERVER,
            "Config": {
                "Image": SERVER,
                "Env": [f"METRICS_TOKEN={SECRET}"],
                "Labels": {"simplestchat.capacity.run": OWNERSHIP_RUN},
            },
            "Created": "2026-09-30T00:00:00Z",
            "State": {"Status": "running", "Running": True, "ExitCode": 0},
            **changes,
        }

    def prepare_collection(self) -> None:
        """Create the minimal retained evidence left before a worker starts."""
        self.write(self.directory / "request.json", request())
        self.write(self.directory / "containers-before.json", {"ids": []})

    def test_request_rejects_coercion_and_unbounded_work(self) -> None:
        """Booleans, nonfinite quotas, malformed IDs and excessive work fail before launch."""
        self.assertEqual(remote.validate_request(request()), request())
        mutations: list[dict[str, object]] = [
            {"revision": "main"},
            {"run": "../outside"},
            {"label": "two words"},
            {"firstSize": True},
            {"meetingSize": 65},
            {"speakers": 0},
            {"steps": 13},
            {"runtimeSeconds": 21601},
            {"serverCpus": float("nan")},
            {"generatorCpus": float("inf")},
            {"audioOnly": 1},
            {"quick": 0},
            {"chatIntervalMs": 999},
            {"chatIntervalMs": 58001},
            {"quick": True, "chatIntervalMs": 30000},
        ]
        for changes in mutations:
            with self.subTest(changes=changes), self.assertRaises(remote.CapacityControlError):
                _ = remote.validate_request(request(**changes))
        self.command.assert_not_called()

    def test_capacity_command_preserves_audio_chat_and_room_shape(self) -> None:
        """One explicit argv carries the measured workload and admission limits unchanged."""
        argv = remote.capacity_argv(request(), image_manifest())
        self.assertEqual(argv[:2], ["/usr/bin/python3", "-B"])
        for flag, expected in {
            "--server-image": SERVER,
            "--generator-image": GENERATOR,
            "--first-size": "30",
            "--steps": "1",
            "--meeting-size": "30",
            "--speakers": "30",
            "--chat-interval-ms": "30000",
            "--server-cpus": "1",
            "--generator-cpus": "4.8",
        }.items():
            self.assertEqual(argv[argv.index(flag) + 1], expected)
        self.assertIn("--audio-only", argv)
        self.assertNotIn("--quick", argv)
        self.assertIn("MAX_PARTICIPANTS_PER_ROOM=30", argv)
        self.assertIn("MAX_BROADCASTERS_PER_ROOM=30", argv)
        self.assertEqual(argv[argv.index("--output") + 1], str(self.directory / "workload"))

    def test_validity_and_quality_failures_remain_failures(self) -> None:
        """A zero-exit calibration cannot conceal failed quality or generator-invalid trials."""
        self.assertTrue(remote.assess(report(), request())["passed"] is True)
        for passed, valid in ((False, True), (True, False), (False, False)):
            with self.subTest(passed=passed, valid=valid):
                result = remote.assess(report(passed=passed, valid=valid), request())
                self.assertTrue(result["passed"] is False)
                self.assertEqual(len(remote.array(result["failedSteps"])), 1)
        for steps in ([], [{"workload": "webinar", "valid": True, "passed": True}]):
            invalid = report()
            invalid["steps"] = steps
            with self.assertRaises(remote.CapacityControlError):
                _ = remote.assess(invalid, request())

    def test_passing_report_must_describe_the_requested_workload_and_revision(self) -> None:
        """A historical video or one-speaker result cannot certify thirty continuous speakers."""
        for key, value in (("audioOnly", False), ("speakers", 1), ("chatIntervalMs", 0)):
            altered = report()
            browser = remote.obj(remote.obj(altered["measurement"])["browser"])
            browser[key] = value
            with self.subTest(key=key), self.assertRaises(remote.CapacityControlError):
                _ = remote.assess(altered, request())
        altered = report()
        remote.obj(altered["measurement"])["meetingSize"] = 5
        with self.assertRaises(remote.CapacityControlError):
            _ = remote.assess(altered, request())
        for role in ("server", "generator"):
            altered = report()
            remote.obj(remote.obj(altered["images"])[role])["revision"] = "0" * 40
            with self.subTest(role=role), self.assertRaises(remote.CapacityControlError):
                _ = remote.assess(altered, request())

    def test_json_evidence_is_private_and_bounded(self) -> None:
        """The durable JSON path rejects aliases and oversized reports, preserving private mode."""
        path = self.directory / "sample.json"
        self.write(path, {"passed": True})
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(remote.read_json(path), {"passed": True})
        alias = self.directory / "alias.json"
        alias.symlink_to(path)
        with self.assertRaisesRegex(remote.CapacityControlError, "invalid_json_file"):
            _ = remote.read_json(alias)
        with (
            patch.object(remote, "MAX_JSON", 1),
            self.assertRaisesRegex(remote.CapacityControlError, "json_too_large"),
        ):
            _ = remote.read_json(path)

    def test_unfinished_journal_blocks_new_work(self) -> None:
        """An abandoned worker cannot be treated as idle merely because containers disappeared."""
        self.write(self.state / "current.json", {"schemaVersion": 1, "finalized": False})
        with (
            patch.object(os, "geteuid", return_value=0),
            self.assertRaisesRegex(remote.CapacityControlError, "cleanup_unfinished"),
        ):
            _ = remote.preflight()

    def test_residuals_confirm_exact_ids_but_never_emit_environment(self) -> None:
        """A label, expected name, immutable image and optional journal ID establish ownership."""
        self.ownership()
        self.ids.side_effect = [[FOREIGN, IDENTITY], [IDENTITY]]
        self.command.return_value = json.dumps([self.inspection()])
        result = remote.residuals(self.directory, [FOREIGN])
        self.assertTrue(result["clean"] is False)
        items = remote.array(result["residuals"])
        self.assertEqual(len(items), 1)
        self.assertTrue(remote.obj(items[0])["confirmedOwned"] is True)
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn("Env", json.dumps(result))
        self.command.assert_called_once_with([*remote.DOCKER, "inspect", IDENTITY])

    def test_label_alone_does_not_authorize_residual_cleanup(self) -> None:
        """An unexpected name, different image or replaced container remains unconfirmed."""
        self.ownership()
        for changes in (
            {"Name": "/unexpected"},
            {"Image": GENERATOR, "Config": {"Image": GENERATOR}},
        ):
            with self.subTest(changes=changes):
                self.ids.side_effect = [[IDENTITY], [IDENTITY]]
                self.command.return_value = json.dumps([self.inspection(**changes)])
                result = remote.residuals(self.directory, [])
                self.assertTrue(
                    remote.obj(remote.array(result["residuals"])[0])["confirmedOwned"] is False
                )
        self.ownership(identity=FOREIGN)
        self.ids.side_effect = [[IDENTITY], [IDENTITY]]
        self.command.return_value = json.dumps([self.inspection()])
        result = remote.residuals(self.directory, [])
        self.assertTrue(remote.obj(remote.array(result["residuals"])[0])["confirmedOwned"] is False)

    def test_interrupted_create_still_identifies_the_exact_owned_container(self) -> None:
        """An interrupted create is found by its unique label and expected image/name."""
        self.ownership(identity=None)
        self.ids.side_effect = [[IDENTITY], [IDENTITY]]
        self.command.return_value = json.dumps([self.inspection()])
        result = remote.residuals(self.directory, [])
        self.assertTrue(remote.obj(remote.array(result["residuals"])[0])["confirmedOwned"] is True)
        self.command.assert_called_once_with([*remote.DOCKER, "inspect", IDENTITY])

    def test_requested_image_identity_survives_a_distinct_resolved_digest(self) -> None:
        """Docker's resolved platform image need not equal the immutable image index requested."""
        self.ownership()
        self.ids.side_effect = [[IDENTITY], [IDENTITY]]
        self.command.return_value = json.dumps([self.inspection(Image=GENERATOR)])
        result = remote.residuals(self.directory, [])
        item = remote.obj(remote.array(result["residuals"])[0])
        self.assertTrue(item["confirmedOwned"] is True)
        self.assertEqual(remote.obj(item["inspection"])["RequestedImage"], SERVER)
        self.assertEqual(remote.obj(item["inspection"])["Image"], GENERATOR)

    def test_foreign_new_container_is_retained_without_ownership(self) -> None:
        """A new container without the exact recorded run label cannot be adopted by cleanup."""
        self.ids.return_value = [IDENTITY]
        self.command.return_value = json.dumps([self.inspection()])
        result = remote.residuals(self.directory, [])
        self.assertTrue(result["clean"] is False)
        self.assertTrue(remote.obj(remote.array(result["residuals"])[0])["confirmedOwned"] is False)
        self.command.assert_called_once_with([*remote.DOCKER, "inspect", IDENTITY])

    def test_missing_outcome_and_unit_timeout_cannot_pass_collection(self) -> None:
        """A killed worker or systemd timeout stays failed even after normal container cleanup."""
        self.prepare_collection()
        with patch.object(remote, "workload_lock", return_value=nullcontext()):
            missing = remote.collect(request())
        self.assertTrue(missing["passed"] is False)
        self.assertEqual(remote.obj(missing["outcome"])["failureClass"], "worker_outcome_missing")
        self.write(self.directory / "outcome.json", {"passed": True, "finalized": True})
        self.unit.return_value = unit_report(
            ActiveState="failed", SubState="failed", Result="timeout"
        )
        with patch.object(remote, "workload_lock", return_value=nullcontext()):
            timed_out = remote.collect(request())
        self.assertTrue(timed_out["passed"] is False)

    def test_collection_does_not_finalize_a_missing_worker_journal(self) -> None:
        """Read-only collection preserves recovery's explicit unfinished-state decision."""
        self.prepare_collection()
        journal: dict[str, object] = {"schemaVersion": 1, "run": RUN, "finalized": False}
        self.write(self.state / "current.json", journal)
        with patch.object(remote, "workload_lock", return_value=nullcontext()):
            result = remote.collect(request())
        self.assertTrue(result["passed"] is False)
        self.assertEqual(remote.read_json(self.state / "current.json"), journal)

    def test_active_unit_cannot_be_collected(self) -> None:
        """Evidence snapshots cannot certify cleanup while the workload is still active."""
        self.prepare_collection()
        self.unit.return_value = unit_report(
            ActiveState="deactivating", SubState="stop-sigterm", MainPID="123"
        )
        with self.assertRaisesRegex(remote.CapacityControlError, "capacity_still_running"):
            _ = remote.collect(request())
        self.command.assert_not_called()

    def test_retained_success_is_saved_before_release_and_survives_systemd_gc(self) -> None:
        """Fresh SSH collection retains exact terminal evidence before releasing transient state."""
        self.prepare_collection()
        self.write(self.directory / "outcome.json", {"passed": True, "finalized": True})
        unit = f"simplestchat-capacity-{RUN}.service"
        self.unit.return_value = unit_report(ActiveState="active", SubState="exited")

        def command(argv: list[str]) -> str:
            if argv == ["/usr/bin/systemctl", "stop", unit]:
                saved = remote.read_json(self.directory / "unit-result.json")
                self.assertEqual(saved["Unit"], unit)
                self.assertEqual(saved["Result"], "success")
                self.assertEqual(saved["MainPID"], "0")
                self.assertTrue(
                    remote.read_json(self.directory / "collection.json")["passed"] is True
                )
            return ""

        self.command.side_effect = command
        with patch.object(remote, "workload_lock", return_value=nullcontext()):
            first = remote.collect(request())
        self.assertTrue(first["passed"] is True)
        self.command.assert_any_call(["/usr/bin/systemctl", "stop", unit])
        self.unit.return_value = unit_report(LoadState="not-found")
        self.command.reset_mock()
        with patch.object(remote, "workload_lock", return_value=nullcontext()):
            repeated = remote.collect(request())
        self.assertTrue(repeated["passed"] is True)
        self.assertTrue(remote.obj(repeated["unit"])["Retained"] is True)
        self.assertEqual(repeated["request"], request())
        self.command.assert_called_once_with(
            [
                "/usr/bin/journalctl",
                "--unit",
                unit,
                "--no-pager",
                "--lines=2000",
                "--output=short-iso",
            ]
        )

    def test_gc_fallback_requires_matching_request_and_exact_terminal_unit(self) -> None:
        """A missing unit cannot inherit another run's successful or still-running evidence."""
        self.prepare_collection()
        self.unit.return_value = unit_report(LoadState="not-found")
        unit = f"simplestchat-capacity-{RUN}.service"
        path = self.directory / "unit-result.json"
        with self.assertRaisesRegex(remote.CapacityControlError, "invalid_json_file"):
            _ = remote.capacity_unit_state(request())
        for state in (
            unit_report(Unit="simplestchat-capacity-another.service"),
            unit_report(Unit=unit, ActiveState="active", SubState="running", MainPID="123"),
            unit_report(Unit=unit, ActiveState="active", SubState="exited", MainPID="123"),
        ):
            self.write(path, state)
            with (
                self.subTest(state=state),
                self.assertRaisesRegex(remote.CapacityControlError, "invalid_retained_unit_state"),
            ):
                _ = remote.capacity_unit_state(request())
        self.write(path, unit_report(Unit=unit))
        for changes in ({"revision": "0" * 40}, {"run": "0" * 32}):
            self.write(self.directory / "request.json", request(**changes))
            with (
                self.subTest(changes=changes),
                self.assertRaisesRegex(remote.CapacityControlError, "collection_revision_mismatch"),
            ):
                _ = remote.capacity_unit_state(request())
        self.command.assert_not_called()

    def test_uncollected_terminal_unit_still_blocks_a_new_workload(self) -> None:
        """Retaining a successful process for collection does not permit overlapping admission."""
        self.command.return_value = (
            f"simplestchat-capacity-{RUN}.service loaded active exited Capacity fixture"
        )
        with (
            patch.object(os, "geteuid", return_value=0),
            self.assertRaisesRegex(remote.CapacityControlError, "capacity_unit_busy"),
        ):
            _ = remote.preflight()

    def test_archive_stream_contains_only_regular_evidence(self) -> None:
        """A finished report round-trips through the real bounded tar writer without aliases."""
        self.prepare_collection()
        self.write(self.directory / "collection.json", {"passed": False})
        buffer = io.BytesIO()
        with patch.object(sys, "stdout", MagicMock(buffer=buffer)):
            remote.stream_archive(request())
        _ = buffer.seek(0)
        with tarfile.open(fileobj=buffer, mode="r:gz") as archive:
            members = archive.getmembers()
            self.assertTrue(members)
            self.assertTrue(all(item.isfile() for item in members))
            self.assertIn("collection.json", [item.name for item in members])

    def test_archive_rejects_symlinks_and_limits_before_transport(self) -> None:
        """Malformed local artifacts and oversized evidence fail before any archive is sent."""
        self.prepare_collection()
        self.write(self.directory / "collection.json", {"passed": False})
        alias = self.directory / "linked.json"
        alias.symlink_to(self.directory / "request.json")
        with self.assertRaisesRegex(remote.CapacityControlError, "unsafe_artifact_kind"):
            remote.stream_archive(request())
        alias.unlink()
        for limit, code in (
            ("MAX_FILES", "too_many_artifacts"),
            ("MAX_FILE", "artifact_too_large"),
            ("MAX_EXPANDED", "artifacts_too_large"),
            ("MAX_ARCHIVE", "archive_too_large"),
        ):
            with (
                self.subTest(limit=limit),
                patch.object(remote, limit, 1),
                self.assertRaisesRegex(remote.CapacityControlError, code),
            ):
                remote.stream_archive(request())

    def test_start_binds_helper_and_launches_bounded_inherited_lock(self) -> None:
        """Systemd runs one exact helper with an inherited nonblocking lock and finite lifetime."""
        self.directory.rmdir()
        helper = (ROOT / "build/vps_capacity_remote.py").read_text()
        _ = (self.source / "build/vps_capacity_remote.py").write_text(helper)
        process_helper = (ROOT / "build/bounded_process.py").read_text()
        _ = (self.source / "build/bounded_process.py").write_text(process_helper)
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(remote, "preflight", return_value={}),
            patch.object(remote, "verify_source", return_value=self.source),
            patch.object(remote, "images_for", return_value=image_manifest()),
        ):
            result = remote.start(request(), helper, process_helper)
        self.assertEqual(result["run"], RUN)
        recorded = remote.array(remote.read_json(self.directory / "unit-command.json")["argv"])
        for argument in (
            "--property=RuntimeMaxSec=1200",
            "--property=TimeoutStopSec=600",
            "--property=KillMode=mixed",
            "--property=RemainAfterExit=yes",
            "--property=Restart=no",
            "--no-fork",
            "--nonblock",
        ):
            self.assertIn(argument, recorded)
        self.assertIn(str(self.state / "workload.lock"), recorded)
        self.assertEqual((self.directory / "worker.py").read_text(), helper)
        self.assertEqual(remote.read_json(self.directory / "request.json"), request())

    def test_worker_requires_quality_success_child_success_and_verified_cleanup(self) -> None:
        """The actual journal lifecycle cannot turn any failed boundary into a passing run."""
        variants = (
            (0, True, True, True),
            (1, True, True, False),
            (0, False, True, False),
            (0, True, False, False),
        )
        for exit_status, quality, clean, expected in variants:
            with self.subTest(exit_status=exit_status, quality=quality, clean=clean):
                log = self.directory / "capacity.log"
                log.unlink(missing_ok=True)
                docker_config = self.directory / "docker-config"
                if docker_config.exists():
                    docker_config.rmdir()
                self.write(self.state / "current.json", {"schemaVersion": 1, "finalized": True})
                self.write(self.directory / "workload/calibration.json", report(passed=quality))
                wait = MagicMock(return_value=exit_status)
                child = MagicMock(wait=wait)
                cleanup: dict[str, object] = {"clean": clean, "residuals": []}
                with (
                    patch.object(remote, "preflight", return_value={}),
                    patch.object(remote, "verify_source", return_value=self.source),
                    patch.object(remote, "images_for", return_value=image_manifest()),
                    patch.object(remote, "residuals", return_value=cleanup),
                    patch.object(subprocess, "Popen", return_value=child) as launch,
                    patch.object(bounded_process, "pump", return_value=exit_status) as pump,
                    patch.object(signal, "signal"),
                ):
                    status = remote.run_worker(request())
                self.assertEqual(status, 0 if expected else 1)
                outcome = remote.read_json(self.directory / "outcome.json")
                self.assertEqual(outcome["passed"], expected)
                self.assertEqual(outcome["finalized"], clean)
                self.assertEqual(remote.read_json(self.state / "current.json")["finalized"], clean)
                launch.assert_called_once_with(
                    remote.capacity_argv(request(), image_manifest()),
                    stdin=subprocess.DEVNULL,
                    stdout=ANY,
                    stderr=subprocess.PIPE,
                    env=dict(
                        remote.ENVIRONMENT,
                        DOCKER_HOST="unix:///var/run/docker.sock",
                        DOCKER_CONFIG=str(docker_config),
                    ),
                    start_new_session=True,
                )
                self.assertEqual(docker_config.stat().st_mode & 0o777, 0o700)
                self.assertEqual(list(docker_config.iterdir()), [])
                pump.assert_called_once()

    def test_image_checks_reject_mutable_refs_wrong_revision_and_root_runtime(self) -> None:
        """Runtime manifests must match both immutable images and their committed nonroot build."""
        manifest_path = self.root / "artifacts" / REVISION / "images.json"
        self.write(manifest_path, image_manifest())
        for changes in (
            {"Id": GENERATOR},
            {"Config": {"User": "root", "Labels": {"org.opencontainers.image.revision": REVISION}}},
            {
                "Config": {
                    "User": "10001:10001",
                    "Labels": {"org.opencontainers.image.revision": "wrong"},
                }
            },
        ):
            inspected = {
                "Id": SERVER,
                "Config": {
                    "User": "10001:10001",
                    "Labels": {"org.opencontainers.image.revision": REVISION},
                },
                **changes,
            }
            with self.subTest(changes=changes), self.assertRaises(remote.CapacityControlError):
                self.command.return_value = json.dumps([inspected])
                _ = remote.images_for(request())
        mutable = image_manifest()
        mutable["serverImage"] = "simplestchat:latest"
        self.write(manifest_path, mutable)
        self.command.reset_mock()
        with self.assertRaisesRegex(remote.CapacityControlError, "mutable_image_refused"):
            _ = remote.images_for(request())
        self.command.assert_not_called()

    def test_dirty_or_wrong_remote_checkout_is_not_executed(self) -> None:
        """A configured revision string never substitutes for checking the actual source tree."""
        for outputs, code in (
            (["0" * 40], "source_revision_mismatch"),
            ([REVISION, " M build/capacity.py"], "remote_checkout_dirty"),
            ([REVISION, "?? untracked.py"], "remote_checkout_dirty"),
        ):
            with (
                self.subTest(outputs=outputs),
                self.assertRaisesRegex(remote.CapacityControlError, code),
            ):
                self.command.side_effect = outputs
                _ = remote.verify_source(request())
        self.command.side_effect = [REVISION, ""]
        self.assertEqual(remote.verify_source(request()), self.source)

    def test_canonical_lock_cannot_be_acquired_twice(self) -> None:
        """Real flock admission shares an inode and fails immediately for a competing owner."""
        with remote.workload_lock():
            identity = (self.state / "workload.lock").stat().st_ino
            with self.assertRaises(BlockingIOError), remote.workload_lock():
                self.fail("A competing workload acquired the same lock")
        with remote.workload_lock():
            self.assertEqual((self.state / "workload.lock").stat().st_ino, identity)

    def test_worker_preflight_exempts_only_its_own_active_unit(self) -> None:
        """The worker's locked recheck permits itself and refuses another active capacity unit."""
        own = f"simplestchat-capacity-{RUN}.service"
        self.command.return_value = f"{own} loaded active running Capacity fixture"
        with patch.object(os, "geteuid", return_value=0):
            self.assertEqual(remote.preflight(allowed_unit=own)["containersBefore"], [])
            with self.assertRaisesRegex(remote.CapacityControlError, "capacity_unit_busy"):
                _ = remote.preflight()
            self.command.return_value = (
                f"{own} loaded active running Capacity fixture\n"
                + "simplestchat-capacity-other.service loaded active running"
            )
            with self.assertRaisesRegex(remote.CapacityControlError, "capacity_unit_busy"):
                _ = remote.preflight(allowed_unit=own)

    def test_preflight_refuses_even_a_stopped_public_project(self) -> None:
        """Private capacity cannot adopt a public host merely because its containers stopped."""
        self.ids.return_value = [IDENTITY]
        with (
            patch.object(os, "geteuid", return_value=0),
            self.assertRaisesRegex(remote.CapacityControlError, "public_project_refused"),
        ):
            _ = remote.preflight()
        self.ids.assert_called_once_with(label="com.docker.compose.project=simplestchat-public")
        self.unit.assert_not_called()
        self.command.assert_not_called()

    def test_recovery_removes_only_revalidated_ids_without_changing_measurement(self) -> None:
        """Explicit recovery unblocks clean ownership state while preserving the failed result."""
        self.prepare_collection()
        failed: dict[str, object] = {"passed": False, "failureClass": "worker_timeout"}
        self.write(self.directory / "outcome.json", failed)
        self.write(
            self.state / "current.json", {"schemaVersion": 1, "run": RUN, "finalized": False}
        )
        owned: dict[str, object] = {
            "clean": False,
            "residuals": [
                {"id": IDENTITY, "confirmedOwned": True, "inspection": {"Name": "capacity-server"}}
            ],
        }
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(
                remote, "residuals", side_effect=[owned, owned, {"clean": True, "residuals": []}]
            ),
        ):
            result = remote.recover(request())
        self.assertTrue(result["finalized"] is True)
        self.assertEqual(result["removed"], [IDENTITY])
        self.assertEqual(self.command.call_count, 2)
        self.command.assert_any_call([*remote.DOCKER, "stop", "--time", "20", IDENTITY], timeout=30)
        self.command.assert_any_call([*remote.DOCKER, "rm", "--volumes", IDENTITY], timeout=30)
        self.assertEqual(remote.read_json(self.directory / "outcome.json"), failed)
        journal = remote.read_json(self.state / "current.json")
        self.assertTrue(journal["finalized"] is True and journal["passed"] is False)
        self.assertTrue(journal["recovered"] is True)

    def test_recovery_retains_unknown_resources_and_unfinished_journal(self) -> None:
        """A foreign residual prevents finalization and receives no destructive command."""
        self.prepare_collection()
        journal: dict[str, object] = {"schemaVersion": 1, "run": RUN, "finalized": False}
        self.write(self.state / "current.json", journal)
        unknown: dict[str, object] = {
            "clean": False,
            "residuals": [{"id": FOREIGN, "confirmedOwned": False}],
        }
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(remote, "residuals", return_value=unknown),
        ):
            result = remote.recover(request())
        self.assertTrue(result["finalized"] is False)
        self.assertEqual(result["removed"], [])
        self.command.assert_not_called()
        self.assertEqual(remote.read_json(self.state / "current.json"), journal)

    def test_recovery_identity_change_aborts_before_any_stop(self) -> None:
        """Ownership must still hold at the final inspection immediately before removal."""
        self.prepare_collection()
        owned: dict[str, object] = {
            "clean": False,
            "residuals": [
                {"id": IDENTITY, "confirmedOwned": True, "inspection": {"Name": "capacity-server"}}
            ],
        }
        unknown: dict[str, object] = {
            "clean": False,
            "residuals": [{"id": IDENTITY, "confirmedOwned": False}],
        }
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(remote, "residuals", side_effect=[owned, unknown]),
            self.assertRaisesRegex(remote.CapacityControlError, "recovery_identity_changed"),
        ):
            _ = remote.recover(request())
        self.command.assert_not_called()
        receipt = remote.read_json(self.directory / "recovery.json")
        self.assertTrue(receipt["finalized"] is False)
        self.assertEqual(receipt["removed"], [])

    def test_recovery_refuses_active_units_and_excessive_residuals(self) -> None:
        """Recovery has its own bounded admission checks and never sweeps a broad container set."""
        self.prepare_collection()
        self.unit.return_value = unit_report(
            ActiveState="active", SubState="running", MainPID="123"
        )
        with self.assertRaisesRegex(remote.CapacityControlError, "capacity_still_running"):
            _ = remote.recover(request())
        self.command.assert_not_called()

        self.unit.return_value = unit_report(
            ActiveState="failed", SubState="failed", Result="exit-code"
        )
        excessive = {"clean": False, "residuals": [{"id": IDENTITY, "confirmedOwned": True}] * 7}
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(remote, "residuals", return_value=excessive),
            self.assertRaisesRegex(
                remote.CapacityControlError, "too_many_residuals_for_bounded_recovery"
            ),
        ):
            _ = remote.recover(request())
        self.command.assert_not_called()

    def test_recovery_removes_generator_before_shared_network_server(self) -> None:
        """Immutable IDs remain the removal targets while dependency order releases the netns."""
        self.prepare_collection()
        server: dict[str, object] = {
            "id": FOREIGN,
            "confirmedOwned": True,
            "inspection": {"Name": "capacity-server-fixture"},
        }
        generator: dict[str, object] = {
            "id": IDENTITY,
            "confirmedOwned": True,
            "inspection": {"Name": "capacity-gen-fixture"},
        }
        # Sorted immutable IDs would put the server first; names affect only
        # dependency order after the independent exact-identity proof.
        both: dict[str, object] = {"clean": False, "residuals": [server, generator]}
        remaining: dict[str, object] = {"clean": False, "residuals": [server]}
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(
                remote,
                "residuals",
                side_effect=[both, both, remaining, {"clean": True, "residuals": []}],
            ),
        ):
            result = remote.recover(request())
        self.assertEqual(result["removed"], [IDENTITY, FOREIGN])
        self.assertEqual(
            self.command.call_args_list,
            [
                call([*remote.DOCKER, "stop", "--time", "20", IDENTITY], timeout=30),
                call([*remote.DOCKER, "rm", "--volumes", IDENTITY], timeout=30),
                call([*remote.DOCKER, "stop", "--time", "20", FOREIGN], timeout=30),
                call([*remote.DOCKER, "rm", "--volumes", FOREIGN], timeout=30),
            ],
        )

    def test_recovery_cannot_finalize_another_runs_journal(self) -> None:
        """A changed global owner aborts finalization and leaves a truthful failed receipt."""
        self.prepare_collection()
        journal: dict[str, object] = {"schemaVersion": 1, "run": "0" * 32, "finalized": False}
        self.write(self.state / "current.json", journal)
        with (
            patch.object(remote, "workload_lock", return_value=nullcontext()),
            patch.object(remote, "residuals", return_value={"clean": True, "residuals": []}),
            self.assertRaisesRegex(remote.CapacityControlError, "recovery_journal_owner_changed"),
        ):
            _ = remote.recover(request())
        self.command.assert_not_called()
        self.assertEqual(remote.read_json(self.state / "current.json"), journal)
        self.assertTrue(remote.read_json(self.directory / "recovery.json")["finalized"] is False)


class UnitStateTests(unittest.TestCase):
    """Parse missing-unit responses without running systemctl or trusting empty failures."""

    def test_missing_unit_is_explicit_while_malformed_systemctl_output_fails(self) -> None:
        """GC can yield a nonzero show status; only an explicit known response is accepted."""
        unit = f"simplestchat-capacity-{RUN}.service"
        output = "LoadState=not-found\nActiveState=inactive\nMainPID=0\n"
        with patch.object(remote, "command", return_value=output) as command:
            self.assertEqual(remote.unit_state(unit)["LoadState"], "not-found")
            command.assert_called_once_with(
                [
                    "/usr/bin/systemctl",
                    "show",
                    unit,
                    "--property=LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,MainPID",
                ],
                allow_failure=True,
            )
        for output in ("", "Failed to connect to bus", "LoadState=not-found\n"):
            with (
                self.subTest(output=output),
                patch.object(remote, "command", return_value=output),
                self.assertRaisesRegex(remote.CapacityControlError, "invalid_unit_state"),
            ):
                _ = remote.unit_state(unit)


if __name__ == "__main__":
    _ = unittest.main()
