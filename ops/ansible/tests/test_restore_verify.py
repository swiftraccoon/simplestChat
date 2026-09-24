"""Exercise restore isolation, cleanup failure and evidence publication boundaries."""

import hashlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Unpack
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_public as release
import restore_verify as restore
from release_json import decode_json, object_value


class RestoreTests(unittest.TestCase):
    """Use only owned temporary files and deterministic Docker responses."""

    def test_backup_snapshot_binds_exact_bytes_and_refuses_changed_archive(self) -> None:
        """A descriptor snapshot must match its recorded release digest before any restore."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            _ = source.write_bytes(b"owned archive fixture")
            source.chmod(0o600)
            info = source.stat()
            owned = os.stat_result((*info[:4], 0, *info[5:]))
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            with patch.object(os, "fstat", return_value=owned):
                restore.snapshot(source, root / "copy", digest)
                self.assertEqual((root / "copy").read_bytes(), source.read_bytes())
                with self.assertRaises(release.ReleaseError):
                    restore.snapshot(source, root / "bad-digest", "0" * 64)
                with (
                    patch.object(restore, "MAX_BACKUP", 1),
                    self.assertRaises(release.ReleaseError),
                ):
                    restore.snapshot(source, root / "too-large", digest)
            link = root / "link"
            link.symlink_to(source)
            with self.assertRaises(OSError):
                restore.snapshot(link, root / "symlink", digest)

    def test_container_has_no_live_mounts_ports_or_network(self) -> None:
        """The only writable storage is capped tmpfs and Docker may never pull an image."""
        arguments = restore.create_arguments("owned-fixture", "sha256:" + "a" * 64)
        for option, value in (
            ("--network", "none"),
            ("--pull", "never"),
            ("--memory", "512m"),
            ("--memory-swap", "512m"),
            ("--cpus", "0.5"),
            ("--cap-drop", "ALL"),
            ("--user", "999:999"),
            ("--log-driver", "none"),
        ):
            self.assertEqual(arguments[arguments.index(option) + 1], value)
        self.assertIn("--read-only", arguments)
        for forbidden in ("--volume", "-v", "--mount", "-p", "--publish", "--privileged"):
            self.assertNotIn(forbidden, arguments)
        self.assertNotIn("/run/simplestchat-postgres", " ".join(arguments))

    def test_restore_failure_and_cleanup_failure_cannot_return_success(self) -> None:
        """Both a partial restore and failed owned-container removal leave negative evidence."""
        for failure in ("none", "restore", "cleanup", "ledger"):
            with self.subTest(failure=failure):
                self.check_restore_case(failure)

    def check_restore_case(self, failure: str) -> None:
        """Execute one isolated lifecycle with a selected deterministic failure."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runner = release.Runner(root)
            identity = "a" * 64
            present = False
            commands: list[tuple[str, ...]] = []

            def docker(*arguments: str, **_options: Unpack[release.CommandOptions]) -> bytes:
                nonlocal present
                commands.append(arguments)
                if arguments[0] == "create":
                    present = True
                    return identity.encode()
                if arguments[0] == "ps":
                    return identity.encode() if present else b""
                if arguments[0] == "rm":
                    self.assertEqual(arguments[-1], identity)
                    if failure == "cleanup":
                        message = "fixture cleanup failure"
                        raise release.ReleaseError(message)
                    present = False
                if "pg_restore" in arguments and failure == "restore":
                    message = "fixture restore failure"
                    raise release.ReleaseError(message)
                return b""

            ledger = b"1|t|" + b"b" * 96 if failure != "ledger" else b"1|f|wrong"
            with (
                patch.object(runner, "docker", side_effect=docker),
                patch.object(restore, "sql", side_effect=[b"", ledger, b'{"activeIncidents":1}']),
                patch.object(restore, "VERIFY_SQL", ROOT / "ops/ansible/files/restore-verify.sql"),
            ):
                if failure == "none":
                    report = restore.verify(
                        runner, "sha256:" + "c" * 64, root / "archive", {"1": "b" * 96}
                    )
                    self.assertTrue(report["verified"])
                    self.assertTrue(report["cleanupPassed"])
                else:
                    with self.assertRaises(release.ReleaseError):
                        _ = restore.verify(
                            runner, "sha256:" + "c" * 64, root / "archive", {"1": "b" * 96}
                        )
            evidence = object_value(decode_json((root / "restore.json").read_text()))
            self.assertEqual(evidence["cleanupPassed"], failure != "cleanup")
            self.assertEqual(present, failure == "cleanup")
            self.assertTrue(any(command[0] == "rm" for command in commands))
            self.assertFalse((root / "restore-verified.timestamp").exists())

    def test_ambiguous_cleanup_never_removes_multiple_containers(self) -> None:
        """A malformed daemon selection cannot widen the owned cleanup scope."""
        with tempfile.TemporaryDirectory() as directory:
            runner = release.Runner(Path(directory))
            with patch.object(runner, "docker", return_value=b"a\nb") as docker:
                with self.assertRaises(release.ReleaseError):
                    restore.cleanup(runner, "owned-fixture")
                self.assertEqual(docker.call_count, 1)

    def test_timestamp_requires_verification_and_cleanup_and_snapshot_removal(self) -> None:
        """A failed restore preserves previous success evidence and deletes its private copy."""
        for verified, cleaned in ((True, True), (False, True), (True, False)):
            with (
                self.subTest(verified=verified, cleaned=cleaned),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                state = root / "monitoring"
                state.mkdir()
                marker = state / "restore-verified.timestamp"
                _ = marker.write_bytes(b"previous verified restore")
                source = root / "results/release.fixture000"
                source.mkdir(parents=True)
                _ = (source / "outcome.json").write_text(
                    json.dumps(
                        {
                            "revision": "a" * 40,
                            "backupSha256": "b" * 64,
                        }
                    )
                )
                with (
                    patch.object(sys, "argv", ["restore_verify.py", "release.fixture000"]),
                    patch.object(sys, "stdout", io.StringIO()),
                    patch.object(os, "geteuid", return_value=0),
                    patch.object(release, "ROOT", root),
                    patch.object(release, "protected"),
                    patch.object(release, "workload_lock"),
                    patch.object(restore, "STATE", state),
                    patch.object(
                        restore,
                        "validate_manifest",
                        return_value={
                            "revision": "a" * 40,
                            "migrations": {"1": "c" * 96},
                        },
                    ),
                    patch.object(restore, "snapshot", side_effect=self.write_snapshot_fixture),
                    patch.object(release.Runner, "docker", return_value=b"sha256:" + b"d" * 64),
                    patch.object(
                        restore,
                        "verify",
                        return_value={
                            "verified": verified,
                            "cleanupPassed": cleaned,
                        },
                    ),
                ):
                    if verified and cleaned:
                        restore.main()
                        self.assertNotEqual(marker.read_bytes(), b"previous verified restore")
                    else:
                        with self.assertRaises(release.ReleaseError):
                            restore.main()
                        self.assertEqual(marker.read_bytes(), b"previous verified restore")
                self.assertEqual(list(state.glob("restores/*/snapshot.dump")), [])

    @staticmethod
    def write_snapshot_fixture(_source: Path, target: Path, _digest: str) -> None:
        """Materialize an owned copy so the publication test also checks actual removal."""
        _ = target.write_bytes(b"owned snapshot fixture")


if __name__ == "__main__":
    _ = unittest.main()
