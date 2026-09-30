"""Exercise authenticated tool installation without downloading or running tools."""

from __future__ import annotations

import hashlib
import io
import json
import os
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from unittest.mock import patch

if TYPE_CHECKING:
    from collections.abc import Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_tools as tools

URL = "https://github.com/example/scanner/releases/download/v1.2.3/scanner.tar.gz"
EXECUTABLE = b"reviewed executable fixture bytes\n"


def bundle(entries: Sequence[tuple[str, bytes | str]]) -> bytes:
    """Build regular and symlink fixtures in memory, never on the host filesystem."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name, content in entries:
            entry = tarfile.TarInfo(name)
            if isinstance(content, str):
                entry.type = tarfile.SYMTYPE
                entry.linkname = content
                archive.addfile(entry)
            else:
                entry.size = len(content)
                archive.addfile(entry, io.BytesIO(content))
    return output.getvalue()


def asset(content: bytes) -> tools.Asset:
    """Bind a fixture archive to its digest and one expected regular executable."""
    return tools.Asset(URL, hashlib.sha256(content).hexdigest(), "tar.gz", {"scanner": "scanner"})


class ArchiveTests(unittest.TestCase):
    """Known-good bytes pass; unsafe or ambiguous authenticated archives fail."""

    def test_regular_executable_is_selected_without_extraction(self) -> None:
        """Ancillary regular files do not replace the named executable."""
        content = bundle([("LICENSE", b"fixture"), ("scanner", EXECUTABLE)])
        self.assertEqual(tools.selected_files(asset(content), content), {"scanner": EXECUTABLE})

    def test_digest_is_checked_before_tar_parsing(self) -> None:
        """A transport mutation fails authentication regardless of parseability."""
        content = bundle([("scanner", EXECUTABLE)])
        with self.assertRaisesRegex(tools.ToolError, "tool_archive_digest"):
            _ = tools.selected_files(asset(content), content + b"changed")

    def test_unsafe_members_and_duplicates_are_rejected(self) -> None:
        """No links, traversal, duplicates or absent executable can pass."""
        cases = [
            [("scanner", "target")],
            [("scanner", EXECUTABLE), ("scanner", EXECUTABLE)],
            [("../scanner", EXECUTABLE)],
            [("/scanner", EXECUTABLE)],
            [("elsewhere", EXECUTABLE)],
            [("scanner", EXECUTABLE), ("other", "scanner")],
        ]
        for entries in cases:
            with self.subTest(entries=entries):
                content = bundle(entries)
                with self.assertRaises(tools.ToolError):
                    _ = tools.selected_files(asset(content), content)

    def test_zip_selects_companion_libraries_and_rejects_missing_entries(self) -> None:
        """Native engines retain only the exact reviewed executable and libraries."""
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("wheel/bin/core", EXECUTABLE)
            archive.writestr("wheel/bin/libs/library.so", b"reviewed library")
            archive.writestr("wheel/python/unneeded.py", b"not installed")
        content = output.getvalue()
        entry = tools.Asset(
            URL,
            hashlib.sha256(content).hexdigest(),
            "zip",
            {
                "wheel/bin/core": "engine/bin/core",
                "wheel/bin/libs/library.so": "engine/bin/libs/library.so",
            },
        )
        self.assertEqual(
            tools.selected_files(entry, content),
            {
                "engine/bin/core": EXECUTABLE,
                "engine/bin/libs/library.so": b"reviewed library",
            },
        )
        entry.files["missing"] = "missing"
        with self.assertRaisesRegex(tools.ToolError, "tool_executable_missing"):
            _ = tools.selected_files(entry, content)

    def test_zip_expansion_is_bounded_before_member_reads(self) -> None:
        """Even unselected wheel members count toward the decompression budget."""
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("scanner", EXECUTABLE)
            archive.writestr("unselected", b"x" * 1024)
        content = output.getvalue()
        entry = tools.Asset(URL, hashlib.sha256(content).hexdigest(), "zip", {"scanner": "scanner"})
        with (
            patch.object(tools, "MAX_UNPACKED", 100),
            self.assertRaisesRegex(tools.ToolError, "tool_archive_expansion"),
        ):
            _ = tools.selected_files(entry, content)

    def test_expanded_archive_and_entry_counts_are_bounded(self) -> None:
        """Limits are checked before retaining excess output or processing entries."""
        content = bundle([("LICENSE", b"fixture"), ("scanner", EXECUTABLE)])
        with patch.object(tools, "MAX_UNPACKED", 100), self.assertRaises(tools.ToolError):
            _ = tools.selected_files(asset(content), content)
        with patch.object(tools, "MAX_MEMBERS", 1), self.assertRaises(tools.ToolError):
            _ = tools.selected_files(asset(content), content)


class InstallationTests(unittest.TestCase):
    """A complete install is reusable only with unchanged identities and permissions."""

    root: Path = Path()
    lock: Path = Path()
    directory: Path = Path()
    archive: bytes = b""

    @override
    def setUp(self) -> None:
        """Create an owned prefix and a lock for this platform only."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.directory = self.root / "installed"
        self.lock = self.root / "lock.json"
        self.archive = bundle([("scanner", EXECUTABLE)])
        _ = self.lock.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "tools": {
                        "scanner": {
                            "version": "1.2.3",
                            "executable": "scanner",
                            "platforms": {
                                tools.current_platform(): {
                                    "url": URL,
                                    "sha256": hashlib.sha256(self.archive).hexdigest(),
                                    "format": "tar.gz",
                                    "files": {"scanner": "scanner"},
                                },
                            },
                        },
                    },
                }
            ),
        )

    def install(self) -> Path:
        """Substitute transport bytes while retaining the real verification path."""
        with patch.object(tools, "download", return_value=self.archive):
            return tools.install(["scanner"], self.directory, lock_path=self.lock)

    def test_install_and_verified_reuse(self) -> None:
        """Every reuse checks the executable, receipt, lock and ownership."""
        self.assertEqual(self.install(), self.directory)
        path = tools.tool_path("scanner", self.directory, lock_path=self.lock)
        self.assertEqual(path.read_bytes(), EXECUTABLE)
        self.assertTrue(os.access(path, os.X_OK))
        with patch.object(
            tools, "download", side_effect=AssertionError("must reuse verified bytes")
        ):
            self.assertEqual(
                tools.install(["scanner"], self.directory, lock_path=self.lock), self.directory
            )

    def test_tampered_executable_fails_reuse(self) -> None:
        """An executable modified after installation never reaches a scanner call."""
        _ = self.install()
        _ = (self.directory / "bin/scanner").write_bytes(b"different executable")
        with self.assertRaisesRegex(tools.ToolError, "tool_executable_digest"):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)

    def test_companion_tampering_and_parent_symlinks_fail_reuse(self) -> None:
        """Verification includes every loaded library and its directory chain."""
        self.archive = bundle([("scanner", EXECUTABLE), ("library", b"native dependency")])
        lock = cast("dict[str, object]", json.loads(self.lock.read_text()))
        tool_set = cast("dict[str, dict[str, object]]", lock["tools"])
        platforms = cast("dict[str, dict[str, object]]", tool_set["scanner"]["platforms"])
        pin = platforms[tools.current_platform()]
        pin["sha256"] = hashlib.sha256(self.archive).hexdigest()
        cast("dict[str, str]", pin["files"])["library"] = "libs/library"
        _ = self.lock.write_text(json.dumps(lock))
        _ = self.install()
        companion = self.directory / "bin/libs/library"
        _ = companion.write_bytes(b"changed library")
        with self.assertRaisesRegex(tools.ToolError, "tool_executable_digest"):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)
        companion.unlink()
        companion.parent.rmdir()
        companion.parent.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(tools.ToolError, "unprotected_tool_directory"):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)

    def test_changed_lock_and_writable_binary_fail(self) -> None:
        """A receipt cannot silently authorize a different pin or shared writer."""
        _ = self.install()
        path = self.directory / "bin/scanner"
        path.chmod(0o722)
        with self.assertRaisesRegex(tools.ToolError, "unprotected_tool_file"):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)
        path.chmod(0o700)
        _ = self.lock.write_text(self.lock.read_text().replace("1.2.3", "1.2.4"))
        with self.assertRaisesRegex(tools.ToolError, "tool_receipt_lock"):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)

    def test_symlink_binary_and_receipt_are_rejected(self) -> None:
        """Installed final paths cannot redirect a verified lookup elsewhere."""
        _ = self.install()
        path = self.directory / "bin/scanner"
        path.unlink()
        path.symlink_to(self.lock)
        with self.assertRaises(OSError):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)
        path.unlink()
        _ = path.write_bytes(EXECUTABLE)
        path.chmod(0o700)
        receipt = self.directory / "receipt.json"
        receipt.unlink()
        receipt.symlink_to(self.lock)
        with self.assertRaises(OSError):
            _ = tools.tool_path("scanner", self.directory, lock_path=self.lock)

    def test_failed_install_is_not_published_and_removes_owned_staging(self) -> None:
        """A failed download leaves no accepted directory or partial installation."""
        with (
            patch.object(tools, "download", side_effect=OSError("fixture failure")),
            self.assertRaises(OSError),
        ):
            _ = tools.install(["scanner"], self.directory, lock_path=self.lock)
        self.assertFalse(self.directory.exists())
        self.assertEqual(list(self.root.glob(".security-tools-*")), [])

    def test_existing_unrelated_directory_is_never_overwritten(self) -> None:
        """Installation refuses to overlay a prefix without its verified receipt."""
        self.directory.mkdir()
        marker = self.directory / "keep"
        _ = marker.write_text("owner data")
        with self.assertRaises(OSError):
            _ = self.install()
        self.assertEqual(marker.read_text(), "owner data")


if __name__ == "__main__":
    _ = unittest.main()
