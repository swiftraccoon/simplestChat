"""Offline provenance fixtures exercise actual archives, bytes and retained evidence."""

from __future__ import annotations

import configparser
import io
import os
import stat
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import Mock, patch

from test_support import ROOT

# isort: split
import bounded_process
import security_vendor as vendor
from release_json import JsonObject, array_value, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence


def tar_bytes(files: Mapping[str, bytes]) -> bytes:
    """Produce a real compressed archive without invoking an external archiver."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for path, data in files.items():
            member = tarfile.TarInfo(path)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


def zip_bytes(files: Sequence[tuple[str, bytes]]) -> bytes:
    """Keep deliberate duplicate-entry fixtures possible."""
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for path, data in files:
            archive.writestr(path, data)
    return output.getvalue()


def source_record(identifier: str, body: bytes, *, format_name: str = "tar.gz") -> JsonObject:
    """Pin one fixture to its real byte digest."""
    return {
        "id": identifier,
        "url": "https://example.invalid/" + identifier,
        "sha256": vendor.sha256(body),
        "format": format_name,
    }


def change(path: str, before: bytes | None, after: bytes | None) -> JsonObject:
    """Represent absence distinctly from a present empty file."""
    return {
        "path": path,
        "upstream_sha256": None if before is None else vendor.sha256(before),
        "vendored_sha256": None if after is None else vendor.sha256(after),
    }


class VendorTests(unittest.TestCase):
    """Full verification must account for every byte without network or extraction."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Initialize typed fixture attributes before unittest invokes setUp."""
        super().__init__(methodName)
        self.root: Path = Path()
        self.cache: Path = Path()
        self.wrap: bytes = b""
        self.manifest: JsonObject = {}

    @override
    def setUp(self) -> None:
        """Create a private fixture with modified, added, deleted and unchanged files."""
        temporary = tempfile.TemporaryDirectory(prefix="vendor-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.cache = self.root / "cache"
        self.cache.mkdir(mode=0o700)
        native = tar_bytes({"native/source.c": b"int native;\n"})
        native_hash = vendor.sha256(native)
        self.wrap = (
            "[wrap-file]\ndirectory = native\n"
            + "source_url = https://example.invalid/native\n"
            + "source_filename = native.tar.gz\n"
            + f"source_hash = {native_hash}\n"
        ).encode()
        original = {
            "pkg/.hidden": b"hidden original\n",
            "pkg/same": b"unchanged\n",
            "pkg/changed": b"old\n",
            "pkg/deleted": b"remove\n",
            "pkg/subprojects/native.wrap": self.wrap,
        }
        crate = tar_bytes(original)
        for body in (crate, native, b"license\n"):
            vendor.write_private(self.cache / vendor.sha256(body), body)
        self.manifest = {
            "sources": [
                source_record("crate", crate),
                source_record("native", native),
                source_record("license", b"license\n", format_name="file"),
            ],
            "trees": [
                {
                    "path": "vendor/pkg",
                    "source": "crate",
                    "prefix": "pkg",
                    "changes": [
                        change("changed", b"old\n", b"new\n"),
                        change("added", None, b"added\n"),
                        change("deleted", b"remove\n", None),
                    ],
                }
            ],
            "files": [{"path": "vendor/corpus/LICENSE", "source": "license"}],
            "maintained_files": ["vendor/README.md"],
            "wraps": [
                {
                    "path": "vendor/pkg/subprojects/native.wrap",
                    "source": "native",
                    "patch_source": None,
                    "patch_directory": None,
                    "fallback_urls": [],
                }
            ],
        }
        for path, body in original.items():
            if path != "pkg/deleted":
                self.put("vendor/" + path, b"new\n" if path == "pkg/changed" else body)
        self.put("vendor/pkg/added", b"added\n")
        self.put("vendor/corpus/LICENSE", b"license\n")
        self.put("vendor/README.md", b"Reviewed vendor fixture.\n")
        self.save_manifest()

    def put(self, path: str, body: bytes) -> None:
        """Write only inside the disposable test root."""
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        _ = target.write_bytes(body)

    def save_manifest(self) -> None:
        """Materialize the deliberately changed review contract."""
        self.put("vendor/integrity.json", vendor.json_bytes(self.manifest))

    def check(self, name: str = "evidence") -> Path:
        """Run the actual verifier against hash-addressed offline sources."""
        output = self.root / name
        vendor.verify(self.root, self.cache, output, offline=True)
        return output

    def test_complete_comparison_and_diff_are_deterministic_and_private(self) -> None:
        """Identical imports retain complete per-file identities and all three deviation kinds."""
        with patch.object(bounded_process, "run") as run:
            first, second = self.check("one"), self.check("two")
        run.assert_not_called()
        self.assertEqual(
            (first / "report.json").read_bytes(), (second / "report.json").read_bytes()
        )
        report = object_value(decode_json((first / "report.json").read_bytes()))
        files = array_value(object_value(array_value(report["trees"])[0])["files"])
        self.assertEqual(len(files), 6)
        diff = (first / "vendor.diff").read_text()
        for expected in ("-old\n+new\n", "new file", "deleted file", "-remove\n"):
            self.assertIn(expected, diff)
        self.assertEqual(report["diff_sha256"], vendor.sha256(diff.encode()))
        self.assertEqual(stat.S_IMODE(first.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((first / "report.json").stat().st_mode), 0o600)

    def test_unlisted_change_addition_and_deletion_fail_and_retain_full_diff(self) -> None:
        """A changed hidden file, extra file or deletion cannot hide behind patch allowances."""
        self.put("vendor/pkg/.hidden", b"unexpected\n")
        self.put("vendor/pkg/unlisted", b"new source\n")
        (self.root / "vendor/pkg/same").unlink()
        with self.assertRaisesRegex(vendor.IntegrityError, "Unlisted or stale deviation"):
            _ = self.check()
        diff = (self.root / "evidence/vendor.diff").read_text()
        for name in (".hidden", "unlisted", "same"):
            self.assertIn(name, diff)
        self.assertFalse((self.root / "evidence/report.json").exists())
        self.assertTrue((self.root / "evidence/failure.json").is_file())

    def test_stale_allowance_and_changed_patch_bytes_fail(self) -> None:
        """An upstreamed patch or later unreviewed edit requires a manifest review."""
        self.put("vendor/pkg/changed", b"old\n")
        self.put("vendor/pkg/added", b"different patch\n")
        with self.assertRaisesRegex(vendor.IntegrityError, "Unlisted or stale deviation"):
            _ = self.check()

    def test_extra_vendor_root_and_missing_maintained_file_fail_coverage(self) -> None:
        """Whole-tree enumeration detects files outside existing dependency directories."""
        self.put("vendor/new-package/source.c", b"unreviewed\n")
        (self.root / "vendor/README.md").unlink()
        with self.assertRaisesRegex(vendor.IntegrityError, "Unlisted or missing vendor files"):
            _ = self.check()

    def test_copied_raw_upstream_file_is_checked(self) -> None:
        """Data and licenses retain the same authenticated-byte requirement as code."""
        self.put("vendor/corpus/LICENSE", b"unreviewed license\n")
        with self.assertRaisesRegex(vendor.IntegrityError, "Copied upstream file differs"):
            _ = self.check()

    def test_manifest_refuses_duplicate_unknown_and_stale_records(self) -> None:
        """Schema ambiguity and unused sources fail before source download."""
        data = vendor.json_bytes(self.manifest)
        with self.assertRaisesRegex(ValueError, "Duplicate JSON key"):
            _ = vendor.parse_manifest(b'{"sources":[],"sources":[]}')
        for field in ("unexpected", "sources"):
            with self.subTest(field=field):
                altered = object_value(decode_json(data))
                if field == "unexpected":
                    altered[field] = True
                else:
                    array_value(altered[field]).append(source_record("unused", b"unused"))
                with self.assertRaises(vendor.IntegrityError):
                    _ = vendor.parse_manifest(vendor.json_bytes(altered))
        changes = array_value(object_value(array_value(self.manifest["trees"])[0])["changes"])
        changes.append(changes[0])
        with self.assertRaisesRegex(vendor.IntegrityError, "Duplicate change path"):
            _ = vendor.parse_manifest(vendor.json_bytes(self.manifest))

    def test_native_wrap_set_is_exhaustive(self) -> None:
        """A new or removed native dependency cannot be introduced by editing only its wrap."""
        self.put("vendor/pkg/subprojects/extra.wrap", self.wrap)
        with self.assertRaisesRegex(vendor.IntegrityError, "Unlisted or missing native wraps"):
            _ = self.check()

    def test_native_metadata_exception_is_exact_and_structurally_checked(self) -> None:
        """One reviewed metadata filename cannot exempt arbitrary JSON or unknown schema fields."""
        maintained = array_value(self.manifest["maintained_files"])
        maintained.append("vendor/native-components.json")
        data = (ROOT / "vendor/native-components.json").read_bytes()
        self.put("vendor/native-components.json", data)
        self.save_manifest()
        _ = self.check("metadata-valid")
        malformed = object_value(decode_json(data))
        malformed["unknown"] = True
        self.put("vendor/native-components.json", vendor.json_bytes(malformed))
        with self.assertRaisesRegex(vendor.IntegrityError, "Unexpected manifest fields"):
            _ = self.check("metadata-invalid")
        maintained.append("vendor/unreviewed.json")
        with self.assertRaisesRegex(vendor.IntegrityError, "Only declared README"):
            _ = vendor.parse_manifest(vendor.json_bytes(self.manifest))

    def test_vcs_unhashed_and_unknown_native_fetches_fail(self) -> None:
        """The active Meson contract accepts only manifest-matching archive pins."""
        for name, contents in (
            ("vcs", b"[wrap-git]\nurl = https://example.invalid/source.git\nrevision = main\n"),
            ("missing_hash", self.wrap.replace(b"source_hash", b"unlisted_hash")),
            ("new_url", self.wrap.replace(b"example.invalid/native", b"example.invalid/other")),
            ("fallback", self.wrap + b"source_fallback_url = https://example.invalid/fallback\n"),
            ("patch", self.wrap + b"patch_url = https://example.invalid/patch.zip\n"),
            ("diff", self.wrap + b"diff_files = unlisted.patch\n"),
        ):
            with self.subTest(name=name):
                self.put("vendor/pkg/subprojects/native.wrap", contents)
                with self.assertRaises(vendor.IntegrityError):
                    _ = self.check(name)

    def test_local_overlay_must_exist_and_remote_patch_must_be_complete(self) -> None:
        """Neither an absent local overlay nor a URL without its recorded hash is accepted."""
        manifest = vendor.parse_manifest(vendor.json_bytes(self.manifest))
        wrap = manifest.wraps[0]
        local = {wrap.path: self.wrap}
        parser = configparser.ConfigParser()
        parser.read_string(self.wrap.decode())
        parser["wrap-file"]["patch_directory"] = "absent"
        modified = vendor.Wrap(wrap.path, wrap.source, None, "absent", ())
        with self.assertRaisesRegex(vendor.IntegrityError, "Missing local native overlay"):
            vendor.validate_wrap_patch(modified, manifest, parser["wrap-file"], local)
        modified = vendor.Wrap(wrap.path, wrap.source, "native", None, ())
        with self.assertRaisesRegex(vendor.IntegrityError, "Native patch pin differs"):
            vendor.validate_wrap_patch(modified, manifest, parser["wrap-file"], local)

    def test_offline_missing_or_corrupt_cache_never_downloads(self) -> None:
        """Offline is a hard network boundary; corrupt content is never silently replaced."""
        source = vendor.Source(
            "missing", "https://example.invalid/source", vendor.sha256(b"good"), "file"
        )
        with patch.object(bounded_process, "run") as run:
            with self.assertRaisesRegex(vendor.IntegrityError, "Offline source missing"):
                _ = vendor.source_bytes(source, self.cache, offline=True)
            vendor.write_private(self.cache / source.sha256, b"corrupt")
            with self.assertRaisesRegex(vendor.IntegrityError, "Source SHA-256 differs"):
                _ = vendor.source_bytes(source, self.cache, offline=False)
        run.assert_not_called()

    def test_download_child_has_deadline_and_wrong_bytes_are_not_published(self) -> None:
        """A successful transport is insufficient without the reviewed archive digest."""
        source = vendor.Source(
            "remote", "https://example.invalid/source", vendor.sha256(b"good"), "file"
        )

        def fetched(
            argv: Sequence[str], *, limits: bounded_process.Limits
        ) -> tuple[int, bytes, bytes]:
            self.assertEqual(limits, bounded_process.Limits(timeout=120, stdout=4096, stderr=4096))
            self.assertIn("_download", argv)
            vendor.write_private(Path(argv[-1]), b"wrong")
            return 0, b"", b""

        with (
            patch.object(bounded_process, "run", side_effect=fetched),
            self.assertRaisesRegex(vendor.IntegrityError, "Source SHA-256 differs"),
        ):
            _ = vendor.source_bytes(source, self.cache, offline=False)
        self.assertFalse((self.cache / source.sha256).exists())
        self.assertFalse(any(path.name.startswith("download-") for path in self.cache.iterdir()))

    def test_symlinks_special_files_and_public_evidence_directories_are_rejected(self) -> None:
        """Local paths cannot replace source bytes with a link or hang on a FIFO."""
        for name in ("link", "fifo"):
            with self.subTest(name=name):
                target = self.root / "vendor/pkg" / name
                if name == "link":
                    target.symlink_to(self.cache)
                else:
                    os.mkfifo(target)
                with self.assertRaises((vendor.IntegrityError, OSError)):
                    _ = self.check(name)
                target.unlink()
        self.cache.chmod(0o755)
        with self.assertRaisesRegex(vendor.IntegrityError, "owned and private"):
            _ = self.check("public")

    def test_existing_output_is_never_overwritten(self) -> None:
        """A previous receipt cannot be mistaken for a new successful verification."""
        output = self.check()
        before = (output / "report.json").read_bytes()
        with self.assertRaises(FileExistsError):
            _ = self.check()
        self.assertEqual(before, (output / "report.json").read_bytes())

    def test_repository_manifest_covers_actual_wraps_and_vendor_roots(self) -> None:
        """The real reviewed manifest is complete without requiring network in unit tests."""
        local = {
            "vendor/" + path: data for path, data in vendor.tree_files(ROOT / "vendor").items()
        }
        manifest = vendor.parse_manifest(local["vendor/integrity.json"])
        vendor.validate_coverage(manifest, local)
        self.assertIn("mediasoup-0.27.0", manifest.sources)
        self.assertIn("mediasoup-sys-0.17.0", manifest.sources)


class ArchiveTests(unittest.TestCase):
    """Authenticated archives still require safe, bounded, unambiguous member parsing."""

    def test_digest_is_verified_before_archive_parser(self) -> None:
        """Authentication precedes archive metadata, decompression and member reads."""
        source = vendor.Source("crate", "https://example.invalid/crate", "0" * 64, "tar.gz")
        with (
            patch.object(vendor, "tar_members") as parse,
            self.assertRaisesRegex(vendor.IntegrityError, "SHA-256"),
        ):
            _ = vendor.archive_files(source, b"unauthenticated")
        parse.assert_not_called()

    def test_tar_and_zip_unsafe_member_paths_are_rejected(self) -> None:
        """Traversal, absolute paths and platform-ambiguous names never reach the filesystem."""
        for name in (
            "../outside",
            "/absolute",
            "pkg/../escape",
            "pkg//file",
            "pkg/./file",
            "C:/file",
            "pkg\\file",
            "pkg/new\nline",
        ):
            for archive_format in ("tar.gz", "zip"):
                with self.subTest(name=name, archive_format=archive_format):
                    body = (
                        tar_bytes({name: b"x"})
                        if archive_format == "tar.gz"
                        else zip_bytes([(name, b"x")])
                    )
                    source = vendor.Source(
                        "fixture",
                        "https://example.invalid/archive",
                        vendor.sha256(body),
                        archive_format,
                    )
                    with self.assertRaises(vendor.IntegrityError):
                        _ = vendor.archive_files(source, body)

    def test_tar_links_devices_and_duplicates_are_rejected(self) -> None:
        """No member may alias another member or introduce a special filesystem object."""
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind):
                output = io.BytesIO()
                with tarfile.open(fileobj=output, mode="w:gz") as archive:
                    member = tarfile.TarInfo("pkg/link")
                    member.type = kind
                    member.linkname = "other"
                    archive.addfile(member)
                with self.assertRaisesRegex(vendor.IntegrityError, "Unsupported tar member type"):
                    _ = vendor.tar_members(output.getvalue())
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for _ in range(2):
                archive.addfile(tarfile.TarInfo("pkg/repeated"), io.BytesIO())
        with self.assertRaisesRegex(vendor.IntegrityError, "Duplicate"):
            _ = vendor.tar_members(output.getvalue())

    def test_zip_links_and_duplicate_members_are_rejected(self) -> None:
        """Zip unix modes and duplicate central-directory records are checked."""
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            member = zipfile.ZipInfo("pkg/link")
            member.create_system = 3
            member.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(member, "target")
        with self.assertRaisesRegex(vendor.IntegrityError, "Unsupported zip member type"):
            _ = vendor.zip_members(output.getvalue())
        with self.assertWarns(UserWarning):
            duplicate = zip_bytes([("pkg/a", b"1"), ("pkg/a", b"2")])
        with self.assertRaisesRegex(vendor.IntegrityError, "Duplicate"):
            _ = vendor.zip_members(duplicate)
        ambiguous = zip_bytes([("pkg/fileX", b"x")]).replace(b"pkg/fileX", b"pkg/file\x00")
        with self.assertRaisesRegex(vendor.IntegrityError, "Ambiguous zip member name"):
            _ = vendor.zip_members(ambiguous)

    def test_compressed_metadata_is_bounded_before_tar_parser_runs(self) -> None:
        """An oversized expanded stream cannot allocate PAX metadata inside tarfile."""
        body = tar_bytes({"pkg/file": b"small"})
        with (
            patch.object(vendor, "MAX_CONTENT", 1024),
            patch.object(tarfile, "open") as parse,
            self.assertRaisesRegex(vendor.IntegrityError, "expansion budget"),
        ):
            _ = vendor.tar_members(body)
        parse.assert_not_called()

    def test_member_count_expansion_and_file_directory_collisions_are_bounded(self) -> None:
        """Budgets are enforced before retaining large bodies or ambiguous trees."""
        for body, patch_name, limit in (
            (tar_bytes({"pkg/a": b"1234"}), "MAX_MEMBER", 3),
            (tar_bytes({"pkg/a": b"12", "pkg/b": b"34"}), "MAX_CONTENT", 3),
            (tar_bytes({"pkg/a": b"", "pkg/b": b""}), "MAX_FILES", 1),
        ):
            with (
                self.subTest(patch_name=patch_name),
                patch.object(vendor, patch_name, limit),
                self.assertRaises(vendor.IntegrityError),
            ):
                _ = vendor.tar_members(body)
        for files in ({"pkg/a": b"x", "pkg/a/b": b"y"}, {"pkg/a/b": b"y", "pkg/a": b"x"}):
            with (
                self.subTest(files=files),
                self.assertRaisesRegex(vendor.IntegrityError, "collision"),
            ):
                _ = vendor.tar_members(tar_bytes(files))

    def test_archive_prefix_and_allowance_identities_are_exact(self) -> None:
        """Unused paths and old/new hashes cannot authorize an arbitrary difference."""
        tree = vendor.Tree("vendor/pkg", "crate", "pkg", ())
        with self.assertRaisesRegex(vendor.IntegrityError, "outside its declared prefix"):
            _ = vendor.compare_tree(tree, {"elsewhere/file": b"x"}, {})
        for before, after in ((None, None), ("0" * 64, "0" * 64)):
            with self.assertRaisesRegex(vendor.IntegrityError, "Stale allowance"):
                _ = vendor.parse_change(
                    {"path": "file", "upstream_sha256": before, "vendored_sha256": after}
                )

    def test_binary_and_empty_file_diffs_retain_complete_identity(self) -> None:
        """Binary content is represented completely, and empty additions remain visible."""
        binary = vendor.render_diff("vendor/pkg/file", b"\x00old", b"\xffnew")
        self.assertIn(b"base64=AG9sZA==", binary)
        self.assertIn(b"base64=/25ldw==", binary)
        self.assertIn(b"new file", vendor.render_diff("vendor/pkg/empty", None, b""))
        self.assertIn(
            b"No newline at end of file", vendor.render_diff("vendor/pkg/text", b"old", b"new")
        )

    def test_https_validation_covers_redirect_targets(self) -> None:
        """Credentials, downgraded redirects and unsupported ports are refused before requests."""
        for url in (
            "http://example.invalid/x",
            "file:///tmp/x",
            "https://user:secret@example.invalid/x",
            "https://example.invalid:8080/x",
            "https://example.invalid/x#fragment",
        ):
            with self.subTest(url=url), self.assertRaises(vendor.IntegrityError):
                _ = vendor.https_url(url)
        handler = vendor.HTTPSRedirects()
        with self.assertRaises(vendor.IntegrityError):
            _ = handler.redirect_request(
                Mock(), Mock(), 302, "redirect", Mock(), "http://example.invalid/x"
            )


if __name__ == "__main__":
    _ = unittest.main()
