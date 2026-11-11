"""Owned filesystem and real Ansible-expression checks; never touch a deployed host."""

from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import ExitStack, contextmanager, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

import test_support

# isort: split
import browser_assets as command
import release_public as public
from release_json import JsonObject, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Generator

REVISION = "a" * 40
OTHER_REVISION = "b" * 40


@dataclass(frozen=True)
class Fixture:
    """One private deployment tree under the current unprivileged test owner."""

    root: Path

    def initialize(self, revision: str = REVISION) -> tuple[int, JsonObject]:
        """Execute the real command, keeping its bounded receipt in memory."""
        output = io.StringIO()
        with (
            patch.object(sys, "argv", ["browser_assets.py", "initialize", "--revision", revision]),
            redirect_stdout(output),
        ):
            status = command.main()
        return status, object_value(decode_json(output.getvalue()))

    def snapshot(self) -> dict[str, tuple[int, int, int, int, bytes | str | None]]:
        """Record bytes, modes, identity and write timestamps without accessing any host path."""
        store = self.root / "browser-assets"
        result: dict[str, tuple[int, int, int, int, bytes | str | None]] = {}
        for path in [store, *store.rglob("*")]:
            metadata = path.lstat()
            content = (
                str(path.readlink())
                if path.is_symlink()
                else path.read_bytes()
                if path.is_file()
                else None
            )
            result[str(path.relative_to(store))] = (
                metadata.st_mode,
                metadata.st_ino,
                metadata.st_mtime_ns,
                metadata.st_ctime_ns,
                content,
            )
        return result


@contextmanager
def owned_command() -> Generator[Fixture]:
    """Redirect only fixed deployment paths/owner, retaining real validation and flock."""
    with tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
        directory = Path(temporary).resolve()
        root, config, work = (directory / name for name in ("public", "configuration", "work"))
        for path in (root, config, work):
            path.mkdir(mode=0o700)
        owner = root.stat().st_uid
        for module, name, value in [
            (command, "ROOT", root),
            (command, "ROOT_UID", owner),
            (public, "ROOT", root),
            (public, "CONFIG", config),
            (public, "WORK", work),
            (public, "ROOT_UID", owner),
        ]:
            _ = stack.enter_context(patch.object(module, name, value))
        previous = os.umask(0o077)
        _ = stack.callback(os.umask, previous)
        yield Fixture(root)


class BrowserAssetCommandTests(unittest.TestCase):
    """Initialization reports actual persistent changes without rewriting stable state."""

    def test_first_initialization_changes_and_second_is_a_verified_filesystem_noop(self) -> None:
        """The second pass retains exact bytes, inodes, modes and write timestamps."""
        helpers = test_support.ROOT / "ops/ansible/files"
        self.assertEqual(Path(str(command.__file__)).parent, helpers)
        with owned_command() as fixture:
            status, receipt = fixture.initialize()
            self.assertEqual(status, 0)
            self.assertEqual(receipt, {"passed": True, "changed": True})
            before = fixture.snapshot()
            status, receipt = fixture.initialize()
            self.assertEqual(status, 0)
            self.assertEqual(receipt, {"passed": True, "changed": False})
            self.assertEqual(fixture.snapshot(), before)

    def test_revision_change_is_reported_once_and_identical_selection_stays_unchanged(self) -> None:
        """A new selection is still a real Ansible change, including after rollback."""
        with owned_command() as fixture:
            self.assertEqual(fixture.initialize()[0], 0)
            for revision in (OTHER_REVISION, REVISION):
                status, receipt = fixture.initialize(revision)
                self.assertEqual(status, 0)
                self.assertEqual(receipt, {"passed": True, "changed": True})
                version = fixture.root / "browser-assets/public/app-version.json"
                self.assertEqual(
                    object_value(decode_json(version.read_bytes())), {"revision": revision}
                )
                before = fixture.snapshot()
                self.assertFalse(fixture.initialize(revision)[1]["changed"])
                self.assertEqual(fixture.snapshot(), before)

    def test_stable_bytes_with_incorrect_proxy_read_mode_are_repaired_and_reported(self) -> None:
        """Skipping equal bytes cannot suppress correction of a private file mode."""
        with owned_command() as fixture:
            self.assertEqual(fixture.initialize()[0], 0)
            version = fixture.root / "browser-assets/public/app-version.json"
            version.chmod(0o600)
            status, receipt = fixture.initialize()
            self.assertEqual(status, 0)
            self.assertEqual(receipt, {"passed": True, "changed": True})
            self.assertEqual(version.stat().st_mode & 0o777, 0o644)
            self.assertFalse(fixture.initialize()[1]["changed"])

    def test_foreign_state_and_invalid_revisions_fail_without_exposing_private_values(self) -> None:
        """No-op detection follows ownership/schema checks and cannot adopt extra files."""
        with owned_command() as fixture:
            self.assertEqual(fixture.initialize()[0], 0)
            private = fixture.root / "browser-assets/PRIVATE_FILE"
            _ = private.write_text("PRIVATE_CONTENT")
            status, receipt = fixture.initialize()
            self.assertEqual(status, 1)
            self.assertEqual(receipt, {"passed": False, "failure": "asset_store_foreign"})
            self.assertNotIn("PRIVATE", json.dumps(receipt))
            self.assertEqual(private.read_text(), "PRIVATE_CONTENT")
        with owned_command() as fixture:
            status, receipt = fixture.initialize("PRIVATE_REVISION")
            self.assertEqual(status, 1)
            self.assertEqual(receipt, {"passed": False, "failure": "asset_selected_revision"})
            self.assertNotIn("PRIVATE", json.dumps(receipt))

    def test_equal_version_bytes_never_bypass_symlink_or_writable_file_refusals(self) -> None:
        """An identical selected revision still requires a trusted ordinary public file."""
        with owned_command() as fixture:
            self.assertEqual(fixture.initialize()[0], 0)
            version = fixture.root / "browser-assets/public/app-version.json"
            content = version.read_bytes()
            outside = fixture.root / "outside-version.json"
            _ = outside.write_bytes(content)
            version.unlink()
            version.symlink_to(outside)
            status, receipt = fixture.initialize()
            self.assertEqual(status, 1)
            self.assertEqual(receipt, {"passed": False, "failure": "asset_store_ownership"})
            self.assertTrue(version.is_symlink())
            self.assertEqual(outside.read_bytes(), content)
            version.unlink()
            _ = version.write_bytes(content)
            version.chmod(0o666)
            status, receipt = fixture.initialize()
            self.assertEqual(status, 1)
            self.assertEqual(receipt, {"passed": False, "failure": "asset_store_ownership"})
            self.assertEqual(version.stat().st_mode & 0o777, 0o666)

    def test_an_existing_workload_keeps_exclusive_ownership_without_mutating_assets(self) -> None:
        """The preexisting busy receipt stays bounded and explicitly reports no change."""
        with owned_command() as fixture, public.workload_lock():
            status, receipt = fixture.initialize()
            self.assertEqual(status, 0)
            self.assertEqual(receipt, {"passed": True, "busy": True, "changed": False})
            self.assertFalse((fixture.root / "browser-assets").exists())


if __name__ == "__main__":
    _ = unittest.main()
