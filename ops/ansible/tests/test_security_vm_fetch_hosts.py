"""Exercise only temporary SSH homes, never the developer's real profile."""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

sys.path.insert(0, str(ROOT / "build"))
from security_tools import ToolError
from security_vm_fetch_hosts import KnownHost

KEY = "ssh-ed25519 " + "A" * 68 + " vm-fixture"


class KnownHostTests(unittest.TestCase):
    """Verify collision refusal and exact ownership through finalization faults."""

    def test_owns_only_generated_entry_and_removes_own_empty_directory(self) -> None:
        """Exercise actual exclusive publication and checked retirement."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            owned = KnownHost(home, 22345, KEY)
            path = home / ".ssh/known_hosts"
            self.assertEqual(path.read_text(), f"[127.0.0.1]:22345 {KEY}\n")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            owned.check()
            owned.finish()
            self.assertFalse((home / ".ssh").exists())

    def test_preserves_preexisting_empty_ssh_directory(self) -> None:
        """Keep a caller-owned directory even when the fixture file is removed."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            ssh = home / ".ssh"
            ssh.mkdir(mode=0o700)
            identity = ssh.stat().st_ino
            owned = KnownHost(home, 22345, KEY)
            owned.finish()
            self.assertEqual(ssh.stat().st_ino, identity)
            self.assertEqual(list(ssh.iterdir()), [])

    def test_existing_config_or_known_hosts_is_never_overwritten(self) -> None:
        """Refuse collisions without changing their bytes or inode."""
        for leaf in ("config", "known_hosts"):
            with self.subTest(leaf=leaf), tempfile.TemporaryDirectory() as name:
                home = Path(name).resolve()
                ssh = home / ".ssh"
                ssh.mkdir(mode=0o700)
                path = ssh / leaf
                _ = path.write_bytes(b"preexisting\n")
                identity = path.stat().st_ino
                with self.assertRaises(ToolError):
                    _ = KnownHost(home, 22345, KEY)
                self.assertEqual(path.read_bytes(), b"preexisting\n")
                self.assertEqual(path.stat().st_ino, identity)

    def test_symlinked_directory_and_broken_collision_are_refused(self) -> None:
        """A symlink cannot turn absence checks into a writable profile."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            target = home / "original"
            target.mkdir(mode=0o700)
            (home / ".ssh").symlink_to(target, target_is_directory=True)
            with self.assertRaises(OSError):
                _ = KnownHost(home, 22345, KEY)
            self.assertEqual(list(target.iterdir()), [])
            (home / ".ssh").unlink()
            _ = target.rename(home / ".ssh")
            link = home / ".ssh/known_hosts"
            link.symlink_to(home / "absent")
            with self.assertRaises(ToolError):
                _ = KnownHost(home, 22345, KEY)
            self.assertTrue(link.is_symlink())

    def test_replacement_is_retained_and_all_original_descriptors_close(self) -> None:
        """Reject a later namespace generation without deleting its replacement."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            owned = KnownHost(home, 22345, KEY)
            originals = owned.home_fd, owned.ssh_fd, owned.file_fd
            path = home / ".ssh/known_hosts"
            _ = path.rename(path.with_name("old"))
            _ = path.write_bytes(b"replacement\n")
            replacement = path.stat().st_ino
            with self.assertRaises(ToolError):
                owned.check()
            with self.assertRaises(ToolError):
                owned.finish()
            self.assertEqual(path.read_bytes(), b"replacement\n")
            self.assertEqual(path.stat().st_ino, replacement)
            for descriptor in originals:
                if descriptor is None:
                    self.fail("fixture did not retain an original descriptor")
                with self.assertRaises(OSError):
                    _ = os.fstat(descriptor)

    def test_same_inode_mutation_is_refused_and_retained(self) -> None:
        """An unchanged inode cannot conceal altered profile bytes."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            owned = KnownHost(home, 22345, KEY)
            path = home / ".ssh/known_hosts"
            original = path.stat().st_ino
            _ = path.write_bytes(b"changed\n")
            with self.assertRaises(ToolError):
                owned.check()
            with self.assertRaises(ToolError):
                owned.finish()
            self.assertEqual(path.stat().st_ino, original)
            self.assertEqual(path.read_bytes(), b"changed\n")

    def test_close_failure_cannot_prevent_other_peer_close_attempts(self) -> None:
        """Keep the close error while attempting the two remaining peers."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()
            owned = KnownHost(home, 22345, KEY)
            originals = [owned.file_fd, owned.ssh_fd, owned.home_fd]
            close = os.close
            attempted: list[int] = []

            def fail_first(descriptor: int) -> None:
                attempted.append(descriptor)
                close(descriptor)
                if len(attempted) == 1:
                    message = "inert_close"
                    raise OSError(message)

            with (
                patch("security_vm_fetch_hosts.os.close", side_effect=fail_first),
                self.assertRaises(OSError),
            ):
                owned.finish()
            self.assertEqual(attempted, originals)
            self.assertFalse((home / ".ssh").exists())

    def test_unsealed_partial_write_is_retained_after_constructor_failure(self) -> None:
        """An early same-inode write cannot receive identity-only deletion credit."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()

            def changed_then_failed(descriptor: int, _data: object) -> int:
                _ = os.pwrite(descriptor, b"changed-before-error\n", 0)
                message = "inert_write"
                raise OSError(message)

            with (
                patch("security_vm_fetch_hosts.os.write", side_effect=changed_then_failed),
                self.assertRaises(OSError),
            ):
                _ = KnownHost(home, 22345, KEY)
            self.assertEqual((home / ".ssh/known_hosts").read_bytes(), b"changed-before-error\n")

    def test_success_shaped_wrong_write_is_not_sealed_for_retirement(self) -> None:
        """A full returned byte count cannot authorize altered file contents."""
        with tempfile.TemporaryDirectory() as name:
            home = Path(name).resolve()

            def wrong_bytes(descriptor: int, data: memoryview) -> int:
                changed = b"x" * len(data)
                return os.pwrite(descriptor, changed, 0)

            with (
                patch("security_vm_fetch_hosts.os.write", side_effect=wrong_bytes),
                self.assertRaises(ToolError),
            ):
                _ = KnownHost(home, 22345, KEY)
            self.assertTrue((home / ".ssh/known_hosts").read_bytes().startswith(b"xxx"))


if __name__ == "__main__":
    _ = unittest.main()
