"""Exercise exact native archive binding with inert local source fixtures."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import tarfile
import tempfile
import unittest
from datetime import date
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import security_codeql_sources as sources
import security_codeql_triage as triage
import security_policy
import security_vendor as vendor
from release_json import JsonObject, array_value, decode_json, object_value
from security_tools import ToolError
from test_security_vendor import source_record, tar_bytes, zip_bytes

if TYPE_CHECKING:
    from collections.abc import Sequence

REVISION = "a" * 40
WRAP = sources.SUBPROJECTS + "/fixture.wrap"
PREFIX = sources.SUBPROJECTS + "/fixture-1.0"
BODY = b"int fixture_value = 1;\n"


class NativeSourceTests(unittest.TestCase):
    """Archive, wrap, overlay and extracted bytes must all agree before finding review."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Initialize typed fixture members before setUp."""
        super().__init__(methodName)
        self.root: Path = ROOT
        self.cache: Path = ROOT
        self.manifest: JsonObject = {}
        self.tracked: dict[str, bytes] = {}
        self.archive_data: bytes = b""

    @override
    def setUp(self) -> None:
        """Create real pinned archive bytes and an independent tracked-source snapshot."""
        temporary = tempfile.TemporaryDirectory(prefix="codeql-native-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.cache = self.root / "cache"
        self.cache.mkdir(mode=0o700)
        self.archive_data = tar_bytes({"fixture-1.0/src/value.c": BODY})
        record = source_record("fixture-1.0", self.archive_data)
        vendor.write_private(self.cache / vendor.sha256(self.archive_data), self.archive_data)
        self.manifest = {
            "sources": [record],
            "trees": [],
            "files": [],
            "maintained_files": [],
            "wraps": [
                {
                    "path": WRAP,
                    "source": "fixture-1.0",
                    "patch_source": None,
                    "patch_directory": None,
                    "fallback_urls": [],
                }
            ],
        }
        self.save(
            WRAP,
            (
                "[wrap-file]\ndirectory = fixture-1.0\n"
                "source_url = https://example.invalid/fixture-1.0\n"
                "source_filename = fixture-1.0.tar.gz\n"
                f"source_hash = {record['sha256']}\n"
            ).encode(),
        )
        self.save_manifest()

    def save(self, name: str, data: bytes) -> None:
        """Update a committed fixture identity and the corresponding working file together."""
        target = self.root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        _ = target.write_bytes(data)
        self.tracked[name] = data

    def save_manifest(self) -> None:
        """Serialize the strict current manifest shape."""
        self.save(sources.MANIFEST, (json.dumps(self.manifest) + "\n").encode())

    def git(self, argv: Sequence[str], **_: object) -> tuple[int, bytes, bytes]:
        """Only the precise selected revision may obtain immutable fixture bytes."""
        self.assertEqual(list(argv[:2]), ["git", "show"])
        self.assertTrue(argv[2].startswith(REVISION + ":"))
        name = argv[2].removeprefix(REVISION + ":")
        return (0, self.tracked[name], b"") if name in self.tracked else (1, b"", b"")

    def resolve(self) -> sources.NativeSources:
        """Exercise production readers with only the Git object transport substituted."""
        return sources.NativeSources(self.root, REVISION, self.cache)

    def test_pinned_member_without_extraction_has_full_provenance(self) -> None:
        """An API-side review can bind source absent from the checkout without downloading."""
        with patch.object(bounded_process, "run", side_effect=self.git):
            resolver = self.resolve()
            identity = resolver.identity(PREFIX + "/src/value.c")
            self.assertIsNotNone(identity)
            value = object_value(identity)
            self.assertEqual(value["sourceSha256"], hashlib.sha256(BODY).hexdigest())
            self.assertEqual(value["sourceArchiveSha256"], vendor.sha256(self.archive_data))
            self.assertEqual(value["sourceMember"], "src/value.c")
            self.assertEqual(value["sourceWrapSha256"], vendor.sha256(self.tracked[WRAP]))
            self.assertIsNone(resolver.identity("src/labels.rs"))
            self.assertIsNone(resolver.identity(sources.SUBPROJECTS + "/unknown/source.c"))

    def test_extracted_source_must_match_and_symlinks_fail(self) -> None:
        """A known archive cannot authorize changed generated compiler input."""
        path = self.root / PREFIX / "src/value.c"
        path.parent.mkdir(parents=True)
        _ = path.write_bytes(BODY)
        with patch.object(bounded_process, "run", side_effect=self.git):
            resolver = self.resolve()
            self.assertIsNotNone(resolver.identity(PREFIX + "/src/value.c"))
            _ = path.write_bytes(b"changed\n")
            with self.assertRaises(ToolError):
                _ = resolver.identity(PREFIX + "/src/value.c")
            path.unlink()
            path.symlink_to(self.root / WRAP)
            with self.assertRaises(ToolError):
                _ = resolver.identity(PREFIX + "/src/value.c")

    def test_missing_corrupted_and_symlinked_cache_never_pass(self) -> None:
        """Even an existing regular source cannot replace authenticated archive evidence."""
        cache_file = self.cache / vendor.sha256(self.archive_data)
        with patch.object(bounded_process, "run", side_effect=self.git):
            for variant in ("missing", "corrupt", "symlink"):
                with self.subTest(variant=variant):
                    cache_file.unlink(missing_ok=True)
                    if variant == "corrupt":
                        _ = cache_file.write_bytes(b"not the pinned input")
                    elif variant == "symlink":
                        cache_file.symlink_to(self.root / WRAP)
                    with self.assertRaises((vendor.IntegrityError, OSError)):
                        _ = self.resolve().identity(PREFIX + "/src/value.c")

    def test_unknown_members_and_changed_manifest_or_wrap_fail(self) -> None:
        """No wildcard root or uncommitted pin can provide reviewable native source."""
        with patch.object(bounded_process, "run", side_effect=self.git):
            with self.assertRaises(ToolError):
                _ = self.resolve().identity(PREFIX + "/src/missing.c")
            for name in (WRAP, sources.MANIFEST):
                with self.subTest(name=name):
                    _ = (self.root / name).write_bytes(self.tracked[name] + b"\n")
                    with self.assertRaises(ToolError):
                        _ = self.resolve()
                    _ = (self.root / name).write_bytes(self.tracked[name])

    def test_remote_patch_replaces_member_only_with_exact_archive(self) -> None:
        """Patch precedence and its digest are part of the finding identity."""
        patched = b"int fixture_value = 2;\n"
        data = zip_bytes([("fixture-1.0/src/value.c", patched)])
        record = source_record("patch", data, format_name="zip")
        vendor.write_private(self.cache / vendor.sha256(data), data)
        self.manifest["sources"] = [*array_value(self.manifest["sources"]), record]
        wrap = object_value(array_value(self.manifest["wraps"])[0])
        wrap["patch_source"] = "patch"
        self.save(
            WRAP,
            self.tracked[WRAP]
            + (
                "patch_url = https://example.invalid/patch\npatch_filename = patch.zip\n"
                f"patch_hash = {record['sha256']}\n"
            ).encode(),
        )
        self.save_manifest()
        with patch.object(bounded_process, "run", side_effect=self.git):
            value = object_value(self.resolve().identity(PREFIX + "/src/value.c"))
            self.assertEqual(value["sourceSha256"], vendor.sha256(patched))
            self.assertEqual(value["sourcePatchSha256"], vendor.sha256(data))

    def test_local_overlay_requires_reviewed_delta_and_tracked_bytes(self) -> None:
        """A packagefiles overlay is checked against both its upstream pin and Git."""
        overlay_path = sources.SUBPROJECTS + "/packagefiles/fixture"
        overlay_body = b"int fixture_value = 3;\n"
        archive = tar_bytes({"fixture/src/value.c": overlay_body})
        record = source_record("overlay", archive)
        vendor.write_private(self.cache / vendor.sha256(archive), archive)
        self.manifest["sources"] = [*array_value(self.manifest["sources"]), record]
        self.manifest["trees"] = [
            {"path": overlay_path, "source": "overlay", "prefix": "fixture", "changes": []}
        ]
        object_value(array_value(self.manifest["wraps"])[0])["patch_directory"] = "fixture"
        self.save(WRAP, self.tracked[WRAP] + b"patch_directory = fixture\n")
        self.save(overlay_path + "/src/value.c", overlay_body)
        self.save_manifest()
        with patch.object(bounded_process, "run", side_effect=self.git):
            value = object_value(self.resolve().identity(PREFIX + "/src/value.c"))
            self.assertEqual(value["sourceSha256"], vendor.sha256(overlay_body))
            self.assertIn("sourceOverlaySha256", value)
            self.save(overlay_path + "/src/value.c", b"unreviewed deviation\n")
            with self.assertRaises(ToolError):
                _ = self.resolve().identity(PREFIX + "/src/value.c")

    def test_archive_root_and_aggregate_content_limits_are_enforced(self) -> None:
        """Authenticated bytes still need the declared extraction root and finite limits."""
        with patch.object(bounded_process, "run", side_effect=self.git):
            with (
                patch.object(vendor, "MAX_CONTENT", len(BODY) - 1),
                self.assertRaises((ToolError, vendor.IntegrityError)),
            ):
                _ = self.resolve().identity(PREFIX + "/src/value.c")
            with (
                patch.object(sources, "archive_files", return_value={"other/src/value.c": BODY}),
                self.assertRaises(ToolError),
            ):
                _ = self.resolve().identity(PREFIX + "/src/value.c")

    def test_native_review_records_bind_each_alert_to_exact_source_and_policy(self) -> None:
        """Every native decision retains provenance and an individual expiring exception."""
        raw = object_value(
            decode_json((ROOT / "security/codeql-native-review-2026-09-30.json").read_bytes())
        )
        entries = [object_value(value) for value in array_value(raw["alerts"])]
        self.assertEqual({triage.positive(item["number"]) for item in entries}, set(range(70, 102)))
        manifest_sha = vendor.sha256((ROOT / sources.MANIFEST).read_bytes())
        previous_sha = object_value(raw["manifestRevalidation"])["previousManifestSha256"]
        reviews = security_policy.read_exceptions(
            today=date.fromisoformat(str(raw["reviewed"])), include_vendor=True
        )
        identity_fields = {
            "rule",
            "toolVersion",
            "path",
            "region",
            "messageSha256",
            "sourceSha256",
            "sourceArchiveSha256",
            "sourceManifestSha256",
            "sourceWrapSha256",
            "sourcePatchSha256",
            "sourceOverlaySha256",
            "sourceMember",
        }
        for item in entries:
            with self.subTest(number=item["number"]):
                self.assertTrue(triage.false_positive(item, "swiftraccoon/simplestChat"))
                self.assertEqual(item["sourceManifestSha256"], manifest_sha)
                identity = {key: value for key, value in item.items() if key in identity_fields}
                self.assertEqual(item["fingerprint"], "codeql:" + triage.digest(identity))
                self.assertTrue(
                    security_policy.permitted(
                        reviews, "codeql", str(item["fingerprint"]), str(item["scope"])
                    )
                )
                identity["sourceManifestSha256"] = previous_sha
                self.assertFalse(
                    security_policy.permitted(
                        reviews, "codeql", "codeql:" + triage.digest(identity), str(item["scope"])
                    )
                )
                for field in (
                    "sourceSha256",
                    "sourceArchiveSha256",
                    "sourceManifestSha256",
                    "sourceWrapSha256",
                ):
                    self.assertRegex(str(item[field]), r"^[a-f0-9]{64}$")
                self.assertNotIn("*", str(item["scope"]))

    def test_authenticated_documentation_symlink_is_never_a_readable_source(self) -> None:
        """Known release symlinks are inert metadata, and descendants cannot alias source."""

        def archive(*, nested: bool) -> bytes:
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w:gz") as bundle:
                link = tarfile.TarInfo("fixture-1.0/docs/link")
                link.type = tarfile.SYMTYPE
                link.linkname = "../../external"
                bundle.addfile(link)
                item = tarfile.TarInfo(
                    "fixture-1.0/docs/link/value.c" if nested else "fixture-1.0/src/value.c"
                )
                item.size = len(BODY)
                bundle.addfile(item, io.BytesIO(BODY))
            return stream.getvalue()

        regular = archive(nested=False)
        source = vendor.Source(
            "fixture", "https://example.invalid/source", vendor.sha256(regular), "tar.gz"
        )
        self.assertEqual(sources.archive_files(source, regular), {"fixture-1.0/src/value.c": BODY})
        nested = archive(nested=True)
        source = vendor.Source(
            "fixture", "https://example.invalid/source", vendor.sha256(nested), "tar.gz"
        )
        with self.assertRaises(ToolError):
            _ = sources.archive_files(source, nested)

    def test_source_binding_changes_fingerprint_without_changing_first_party(self) -> None:
        """Equal member bytes from a different reviewed archive remain different evidence."""
        location: JsonObject = {
            "path": PREFIX + "/src/value.c",
            "start_line": 1,
            "end_line": 1,
            "start_column": 1,
            "end_column": 3,
        }
        with patch.object(bounded_process, "run", side_effect=self.git):
            resolver = self.resolve()
            first = triage.finding(
                self.root,
                REVISION,
                "cpp/fixture",
                "2.27.1",
                location,
                message="fixture",
                sources=resolver,
            )
            altered = copy.deepcopy(first)
            altered["sourceArchiveSha256"] = "f" * 64
            self.assertNotEqual(triage.digest(first), triage.digest(altered))
            self.assertEqual(first["sourceSha256"], vendor.sha256(BODY))
