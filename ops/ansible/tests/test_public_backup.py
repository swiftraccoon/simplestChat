"""Nightly archive lifecycle, receipt trust and durable publication without a daemon."""

import hashlib
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from collections.abc import Callable
from contextlib import suppress
from pathlib import Path
from typing import TYPE_CHECKING, final, override
from unittest.mock import Mock, patch

from test_public_templates import render
from test_support import ROOT, yaml_value

# isort: split
import backup_public as backup
import release_public as release

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue


class NightlyBackupTests(unittest.TestCase):
    """Backups validate before they count, and cleanup cannot cross ownership boundaries."""

    def test_real_quiet_timeout_preserves_failure_after_zombie_only_cleanup(self) -> None:
        """An exited single-member group is harmless, including Darwin's special EPERM case."""
        with tempfile.TemporaryDirectory() as directory:
            runner = release.Runner(Path(directory))
            with self.assertRaises(subprocess.TimeoutExpired):
                _ = runner.run([sys.executable, "-c", "import time; time.sleep(30)"], timeout=0.1)

    def test_runner_kills_term_ignoring_descendant_before_reaping_leader(self) -> None:
        """A real local fork cannot outlive timeout cleanup just because its leader exits."""
        script = (
            "import os,signal,sys,time; from pathlib import Path\n"
            + "pid=os.fork()\n"
            + "if pid == 0:\n"
            + " signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
            + " Path(sys.argv[1]).write_text(str(os.getpid()))\n"
            + "while True: time.sleep(0.02)\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_file = root / "child.pid"
            runner = release.Runner(root)
            descendant: int | None = None
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    _ = runner.run([sys.executable, "-c", script, str(child_file)], timeout=0.5)
                descendant = int(child_file.read_text())
                status = subprocess.run(  # noqa: S603 -- read-only query for this fixture's PID.
                    ["/bin/ps", "-o", "stat=", "-p", str(descendant)],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                self.assertTrue(status.returncode != 0 or status.stdout.strip().startswith("Z"))
            finally:
                if descendant is None and child_file.is_file():
                    descendant = int(child_file.read_text())
                if descendant is not None:
                    with suppress(ProcessLookupError):
                        os.kill(descendant, signal.SIGKILL)

    def test_wrapper_preserves_failure_and_runs_guarded_exit_cleanup(self) -> None:
        """The wrapper has bounded inputs, signal exits and an unconditional cleanup trap."""
        script = render("backup-nightly.sh.j2")
        result = subprocess.run(
            ["/bin/bash", "-n"], input=script, text=True, check=False, capture_output=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("trap cleanup EXIT", script)
        self.assertIn('exit "$status"', script)
        self.assertIn('"$helper" cleanup', script)
        self.assertIn("keep_days=14", script)
        self.assertIn("keep_at_least=3", script)

    def test_units_share_the_canonical_runtime_lock_directory(self) -> None:
        """The timer survives reboot, and its worker can open only the intended lock root."""
        timer = render("backup-nightly.timer.j2")
        self.assertIn("OnCalendar=*-*-* 04:00:00 UTC", timer)
        self.assertIn("Persistent=true", timer)
        service = render("backup-nightly.service.j2")
        self.assertIn("Type=oneshot", service)
        self.assertIn("RuntimeDirectory=simplestchat-bench", service)
        self.assertIn("RuntimeDirectoryPreserve=yes", service)
        self.assertIn(
            "ReadWritePaths=/srv/simplestchat-public/backups /run/simplestchat-bench", service
        )
        self.assertIn("ProtectSystem=strict", service)

    def test_playbook_bounds_retention_before_installing_anything(self) -> None:
        """Inventory types are checked before Jinja's integer conversion can lose information."""
        text = (ROOT / "ops/ansible/backup.yml").read_text()
        self.assertIsInstance(yaml_value(text), list)
        for expected in (
            "scpub_backup_enabled | bool",
            "scpub_backup_keep_days | default(14) is integer",
            "scpub_backup_keep_at_least | default(3) is integer",
            "name: simplestchat-backup.timer",
            "backup_public.py",
        ):
            self.assertIn(expected, text)

    def test_retention_refuses_malformed_or_extreme_values(self) -> None:
        """Neither negative indexes nor shell text can reach deletion logic."""
        for days, count in ((0, 3), (-1, 3), (3651, 3), (14, 0), (14, 366), (True, 3)):
            with self.subTest(days=days, count=count), self.assertRaises(release.ReleaseError):
                backup.retention(days, count)
        backup.retention(1, 1)
        backup.retention(3650, 365)

    def test_cleanup_removes_only_known_private_partials(self) -> None:
        """Unknown names and complete archives survive; symlink partials are rejected."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(release, "ROOT_UID", os.getuid()),
        ):
            target = Path(directory)
            owned = target / "20260930T040000Z.dump.partial"
            owned.touch(mode=0o600)
            unrelated = target / "operator.partial"
            unrelated.touch(mode=0o600)
            complete = target / "20260930T040000Z.dump"
            complete.touch(mode=0o600)
            backup.cleanup_partials(target)
            self.assertFalse(owned.exists())
            self.assertTrue(unrelated.exists())
            self.assertTrue(complete.exists())
            owned.symlink_to(complete)
            with self.assertRaises(release.ReleaseError):
                backup.cleanup_partials(target)
            self.assertTrue(complete.exists())

    def test_dump_failure_removes_its_partial_without_publishing_receipt(self) -> None:
        """A failed pg_dump leaves command evidence but no archive freshness claim."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(release, "ROOT_UID", os.getuid()),
        ):
            target = Path(directory)
            runner = Mock(docker=Mock(side_effect=[b"a" * 64, b"sha256:" + b"b" * 64]))

            def failed_dump(_runner: object, _database: str, partial: Path) -> None:
                _ = partial.write_bytes(b"PGDMP-incomplete")
                raise release.ReleaseError("fixture dump failed")  # noqa: EM101, TRY003

            with (
                patch.object(release, "Runner", return_value=runner),
                patch.object(release, "backup_headroom"),
                patch.object(release, "ledger", return_value={"1": "a" * 96}),
                patch.object(backup, "dump_database", side_effect=failed_dump),
                self.assertRaises(release.ReleaseError),
            ):
                _ = backup.nightly(target, 14, 3)
            self.assertEqual(list(target.glob("*.partial")), [])
            self.assertEqual(list(target.glob("*.receipt.json")), [])
            self.assertEqual(list(target.glob("*.dump")), [])
            self.assertEqual(len(list(target.glob("*.logs"))), 1)

    def test_successful_nightly_receipt_binds_image_and_migration_ledger(self) -> None:
        """The published current archive contains everything an exact-format drill needs."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(release, "ROOT_UID", os.getuid()),
        ):
            target = Path(directory)
            runner = Mock(docker=Mock(side_effect=[b"a" * 64, b"sha256:" + b"b" * 64]))

            def completed_dump(_runner: object, _database: str, partial: Path) -> None:
                _ = partial.write_bytes(b"PGDMP-complete")
                partial.chmod(0o600)

            with (
                patch.object(release, "Runner", return_value=runner),
                patch.object(release, "backup_headroom"),
                patch.object(release, "ledger", return_value={"21": "a" * 96}),
                patch.object(backup, "dump_database", side_effect=completed_dump),
            ):
                dump = backup.nightly(target, 14, 3)
            receipt = dump.with_suffix(".receipt.json")
            _dump, record, _completed = release.validated_backup(receipt)
            self.assertEqual(record["backupMigrations"], {"21": "a" * 96})
            self.assertEqual(record["postgresImage"], "sha256:" + "b" * 64)
            self.assertEqual(list(target.glob("*.partial")), [])

    def test_only_expired_owned_nightly_work_can_be_recovered(self) -> None:
        """Unknown operations and malformed boot identities never age into authorization."""
        boot = "a" * 8 + "-" + "b" * 4 + "-" + "c" * 4 + "-" + "d" * 4 + "-" + "e" * 12
        record: JsonObject = {"operation": "nightly_backup", "untilMonotonic": 500, "bootId": boot}
        with (
            patch.object(release, "boot_id", return_value=boot),
            patch.object(time, "monotonic", return_value=400),
        ):
            self.assertFalse(release.expired_backup(record))
        with (
            patch.object(release, "boot_id", return_value=boot),
            patch.object(time, "monotonic", return_value=501),
        ):
            self.assertTrue(release.expired_backup(record))
            self.assertFalse(release.expired_backup(dict(record, operation="capacity")))
            self.assertFalse(release.expired_backup(dict(record, bootId="invalid")))
        with patch.object(release, "boot_id", return_value=boot.replace("a", "f")):
            self.assertTrue(release.expired_backup(record))

    def test_database_size_controls_the_free_space_reserve(self) -> None:
        """A large database is refused even when the historical fixed 1 GiB guard passed."""
        runner = release.Runner(Path())
        with (
            patch.object(runner, "docker", return_value=b"2147483648\n"),
            patch.object(
                shutil, "disk_usage", return_value=type("Usage", (), {"free": 3 * 1024**3})()
            ),
            self.assertRaisesRegex(release.ReleaseError, "headroom"),
        ):
            release.backup_headroom(runner, "a" * 64)

    def test_busy_lock_prevents_backup_and_cleanup(self) -> None:
        """The backup cannot bypass a release or benchmark lock, including the exit cleanup."""
        for action in ("nightly", "cleanup"):
            with (
                self.subTest(action=action),
                patch.object(sys, "argv", ["backup_public.py", action]),
                patch.object(os, "geteuid", return_value=0),
                patch.object(os, "umask"),
                patch.object(signal, "signal"),
                patch.object(release, "workload_lock", side_effect=BlockingIOError),
                patch.object(backup, "nightly") as nightly,
                patch.object(backup, "cleanup_partials") as cleanup,
                self.assertRaises(BlockingIOError),
            ):
                backup.main()
            nightly.assert_not_called()
            cleanup.assert_not_called()


@final
class BackupIntegrityTests(unittest.TestCase):
    """Same-size corruption and forged freshness metadata do not pass archive validation."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Initialize harmless placeholders without creating files during discovery."""
        super().__init__(methodName)
        self.root = Path()
        self.dump = Path()
        self.receipt = Path()
        self.record: JsonObject = {}

    @override
    def setUp(self) -> None:
        """Create ordinary private files owned by this disposable test user."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.dump = self.root / "database-before.dump"
        _ = self.dump.write_bytes(b"PGDMP-fixture")
        self.dump.chmod(0o600)
        self.receipt = self.root / "database-before.receipt.json"
        self.record = {
            "schemaVersion": 1,
            "dump": self.dump.name,
            "bytes": self.dump.stat().st_size,
            "sha256": hashlib.sha256(self.dump.read_bytes()).hexdigest(),
            "completedAt": release.timestamp(),
        }
        self.write_record()
        owner = patch.object(release, "ROOT_UID", os.getuid())
        _ = owner.start()
        self.addCleanup(owner.stop)

    def write_record(self) -> None:
        """Update only the disposable private receipt."""
        _ = self.receipt.write_text(json.dumps(self.record))
        self.receipt.chmod(0o600)

    def test_valid_record_binds_size_hash_and_completion_time(self) -> None:
        """A complete private archive supplies its own recorded completion timestamp."""
        dump, record, completed = release.validated_backup(self.receipt)
        self.assertEqual(dump, self.dump)
        self.assertEqual(record, self.record)
        self.assertGreater(completed, 0)

    def test_same_size_corruption_is_rejected(self) -> None:
        """Size equality cannot substitute for digest verification."""
        _ = self.dump.write_bytes(b"PGDMP-altered")
        with self.assertRaisesRegex(release.ReleaseError, "digest"):
            _ = release.validated_backup(self.receipt)

    def test_digest_cache_invalidates_same_size_rewrites(self) -> None:
        """Same-size corruption fails even when filesystem timestamps collide exactly."""
        metadata = self.dump.stat()
        current = max(metadata.st_mtime_ns, metadata.st_ctime_ns) / 1_000_000_000
        frozen = self.frozen_dump_stat()
        for later in (0, release.BACKUP_HASH_TIMESTAMP_MARGIN_SECONDS + 1):
            with self.subTest(later=later):
                _ = self.dump.write_bytes(b"PGDMP-fixture")
                cache: JsonObject = {}
                with (
                    patch.object(os, "fstat", side_effect=frozen),
                    patch.object(time, "time", return_value=current) as clock,
                ):
                    _ = release.validated_backup(self.receipt, digest_cache=cache)
                    self.assertEqual(cache, {})
                    _ = self.dump.write_bytes(b"PGDMP-altered")
                    clock.return_value = current + later
                    with self.assertRaisesRegex(release.ReleaseError, "digest"):
                        _ = release.validated_backup(self.receipt, digest_cache=cache)

    def frozen_dump_stat(self) -> Callable[[int], os.stat_result]:
        """Simulate a coarse filesystem reporting identical metadata after a rewrite."""
        snapshot = self.dump.stat()
        actual_fstat = os.fstat

        def metadata(descriptor: int) -> os.stat_result:
            actual = actual_fstat(descriptor)
            if (actual.st_dev, actual.st_ino) == (snapshot.st_dev, snapshot.st_ino):
                return snapshot
            return actual

        return metadata

    def test_stable_hash_cache_expires_and_rejects_clock_rollback(self) -> None:
        """Old unchanged dumps reuse hashes, but expiry and backward time force a read."""
        metadata = self.dump.stat()
        current = max(metadata.st_mtime_ns, metadata.st_ctime_ns) / 1_000_000_000 + 10
        for age, hashes in ((1, 1), (release.BACKUP_HASH_CACHE_SECONDS, 2), (-1, 2)):
            with (
                self.subTest(age=age),
                patch.object(time, "time", return_value=current) as clock,
                patch.object(hashlib, "file_digest", wraps=hashlib.file_digest) as digest,
            ):
                cache: JsonObject = {}
                _ = release.validated_backup(self.receipt, digest_cache=cache)
                self.assertEqual(cache.get("schemaVersion"), 2)
                clock.return_value = current + age
                _ = release.validated_backup(self.receipt, digest_cache=cache)
                self.assertEqual(digest.call_count, hashes)

    def test_old_contract_and_hashes_checked_while_fresh_cannot_be_reused(self) -> None:
        """Neither legacy entries nor initially racy signatures become trusted by waiting."""
        metadata = self.dump.stat()
        modified = max(metadata.st_mtime_ns, metadata.st_ctime_ns) / 1_000_000_000
        for change in ("legacy", "fresh"):
            with (
                self.subTest(change=change),
                patch.object(time, "time", return_value=modified + 10),
                patch.object(hashlib, "file_digest", wraps=hashlib.file_digest) as digest,
            ):
                cache: JsonObject = {}
                _ = release.validated_backup(self.receipt, digest_cache=cache)
                if change == "legacy":
                    del cache["schemaVersion"]
                else:
                    cache["checkedAt"] = modified
                _ = release.validated_backup(self.receipt, digest_cache=cache)
                self.assertEqual(digest.call_count, 2)

    def test_slow_hash_cannot_make_fresh_timestamps_cacheable(self) -> None:
        """Eligibility is captured before reading, even when hashing spans the margin."""
        metadata = self.dump.stat()
        current = max(metadata.st_mtime_ns, metadata.st_ctime_ns) / 1_000_000_000
        with (
            patch.object(time, "time", return_value=current) as clock,
            patch.object(hashlib, "file_digest", wraps=hashlib.file_digest) as digest,
        ):
            actual_digest = hashlib.sha256(self.dump.read_bytes())

            def slow_digest(*_arguments: object) -> object:
                clock.return_value = current + release.BACKUP_HASH_TIMESTAMP_MARGIN_SECONDS + 1
                return actual_digest

            digest.side_effect = slow_digest
            cache: JsonObject = {}
            _ = release.validated_backup(self.receipt, digest_cache=cache)
            self.assertEqual(cache, {})

    def test_bad_schema_types_names_and_dates_are_rejected(self) -> None:
        """Boolean versions, traversal, malformed hashes and future dates cannot count."""
        cases: tuple[tuple[str, JsonValue], ...] = (
            ("schemaVersion", True),
            ("schemaVersion", 2),
            ("bytes", True),
            ("bytes", "13"),
            ("dump", "../outside.dump"),
            ("sha256", "z" * 64),
            ("completedAt", "2099-01-01T00:00:00Z"),
            ("completedAt", "2026-01-01"),
        )
        for key, value in cases:
            with self.subTest(key=key, value=value):
                candidate = dict(self.record)
                candidate[key] = value
                _ = self.receipt.write_text(json.dumps(candidate))
                with self.assertRaises((release.ReleaseError, ValueError)):
                    _ = release.validated_backup(self.receipt)

    def test_private_permissions_owner_and_links_are_required(self) -> None:
        """Readable files, foreign owners, hardlinks and symlinks fail before acceptance."""
        for path in (self.receipt, self.dump):
            with self.subTest(path=path):
                path.chmod(0o644)
                with self.assertRaises(release.ReleaseError):
                    _ = release.validated_backup(self.receipt)
                path.chmod(0o600)
        with (
            patch.object(release, "ROOT_UID", os.getuid() + 1),
            self.assertRaises(release.ReleaseError),
        ):
            _ = release.validated_backup(self.receipt)
        link = self.root / "another.dump"
        os.link(self.dump, link)
        with self.assertRaises(release.ReleaseError):
            _ = release.validated_backup(self.receipt)
        link.unlink()
        self.dump.unlink()
        self.dump.symlink_to(self.receipt)
        with self.assertRaises(release.ReleaseError):
            _ = release.validated_backup(self.receipt)

    def test_archive_flush_precedes_rename_and_receipt_flush(self) -> None:
        """The archive reaches durable storage before a durable receipt can name it."""
        partial = self.root / "new.dump.partial"
        _ = partial.write_bytes(b"PGDMP-new")
        destination = self.root / "new.dump"
        events: list[str] = []
        replace = Path.replace

        def observe_flush(descriptor: int) -> None:
            events.append("directory" if stat.S_ISDIR(os.fstat(descriptor).st_mode) else "file")

        def observe_replace(source: Path, target: Path) -> Path:
            events.append("rename")
            return replace(source, target)

        with (
            patch.object(os, "fsync", side_effect=observe_flush),
            patch.object(Path, "replace", autospec=True, side_effect=observe_replace),
        ):
            release.publish_backup(partial, destination)
            release.atomic(self.root / "new.receipt.json", {"schemaVersion": 1})
        self.assertEqual(events, ["file", "rename", "directory", "file", "rename", "directory"])


if __name__ == "__main__":
    _ = unittest.main()
