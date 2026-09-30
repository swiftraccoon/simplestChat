"""Validate encrypted archive selection and isolated restore boundaries without a network."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING, Unpack, final, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import backup_offsite as offsite
import release_public as release
import restore_verify as restore
from release_json import decode_json, object_value

if TYPE_CHECKING:
    from release_json import JsonObject

SNAPSHOT = "a" * 64
STAMP = "20260930T040000Z"
IMAGE = "sha256:" + "b" * 64
ARCHIVE = b"PGDMP-fixture-only"
MIGRATIONS = {"1": "c" * 96}


@final
class OffsiteBackupTests(unittest.TestCase):
    """Credential-free commands retain an exact encrypted snapshot and honest restore verdict."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Prepare placeholders without creating filesystem state at import time."""
        super().__init__(methodName)
        self.root = Path()
        self.config = Path()
        self.state = Path()
        self.attempt = Path()
        self.record: JsonObject = {}
        self.configured = offsite.Settings(
            "backup.example.test",
            "backup",
            22,
            "/archives/chat",
            64,
            restore.DEFAULT_RESTORE_LIMITS,
        )

    @override
    def setUp(self) -> None:
        """Use only private temporary files containing synthetic fixtures."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.config, self.state, self.attempt = (
            self.root / name for name in ("config", "state", "attempt")
        )
        for path in (self.config, self.state, self.attempt):
            path.mkdir(mode=0o700)
        work = self.root / "work"
        work.mkdir(mode=0o700)
        for name in ("identity", "known_hosts", "password"):
            path = self.config / name
            _ = path.write_text("fixture-private-content")
            path.chmod(0o600)
        self.record = {
            "schemaVersion": 1,
            "dump": f"{STAMP}.dump",
            "bytes": len(ARCHIVE),
            "sha256": hashlib.sha256(ARCHIVE).hexdigest(),
            "completedAt": release.timestamp(),
            "postgresImage": IMAGE,
            "backupMigrations": dict(MIGRATIONS),
        }
        settings: JsonObject = {
            "schemaVersion": 1,
            "host": "backup.example.test",
            "user": "backup",
            "port": 22,
            "repositoryPath": "/archives/chat",
            "archiveLimitMiB": 64,
            "restoreMemoryMiB": 512,
            "restoreDataMiB": 256,
        }
        release.atomic(self.config / "settings.json", settings)
        for change in (
            patch.object(offsite, "CONFIG", self.config),
            patch.object(offsite, "STATE", self.state),
            patch.object(release, "ROOT_UID", os.getuid()),
            patch.object(release, "WORK", work),
        ):
            _ = change.start()
            self.addCleanup(change.stop)

    def test_settings_and_ssh_are_strict_and_commands_contain_only_secret_paths(self) -> None:
        """No password value, agent or ambient SSH config can supply transport behavior."""
        configured = offsite.settings()
        self.assertEqual(configured, self.configured)
        command = configured.command()
        self.assertIn("--password-file", command)
        self.assertIn(str(self.config / "password"), command)
        arguments = " ".join(command)
        self.assertIn("StrictHostKeyChecking=yes", arguments)
        self.assertIn("IdentitiesOnly=yes", arguments)
        self.assertIn("IdentityAgent=none", arguments)
        self.assertIn("-F /dev/null", arguments)
        self.assertIn("-s backup@backup.example.test sftp", arguments)
        self.assertNotIn("fixture-private-content", arguments)
        (self.config / "identity").chmod(0o644)
        with self.assertRaises(release.ReleaseError):
            _ = offsite.settings()

    def test_unknown_settings_and_unbounded_resources_are_rejected(self) -> None:
        """There are no alternate formats or implicit resource fallbacks."""
        release.atomic(self.config / "settings.json", {"schemaVersion": 0})
        with self.assertRaises(release.ReleaseError):
            _ = offsite.settings()
        for limits in (
            restore.RestoreLimits(511, 256),
            restore.RestoreLimits(512, 512),
            restore.RestoreLimits(65537, 256),
        ):
            with self.subTest(limits=limits), self.assertRaises(release.ReleaseError):
                limits.validate()

    def test_upload_records_exact_snapshot_only_after_success(self) -> None:
        """A verified archive and receipt are the only source paths sent to restic."""
        archive, receipt = self.root / f"{STAMP}.dump", self.root / f"{STAMP}.receipt.json"
        runner = release.Runner(self.attempt)
        with (
            patch.object(offsite, "current_archive", return_value=(archive, receipt, self.record)),
            patch.object(
                runner,
                "run",
                return_value=json.dumps(
                    {"message_type": "summary", "snapshot_id": SNAPSHOT}
                ).encode(),
            ) as run,
        ):
            record = offsite.upload(runner, self.configured)
        run.assert_called_once_with(
            [
                *self.configured.command(),
                "backup",
                "--json",
                "--tag",
                "simplestchat-nightly",
                "--",
                str(archive),
                str(receipt),
            ],
            timeout=900,
        )
        self.assertEqual(record["snapshot"], SNAPSHOT)
        self.assertTrue((self.state / "offsite-latest.json").exists())
        self.assertFalse((self.state / "restore-verified.timestamp").exists())

    def test_failed_upload_does_not_replace_selection_or_mark_restoration(self) -> None:
        """Restic failures cannot claim remote backup freshness."""
        runner = release.Runner(self.attempt)
        with (
            patch.object(
                offsite,
                "current_archive",
                return_value=(Path("archive"), Path("receipt"), self.record),
            ),
            patch.object(runner, "run", side_effect=release.ReleaseError("fixture")),
            self.assertRaises(release.ReleaseError),
        ):
            _ = offsite.upload(runner, self.configured)
        self.assertFalse((self.state / "offsite-latest.json").exists())

    def test_exact_selection_refuses_other_repositories_and_partial_snapshot_ids(self) -> None:
        """An unrelated repository and ambiguous latest selectors never substitute for identity."""
        release.atomic(
            self.state / "offsite-latest.json",
            {
                "schemaVersion": 1,
                "repository": "sftp:other:/repo",
                "snapshot": SNAPSHOT,
                "stamp": STAMP,
            },
        )
        with self.assertRaises(release.ReleaseError):
            _ = offsite.selection(self.configured, "", "")
        for snapshot, stamp in (
            ("latest", STAMP),
            (SNAPSHOT[:8], STAMP),
            (SNAPSHOT, "../file"),
            (SNAPSHOT, ""),
        ):
            with self.subTest(snapshot=snapshot), self.assertRaises(release.ReleaseError):
                _ = offsite.selection(self.configured, snapshot, stamp)
        self.assertEqual(offsite.selection(self.configured, SNAPSHOT, STAMP), (SNAPSHOT, STAMP))

    def fixture_download(
        self, arguments: Sequence[str], **options: Unpack[release.CommandOptions]
    ) -> bytes:
        """Model only bounded restic file downloads, never a database or network call."""
        maximum = (
            release.MAX_BACKUP_RECEIPT_BYTES
            if arguments[-1].endswith(".json")
            else 64 * offsite.MIB
        )
        self.assertEqual(arguments[:3], ["/usr/bin/prlimit", f"--fsize={maximum}:{maximum}", "--"])
        self.assertEqual(arguments[-3:-1], ["dump", SNAPSHOT])
        target = options.get("output_path")
        self.assertIsNotNone(target)
        if target is not None:
            _ = target.write_bytes(
                json.dumps(self.record).encode() if target.suffix == ".json" else ARCHIVE
            )
            target.chmod(0o600)
        return b""

    def test_retrieved_archive_restores_exact_ledger_before_recording_success(self) -> None:
        """Only verified bytes and successful owned cleanup update the restore marker."""
        runner = release.Runner(self.attempt)
        with (
            patch.object(runner, "run", side_effect=self.fixture_download),
            patch.object(
                restore, "verify", return_value={"verified": True, "cleanupPassed": True}
            ) as verify,
        ):
            record = offsite.drill(runner, self.configured, SNAPSHOT, STAMP)
        self.assertEqual(record["backupSha256"], self.record["sha256"])
        self.assertTrue((self.state / "restore-verified.timestamp").exists())
        self.assertEqual(list(self.attempt.glob("download.*")), [])
        verify.assert_called_once()

    def test_failed_restore_cleanup_cannot_advance_success_marker(self) -> None:
        """Uncertain container cleanup stays failed and removes downloaded copies."""
        runner = release.Runner(self.attempt)
        with (
            patch.object(runner, "run", side_effect=self.fixture_download),
            patch.object(
                restore, "verify", return_value={"verified": True, "cleanupPassed": False}
            ),
            self.assertRaises(release.ReleaseError),
        ):
            _ = offsite.drill(runner, self.configured, SNAPSHOT, STAMP)
        self.assertFalse((self.state / "restore-verified.timestamp").exists())
        self.assertEqual(list(self.attempt.glob("download.*")), [])

    def test_archive_identity_requires_current_nightly_provenance(self) -> None:
        """A receipt without the exact image and migration map cannot drive a restore."""
        self.assertEqual(offsite.archive_identity(self.record), (IMAGE, MIGRATIONS))
        for key in ("postgresImage", "backupMigrations"):
            record = dict(self.record)
            del record[key]
            with self.subTest(key=key), self.assertRaises((release.ReleaseError, ValueError)):
                _ = offsite.archive_identity(record)

    def interrupted_attempt(self, action: str = "restore") -> Path:
        """Retain the exact current journal and a private attempted operation."""
        attempts = self.state / "offsite"
        attempts.mkdir(mode=0o700, exist_ok=True)
        attempt = attempts / (action + ".abcdefgh")
        attempt.mkdir(mode=0o700)
        release.atomic(
            release.WORK / "current.json",
            {
                "schemaVersion": 1,
                "operation": "offsite_backup",
                "action": action,
                "attempt": str(attempt),
                "finalized": False,
            },
        )
        return attempt

    def test_recovery_verifies_exact_container_before_removing_and_is_repeatable(self) -> None:
        """A create-response interruption still binds the unique name, label and image."""
        attempt = self.interrupted_attempt()
        name, identity = "scpub-restore-" + "d" * 32, "e" * 64
        release.atomic(
            attempt / "restore.json",
            {"container": name, "containerId": "", "postgresImage": IMAGE},
        )
        runner = release.Runner(self.attempt)
        present = True
        commands: list[tuple[str, ...]] = []

        def docker(*arguments: str, **_options: Unpack[release.CommandOptions]) -> bytes:
            nonlocal present
            commands.append(arguments)
            if arguments[0] == "ps":
                self.assertIn("name=^/" + name + "$", arguments)
                self.assertIn("label=" + restore.LABEL + "=" + name, arguments)
                return identity.encode() if present else b""
            if arguments[0] == "inspect":
                self.assertEqual(arguments[-1], identity)
                return json.dumps(
                    {"id": identity, "name": "/" + name, "image": IMAGE, "owner": name}
                ).encode()
            self.assertEqual(arguments, ("rm", "--force", "--volumes", identity))
            present = False
            return b""

        with patch.object(runner, "docker", side_effect=docker):
            offsite.recover_interrupted(runner)
            before_repeat = list(commands)
            offsite.recover_interrupted(runner)
        self.assertEqual(commands, before_repeat)
        self.assertFalse(present)
        current = object_value(decode_json((release.WORK / "current.json").read_text()))
        self.assertTrue(current["finalized"])
        self.assertFalse(current["passed"])
        self.assertTrue((self.attempt / "recovery.json").is_file())
        self.assertFalse((self.state / "restore-verified.timestamp").exists())

    def test_recovery_rejects_changed_image_label_name_or_id_without_removal(self) -> None:
        """A reused name or altered owner never authorizes destructive recovery."""
        attempt = self.interrupted_attempt()
        name, identity = "scpub-restore-" + "d" * 32, "e" * 64
        release.atomic(
            attempt / "restore.json",
            {"container": name, "containerId": identity, "postgresImage": IMAGE},
        )
        runner = release.Runner(self.attempt)
        for key in ("id", "name", "image", "owner"):
            metadata = {"id": identity, "name": "/" + name, "image": IMAGE, "owner": name}
            metadata[key] = "foreign"
            with (
                self.subTest(key=key),
                patch.object(
                    runner, "docker", side_effect=[identity.encode(), json.dumps(metadata).encode()]
                ) as docker,
                self.assertRaisesRegex(release.ReleaseError, "ownership changed"),
            ):
                offsite.recover_interrupted(runner)
            self.assertEqual(docker.call_count, 2)
        current = object_value(decode_json((release.WORK / "current.json").read_text()))
        self.assertFalse(current["finalized"])

    def test_recovery_before_container_intent_refuses_unrecorded_resources(self) -> None:
        """Missing creation evidence permits no label-wide deletion."""
        attempt = self.interrupted_attempt()
        download = attempt / "download.abcdefgh"
        download.mkdir(mode=0o700)
        partial = download / f"{STAMP}.dump"
        _ = partial.write_bytes(b"partial fixture")
        partial.chmod(0o600)
        runner = release.Runner(self.attempt)
        with (
            patch.object(runner, "docker", return_value=b"e" * 64) as docker,
            self.assertRaisesRegex(release.ReleaseError, "Unrecorded"),
        ):
            offsite.recover_interrupted(runner)
        self.assertEqual(docker.call_count, 1)
        with patch.object(runner, "docker", return_value=b""):
            offsite.recover_interrupted(runner)
        self.assertFalse(download.exists())
        self.assertTrue(json.loads((release.WORK / "current.json").read_text())["finalized"])

    def test_download_recovery_never_follows_symlinks_or_unrecognized_files(self) -> None:
        """Retained plaintext cleanup cannot reach outside its recorded private attempt."""
        attempt = self.interrupted_attempt()
        download = attempt / "download.abcdefgh"
        download.mkdir(mode=0o700)
        outside = self.root / "unrelated"
        _ = outside.write_bytes(b"preserve")
        link = download / f"{STAMP}.dump"
        link.symlink_to(outside)
        with self.assertRaises(release.ReleaseError):
            offsite.remove_downloads(attempt)
        self.assertEqual(outside.read_bytes(), b"preserve")
        link.unlink()
        unexpected = download / "operator-notes"
        unexpected.touch(mode=0o600)
        with self.assertRaises(release.ReleaseError):
            offsite.remove_downloads(attempt)
        self.assertTrue(unexpected.exists())

    def test_recovery_refuses_foreign_journal_and_keeps_failed_cleanup_unfinished(self) -> None:
        """Only the owning workflow can finalize, and failed removal retains the blocker."""
        attempt = self.interrupted_attempt()
        name, identity = "scpub-restore-" + "d" * 32, "e" * 64
        release.atomic(
            attempt / "restore.json",
            {"container": name, "containerId": identity, "postgresImage": IMAGE},
        )
        runner = release.Runner(self.attempt)
        with (
            patch.object(restore, "cleanup", side_effect=release.ReleaseError("fixture")),
            self.assertRaises(release.ReleaseError),
        ):
            offsite.recover_interrupted(runner)
        journal = release.WORK / "current.json"
        current = object_value(decode_json(journal.read_text()))
        self.assertFalse(current["finalized"])
        current["operation"] = "capacity"
        release.atomic(journal, current)
        with self.assertRaisesRegex(release.ReleaseError, "another operation"):
            offsite.recover_interrupted(runner)

    def test_service_recovers_after_exit_and_schedules_do_not_overlap(self) -> None:
        """Bounded post-stop recovery and separate timer windows are part of deployment."""
        directory = ROOT / "ops/ansible/templates"
        service = (directory / "backup-offsite.service.j2").read_text()
        self.assertIn("backup_offsite.py recover", service)
        self.assertIn("TimeoutStopSec=180", service)
        timer = (directory / "backup-offsite.timer.j2").read_text()
        self.assertIn("04:30:00 UTC", timer)
        self.assertIn("06:00:00 UTC", timer)

    def test_actual_cli_help_imports_without_service_or_credentials(self) -> None:
        """The installed entry point describes itself before any privileged action."""
        result = subprocess.run(  # noqa: S603 -- local help only.
            [sys.executable, str(ROOT / "ops/ansible/files/backup_offsite.py"), "--help"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("upload,restore", result.stdout)


if __name__ == "__main__":
    _ = unittest.main()
