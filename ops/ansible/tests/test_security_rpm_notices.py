"""Bounded notice evidence regressions use inert files in owned temporary trees."""

from __future__ import annotations

import copy
import hashlib
import json
import stat
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import obj

# isort: split
import security_image_policy as policy
import security_rpm_notices as notices
from security_tools import ToolError

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

BODY = b"Inert notice content; never include this text in the report."
NOTICE = "/usr/share/licenses/fixture/LICENSE"


def package(root: Path) -> JsonObject:
    """Create one real file and its agreeing RPM/Syft metadata."""
    path = root / NOTICE.lstrip("/")
    path.parent.mkdir(parents=True)
    _ = path.write_bytes(BODY)
    return {
        "name": "fixture",
        "type": "rpm",
        "version": "1.0-1.fc44",
        "foundBy": "rpm-db-cataloger",
        "metadataType": "rpm-db-entry",
        "purl": "pkg:rpm/fedora/fixture@1.0-1.fc44?arch=x86_64&distro=fedora-44"
        + "&upstream=fixture-1.0-1.fc44.src.rpm",
        "licenses": [{"spdxExpression": "MIT"}],
        "metadata": {
            "name": "fixture",
            "version": "1.0",
            "release": "1.fc44",
            "sourceRpm": "fixture-1.0-1.fc44.src.rpm",
            "epoch": None,
            "architecture": "x86_64",
            "files": [
                {
                    "path": NOTICE,
                    "mode": stat.S_IFREG | 0o644,
                    "size": len(BODY),
                    "digest": {"algorithm": "sha256", "value": hashlib.sha256(BODY).hexdigest()},
                }
            ],
        },
    }


def collect(root: Path, packages: list[JsonObject]) -> JsonObject:
    """Bind fixtures to distinct complete artifact identities."""
    return notices.collect(
        root,
        packages,
        image_id="sha256:" + "a" * 64,
        archive_sha256="b" * 64,
        sbom_sha256="c" * 64,
        revision="d" * 40,
        platform="linux/amd64",
    )


class RpmNoticeTests(unittest.TestCase):
    """Notice collection preserves observations without granting license policy exceptions."""

    def test_exact_file_identity_and_no_content_are_retained(self) -> None:
        """The report binds the artifact, package and actual notice without copying its text."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            report = collect(root, [value])
            self.assertTrue(report["passed"])
            self.assertEqual(report["noticeBytes"], len(BODY))
            self.assertEqual(report["archiveSha256"], "b" * 64)
            observed = obj(report, "runtimePackages", 0, "notices", 0)
            self.assertEqual(observed["sha256"], hashlib.sha256(BODY).hexdigest())
            self.assertTrue(observed["matchesRpmDigest"])
            self.assertNotIn(BODY.decode(), json.dumps(report))
            self.assertNotIn("waived", report)

    def test_changed_regular_notice_retains_failed_integrity(self) -> None:
        """A mismatching notice remains reviewable but cannot have a passing receipt."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            _ = (root / NOTICE.lstrip("/")).write_bytes(b"different")
            report = collect(root, [value])
            self.assertFalse(report["passed"])
            self.assertFalse(obj(report, "runtimePackages", 0, "notices", 0)["matchesRpmDigest"])

    def test_notice_symlink_stays_inside_the_notice_scope(self) -> None:
        """A symlink can identify another notice, never an unrelated image file."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            alias = root / "usr/share/licenses/fixture/COPYING"
            alias.symlink_to("LICENSE")
            record = obj(value, "metadata", "files", 0)
            record.update(path="/usr/share/licenses/fixture/COPYING", mode=stat.S_IFLNK | 0o777)
            obj(record, "digest")["value"] = ""
            report = collect(root, [value])
            observed = obj(report, "runtimePackages", 0, "notices", 0)
            self.assertEqual(observed["resolvedPath"], NOTICE)
            self.assertIsNone(observed["matchesRpmDigest"])
            alias.unlink()
            (root / "etc").mkdir()
            _ = (root / "etc/inert").write_bytes(BODY)
            alias.symlink_to("/etc/inert")
            with self.assertRaisesRegex(ToolError, "link_scope"):
                _ = collect(root, [value])

    def test_empty_notice_set_does_not_read_arbitrary_payloads(self) -> None:
        """Unselected paths are explicit absence of notice evidence, not failed file reads."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            obj(value, "metadata", "files", 0)["path"] = "/app/uncreated-payload"
            report = collect(root, [value])
            self.assertEqual(report["noticeFiles"], 0)
            self.assertEqual(obj(report, "runtimePackages", 0)["notices"], [])

    def test_identity_duplicates_and_unconfined_paths_fail(self) -> None:
        """Names, origins and canonical image paths cannot disagree with package identities."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            original = package(root)
            for key, replacement in (("architecture", "aarch64"), ("sourceRpm", "other.src.rpm")):
                value = copy.deepcopy(original)
                obj(value, "metadata")[key] = replacement
                with self.subTest(key=key), self.assertRaises(ToolError):
                    _ = collect(root, [value])
            with self.assertRaisesRegex(ToolError, "duplicate_package"):
                _ = collect(root, [original, original])
            for path in ("/usr/share/licenses/../outside", "relative", "/usr/share/licenses//file"):
                value = copy.deepcopy(original)
                obj(value, "metadata", "files", 0)["path"] = path
                with self.subTest(path=path), self.assertRaisesRegex(ToolError, "notice_path"):
                    _ = collect(root, [value])
            with self.assertRaisesRegex(ToolError, "rootfs"):
                _ = collect(root / NOTICE.lstrip("/"), [original])

    def test_metadata_count_and_byte_budgets_fail_closed(self) -> None:
        """Independent limits apply before excessive traversal, reading or publication."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            for name, limit in (
                ("MAX_PACKAGES", 0),
                ("MAX_METADATA_FILES", 0),
                ("MAX_NOTICES", 0),
                ("MAX_NOTICE_BYTES", len(BODY) - 1),
                ("MAX_TOTAL_BYTES", len(BODY) - 1),
            ):
                with (
                    self.subTest(limit=name),
                    patch.object(notices, name, limit),
                    self.assertRaises(ToolError),
                ):
                    _ = collect(root, [value])
            report = collect(root, [value])
            with (
                patch.object(notices, "MAX_REPORT_BYTES", len(json.dumps(report).encode())),
                self.assertRaisesRegex(ToolError, "report_budget"),
            ):
                _ = collect(root, [value])

    def test_license_review_identity_matches_policy_without_approving_it(self) -> None:
        """Complete, missing and mixed declarations preserve the policy's exact fingerprint."""
        variants: list[list[JsonValue]] = [
            [{"spdxExpression": "MIT"}],
            [],
            [{"value": "unparsed"}],
            [{"spdxExpression": "MIT"}, {"value": "unparsed"}],
        ]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            value = package(root)
            for records in variants:
                value["licenses"] = records
                report = collect(root, [value])
                verdict = policy.license_verdict([value], {"allowedLicenses": []}, [])
                self.assertFalse(verdict["passed"])
                self.assertEqual(
                    obj(report, "runtimePackages", 0, "licenses")["reviewFingerprint"],
                    obj(verdict, "blocked", 0)["fingerprint"],
                )
