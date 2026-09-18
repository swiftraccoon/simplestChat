"""Offline prepared-host checks using disposable files, never installed helpers."""

import hashlib
import io
import json
import os
import platform
import stat
import sys
import tempfile
import unittest
from contextlib import ExitStack, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import override
from unittest.mock import patch

from test_support import ROOT, obj

# isort: split
import release_preflight as PREFLIGHT  # noqa: N812 - retain the helper-under-test naming.
from release_json import decode_json

SOURCE = ROOT / "ops/ansible/files/release_preflight.py"
ORIGINAL_LSTAT = Path.lstat


@dataclass(frozen=True, slots=True, kw_only=True)
class Metadata:
    """Only the filesystem metadata fields inspected by the preflight."""

    st_uid: int
    st_gid: int
    st_mode: int
    st_size: int


type Snapshot = dict[str, tuple[int, int, int, int, int, int, int, bytes | None]]


class ReleasePreflightTests(unittest.TestCase):
    """Verify the release preflight contract offline."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest signature.
        """Initialize harmless fixture slots; actual resources are created by setUp."""
        super().__init__(methodName)
        self.directory: Path = Path()
        self.config: Path = Path()
        self.root: Path = Path()
        self.helpers: Path = Path()
        self.selection: Path = Path()
        self.marker: Path = Path()
        self.contents: bytes = b""
        self.expected: dict[str, str] = {}
        self.metadata_overrides: dict[Path, dict[str, int]] = {}
        self.external_ancestors: set[Path] = set()

    @override
    def setUp(self) -> None:
        """Prepare the isolated fixture and register cleanup."""
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-preflight.")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        self.config = self.directory / "etc/simplestchat-public"
        self.root = self.directory / "srv/simplestchat-public"
        self.helpers = self.directory / "usr/local/libexec/simplestchat-public"
        for path in (self.config, self.root, self.root / "releases", self.helpers):
            path.mkdir(parents=True, exist_ok=True)
            path.chmod(0o755 if path == self.helpers else 0o700)
        for path in self.helpers.parents:
            if path != self.directory and self.directory in path.parents:
                path.chmod(0o755)
        self.selection = self.config / "images.json"
        _ = self.selection.write_bytes(b"PRIVATE_CONFIGURATION_CONTENT\n")
        self.selection.chmod(0o600)
        self.marker = self.directory / "helper-was-executed"
        self.contents = (
            f"from pathlib import Path\nPath({str(self.marker)!r}).touch()\n"
            "# PRIVATE_INSTALLED_HELPER_CONTENT\n\n"
        ).encode()
        self.expected = {}
        for name in sorted(PREFLIGHT.BASE_HELPERS | PREFLIGHT.FETCH_HELPERS):
            path = self.helpers / name
            _ = path.write_bytes(self.contents)
            path.chmod(0o644)
            self.expected[name] = hashlib.sha256(self.contents).hexdigest()

        self.metadata_overrides = {}
        # Only fixture ownership and its synthetic /usr ancestors are modeled.
        # Actual fixture permissions, types, sizes, and bytes remain under test.
        self.external_ancestors = set(self.directory.parents) | {self.directory}
        stack = ExitStack()
        self.addCleanup(stack.close)
        for name, value in (
            ("CONFIG", self.config),
            ("ROOT", self.root),
            ("HELPERS", self.helpers),
        ):
            _ = stack.enter_context(patch.object(PREFLIGHT, name, value))
        _ = stack.enter_context(patch.object(os, "geteuid", return_value=0))
        _ = stack.enter_context(patch.object(platform, "system", return_value="Linux"))
        _ = stack.enter_context(patch.object(platform, "machine", return_value="x86_64"))
        _ = stack.enter_context(
            patch.object(
                platform,
                "freedesktop_os_release",
                return_value={"ID": "debian", "VERSION_ID": "13"},
            )
        )

        def fixture_lstat(path: Path) -> Metadata:
            return self.fixture_lstat(path)

        _ = stack.enter_context(patch.object(Path, "lstat", fixture_lstat))

    def fixture_lstat(self, path: Path) -> Metadata:
        """Model fixture ownership while retaining real file kinds, permissions and sizes."""
        metadata = ORIGINAL_LSTAT(path)
        values = {
            "st_uid": metadata.st_uid,
            "st_gid": metadata.st_gid,
            "st_mode": metadata.st_mode,
            "st_size": metadata.st_size,
        }
        if path in self.external_ancestors:
            values.update(st_uid=0, st_gid=0, st_mode=stat.S_IFDIR | 0o755)
        elif self.directory in path.parents:
            values.update(st_uid=0, st_gid=0)
        values.update(self.metadata_overrides.get(path, {}))
        return Metadata(
            st_uid=values["st_uid"],
            st_gid=values["st_gid"],
            st_mode=values["st_mode"],
            st_size=values["st_size"],
        )

    def snapshot(self) -> Snapshot:
        """Capture file contents and metadata to prove the preflight is read-only."""
        result: Snapshot = {}
        for path in (self.directory, *sorted(self.directory.rglob("*"))):
            value = ORIGINAL_LSTAT(path)
            result[str(path.relative_to(self.directory))] = (
                value.st_mode,
                value.st_uid,
                value.st_gid,
                value.st_ino,
                value.st_size,
                value.st_mtime_ns,
                value.st_ctime_ns,
                path.read_bytes() if stat.S_ISREG(value.st_mode) else None,
            )
        return result

    def run_main(self, arguments: list[str]) -> tuple[int, str]:
        """Exercise CLI parsing with captured public output and restored arguments."""
        output = io.StringIO()
        with (
            patch.object(sys, "argv", ["release_preflight.py", *arguments]),
            redirect_stdout(output),
        ):
            code = PREFLIGHT.main()
        return code, output.getvalue()

    def test_complete_base_and_github_sets_accept_exact_raw_source_bytes(self) -> None:
        """Verify complete base and github sets accept exact raw source bytes."""
        for expected in (
            {name: self.expected[name] for name in PREFLIGHT.BASE_HELPERS},
            self.expected,
        ):
            with self.subTest(names=sorted(expected)):
                result = PREFLIGHT.check(expected)
                self.assertEqual(set(result), {"schemaVersion", "runId", "prepared"})
                self.assertEqual(result["schemaVersion"], 1)
                self.assertIs(result["prepared"], expr2=True)
                self.assertRegex(result["runId"], r"^[a-f0-9]{32}$")
        trimmed = dict(
            self.expected,
            **{"release-public.py": hashlib.sha256(self.contents.rstrip()).hexdigest()},
        )
        with self.assertRaisesRegex(PREFLIGHT.PreflightError, "^helper_digest_mismatch$"):
            _ = PREFLIGHT.check(trimmed)

    def test_success_is_read_only_does_not_execute_helpers_and_has_fresh_run_ids(self) -> None:
        """Verify success is read only does not execute helpers and has fresh run ids."""
        before = self.snapshot()
        first = PREFLIGHT.check(self.expected)
        second = PREFLIGHT.check(self.expected)
        self.assertNotEqual(first["runId"], second["runId"])
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(self.marker.exists())
        self.assertFalse(any(path.name == "__pycache__" for path in self.directory.rglob("*")))

    def test_normal_mode_supports_initial_helper_and_storage_reconciliation(self) -> None:
        """Verify normal mode supports initial helper and storage reconciliation."""
        for path in self.helpers.iterdir():
            path.unlink()
        self.helpers.rmdir()
        (self.root / "releases").rmdir()
        before = self.snapshot()
        result = PREFLIGHT.check({})
        self.assertIs(result["prepared"], expr2=False)
        self.assertEqual(self.snapshot(), before)

    def test_helper_set_must_be_exact_and_digest_shape_is_strict(self) -> None:
        """Verify helper set must be exact and digest shape is strict."""
        invalid_sets: tuple[object, ...] = (
            None,
            [],
            True,
            {"release-public.py": "a" * 64},
            {**self.expected, "../extra.py": "a" * 64},
            {name: value for name, value in self.expected.items() if name != "reboot-public.py"},
        )
        for expected in invalid_sets:
            with (
                self.subTest(expected=expected),
                self.assertRaisesRegex(PREFLIGHT.PreflightError, "^invalid_helper_set$"),
            ):
                _ = PREFLIGHT.check(expected)
        for value in (None, True, "", "a" * 63, "A" * 64, "g" * 64, "a" * 64 + "\n"):
            with (
                self.subTest(value=value),
                self.assertRaisesRegex(PREFLIGHT.PreflightError, "^invalid_helper_digest$"),
            ):
                _ = PREFLIGHT.check(dict(self.expected, **{"release-public.py": value}))

    def test_missing_required_paths_are_rejected(self) -> None:
        """Verify missing required paths are rejected."""
        for path in (
            self.helpers / "release-public.py",
            self.root / "releases",
            self.helpers,
            self.selection,
            self.config,
            self.root,
        ):
            retained = path.with_name(path.name + ".retained")
            _ = path.rename(retained)
            try:
                with self.subTest(path=path), self.assertRaises(FileNotFoundError):
                    _ = PREFLIGHT.check(self.expected)
            finally:
                _ = retained.rename(path)

    def test_same_size_changed_helper_is_rejected(self) -> None:
        """Verify same size changed helper is rejected."""
        path = self.helpers / "release-public.py"
        _ = path.write_bytes(self.contents.replace(b"PRIVATE", b"CHANGED", 1))
        self.assertEqual(path.stat().st_size, len(self.contents))
        with self.assertRaisesRegex(PREFLIGHT.PreflightError, "^helper_digest_mismatch$"):
            _ = PREFLIGHT.check(self.expected)

    def test_wrong_directory_file_and_selection_modes_are_rejected(self) -> None:
        """Verify wrong directory file and selection modes are rejected."""
        paths = (
            self.config,
            self.root,
            self.root / "releases",
            self.helpers,
            self.helpers.parent,
            self.helpers / "release-public.py",
            self.selection,
        )
        for path in paths:
            previous = stat.S_IMODE(ORIGINAL_LSTAT(path).st_mode)
            path.chmod(previous | 0o002)
            try:
                with (
                    self.subTest(path=path),
                    self.assertRaisesRegex(PREFLIGHT.PreflightError, "^unsafe_release_path$"),
                ):
                    _ = PREFLIGHT.check(self.expected)
            finally:
                path.chmod(previous)

    def test_nonroot_owner_or_group_is_rejected(self) -> None:
        """Verify nonroot owner or group is rejected."""
        for path in (
            self.config,
            self.root,
            self.helpers,
            self.helpers.parent,
            self.helpers / "release-public.py",
            self.selection,
        ):
            for field in ("st_uid", "st_gid"):
                self.metadata_overrides[path] = {field: 501}
                try:
                    with (
                        self.subTest(path=path, field=field),
                        self.assertRaisesRegex(PREFLIGHT.PreflightError, "^unsafe_release_path$"),
                    ):
                        _ = PREFLIGHT.check(self.expected)
                finally:
                    self.metadata_overrides.clear()

    def test_symlink_files_and_intermediate_helper_directory_are_rejected(self) -> None:
        """Verify symlink files and intermediate helper directory are rejected."""
        for path in (self.helpers / "release-public.py", self.selection, self.helpers.parent):
            retained = path.with_name(path.name + ".retained")
            _ = path.rename(retained)
            path.symlink_to(retained, target_is_directory=retained.is_dir())
            try:
                with (
                    self.subTest(path=path),
                    self.assertRaisesRegex(PREFLIGHT.PreflightError, "^unsafe_release_path$"),
                ):
                    _ = PREFLIGHT.check(self.expected)
            finally:
                path.unlink()
                _ = retained.rename(path)

    def test_directories_cannot_substitute_for_helper_or_selection_files(self) -> None:
        """Verify directories cannot substitute for helper or selection files."""
        for path in (self.helpers / "release-public.py", self.selection):
            contents = path.read_bytes()
            mode = stat.S_IMODE(ORIGINAL_LSTAT(path).st_mode)
            path.unlink()
            path.mkdir(mode=mode)
            try:
                with (
                    self.subTest(path=path),
                    self.assertRaisesRegex(PREFLIGHT.PreflightError, "^unsafe_release_path$"),
                ):
                    _ = PREFLIGHT.check(self.expected)
            finally:
                path.rmdir()
                _ = path.write_bytes(contents)
                path.chmod(mode)

    def test_empty_or_oversized_helper_and_selection_are_rejected(self) -> None:
        """Verify empty or oversized helper and selection are rejected."""
        for path, limit in (
            (self.helpers / "release-public.py", PREFLIGHT.MAX_HELPER),
            (self.selection, 65536),
        ):
            contents = path.read_bytes()
            for size in (0, limit + 1):
                _ = path.write_bytes(b"x" * size)
                try:
                    with (
                        self.subTest(path=path, size=size),
                        self.assertRaisesRegex(
                            PREFLIGHT.PreflightError, "^invalid_release_file_size$"
                        ),
                    ):
                        _ = PREFLIGHT.check(self.expected)
                finally:
                    _ = path.write_bytes(contents)

    def test_root_linux_architecture_and_debian_major_are_required(self) -> None:
        """Verify root linux architecture and debian major are required."""
        cases = (
            (os, "geteuid", 501, "root_required"),
            (platform, "system", "Darwin", "unsupported_platform"),
            (platform, "machine", "aarch64", "unsupported_platform"),
            (
                platform,
                "freedesktop_os_release",
                {"ID": "ubuntu", "VERSION_ID": "13"},
                "unsupported_distribution",
            ),
            (
                platform,
                "freedesktop_os_release",
                {"ID": "debian", "VERSION_ID": "12"},
                "unsupported_distribution",
            ),
            (
                platform,
                "freedesktop_os_release",
                {"ID": "debian"},
                "unsupported_distribution",
            ),
        )
        for target, name, value, failure in cases:
            with (
                self.subTest(name=name, value=value),
                patch.object(target, name, return_value=value),
                self.assertRaisesRegex(PREFLIGHT.PreflightError, "^" + failure + "$"),
            ):
                _ = PREFLIGHT.check(self.expected)
        with patch.object(
            platform,
            "freedesktop_os_release",
            return_value={"ID": "debian", "VERSION_ID": "13.1"},
        ):
            self.assertTrue(PREFLIGHT.check(self.expected)["prepared"])

    def test_main_success_returns_only_public_receipt_without_file_contents(self) -> None:
        """Verify main success returns only public receipt without file contents."""
        before = self.snapshot()
        code, output = self.run_main([json.dumps(self.expected)])
        self.assertEqual(code, 0)
        value = obj(decode_json(output))
        self.assertEqual(set(value), {"schemaVersion", "runId", "prepared"})
        self.assertTrue(value["prepared"])
        self.assertNotIn("PRIVATE", output)
        self.assertNotIn(str(self.directory), output)
        self.assertEqual(self.snapshot(), before)

    def test_main_invalid_requests_and_filesystem_failures_are_coarse(self) -> None:
        """Verify main invalid requests and filesystem failures are coarse."""
        for arguments, failure in (
            ([], "invalid_request"),
            (["{}", "{}"], "invalid_request"),
            (["x" * 4097], "invalid_request"),
            (["PRIVATE_INVALID_JSON"], "preflight_unavailable"),
            (["[]"], "invalid_helper_set"),
        ):
            with self.subTest(arguments=arguments[:1]):
                code, output = self.run_main(arguments)
                self.assertEqual(code, 1)
                self.assertEqual(
                    obj(decode_json(output)),
                    {"schemaVersion": 1, "passed": False, "failure": failure},
                )
                self.assertNotIn("PRIVATE", output)
        with patch.object(PREFLIGHT, "protected", side_effect=OSError("PRIVATE_FILESYSTEM_DETAIL")):
            code, output = self.run_main(["{}"])
        self.assertEqual(code, 1)
        self.assertEqual(obj(decode_json(output))["failure"], "preflight_unavailable")
        self.assertNotIn("PRIVATE", output)


if __name__ == "__main__":
    _ = unittest.main()
