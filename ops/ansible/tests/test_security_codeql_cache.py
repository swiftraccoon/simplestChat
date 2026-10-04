"""Verify exact extraction provenance without ever caching current scan successes."""

from __future__ import annotations

import hashlib
import json
import stat
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import ci_verified
import security_codeql
import security_codeql_cache as cache
import security_codeql_local as local
import security_codeql_resources as resources
import security_codeql_tools as tools
from security_context import Context
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject


class NativeCacheTests(unittest.TestCase):
    """Cache corruption, input drift and unavailable source must fail before analysis."""

    directory: Path = Path()

    @override
    def setUp(self) -> None:
        """Keep all source, database and cache fixtures inside the ignored repository results."""
        self.assertFalse((ROOT / "results").is_symlink())
        (ROOT / "results").mkdir(mode=0o700, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="native-cache-", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)

    def database(self) -> tuple[Path, Path, JsonObject]:
        """Represent a finalized genuine native database's source archive contract."""
        source = self.directory / "source"
        database = self.directory / "database"
        source.mkdir()
        database.mkdir()
        _ = (database / "codeql-database.yml").write_text("sourceLocationPrefix: " + str(source))
        manifest: JsonObject = {}
        with zipfile.ZipFile(database / "src.zip", "w") as archive:
            for name in security_codeql.REQUIRED:
                data = ("// " + name).encode()
                manifest[name] = hashlib.sha256(data).hexdigest()
                archive.writestr(str(source / name).lstrip("/"), data)
        return database, source, manifest

    def test_source_integrity_requires_current_protocol_bytes_and_exact_root(self) -> None:
        """Relocation or changed native bytes cannot be passed off as the current extraction."""
        database, source, manifest = self.database()
        cache.source_integrity(database, source, manifest, "c-cpp")
        with self.assertRaisesRegex(ToolError, "sourceLocationPrefix"):
            cache.source_integrity(database, source / "different", manifest, "c-cpp")
        manifest[next(iter(manifest))] = "0" * 64
        with self.assertRaisesRegex(ToolError, "codeql_cache_source_changed"):
            cache.source_integrity(database, source, manifest, "c-cpp")

    def test_source_integrity_rejects_missing_protocols_and_unkeyed_web_bytes(self) -> None:
        """The ignored frontend subtree is reusable only because native extraction contains none."""
        database, source, manifest = self.database()
        with zipfile.ZipFile(database / "src.zip", "w") as archive:
            archive.writestr(str(source / "web/index.ts").lstrip("/"), b"web")
        with self.assertRaisesRegex(ToolError, "codeql_cache_unkeyed_source"):
            cache.source_integrity(database, source, manifest, "c-cpp")
        with zipfile.ZipFile(database / "src.zip", "w") as archive:
            archive.writestr("external/header.h", b"header")
        with self.assertRaisesRegex(ToolError, "codeql_cache_source_coverage"):
            cache.source_integrity(database, source, manifest, "c-cpp")

    def test_rust_source_integrity_binds_main_and_shared_json(self) -> None:
        """Rust's buildless source archive may contain its separately keyed shared JSON data."""
        database, source, _ = self.database()
        bodies = {"src/main.rs": b"fn main() {}", "web/tests/shared.json": b"{}"}
        manifest: JsonObject = {
            name: hashlib.sha256(body).hexdigest() for name, body in bodies.items()
        }
        with zipfile.ZipFile(database / "src.zip", "w") as archive:
            for name, body in bodies.items():
                archive.writestr(str(source / name).lstrip("/"), body)
        cache.source_integrity(database, source, manifest, "rust")
        manifest["web/tests/shared.json"] = "0" * 64
        with self.assertRaisesRegex(ToolError, "codeql_cache_source_changed"):
            cache.source_integrity(database, source, manifest, "rust")

    def test_archive_rejects_escapes_links_duplicates_and_expansion(self) -> None:
        """Never hand a dangerous or oversized cached ZIP to the analyzer's unpacker."""
        for name, mode in (("../escape", 0), ("/absolute", 0), ("link", stat.S_IFLNK)):
            with self.subTest(name=name):
                path = self.directory / "unsafe.zip"
                with zipfile.ZipFile(path, "w") as archive:
                    member = zipfile.ZipInfo(name)
                    member.external_attr = mode << 16
                    archive.writestr(member, b"fixture")
                with zipfile.ZipFile(path) as archive, self.assertRaises(ToolError):
                    _ = cache.archive_members(archive)
        with (
            zipfile.ZipFile(path) as archive,
            patch.object(cache, "MAX_BUNDLE", 1),
            self.assertRaises(ToolError),
        ):
            _ = cache.archive_members(archive)

    def test_key_binds_paths_packages_compilers_openssl_and_native_source(self) -> None:
        """Each environment or extraction input change invalidates an otherwise identical key."""
        with (
            patch.object(ci_verified, "cache_key", return_value="native-inputs") as inputs,
            patch.object(cache, "command", return_value=b"packages") as packages,
            patch.object(cache, "executable", return_value="/usr/bin/dpkg-query"),
            patch.object(ci_verified, "digest", return_value="compiler") as compiler,
            patch.object(cache, "openssl_identity", return_value="openssl") as openssl,
            patch.object(tools, "pin", return_value={"pin": "fixture"}) as pin,
        ):
            source, prefix = self.directory / "source", self.directory / "openssl"
            original = cache.cache_key(ROOT, source, prefix, "c-cpp", "security")
            self.assertEqual(original, cache.cache_key(ROOT, source, prefix, "c-cpp", "security"))
            self.assertNotEqual(original, cache.cache_key(ROOT, source, prefix, "c-cpp", "all"))
            self.assertNotEqual(original, cache.cache_key(ROOT, source, None, "rust", "security"))
            self.assertNotEqual(
                original, cache.cache_key(ROOT, source / "other", prefix, "c-cpp", "security")
            )
            self.assertNotEqual(
                original, cache.cache_key(ROOT, source, prefix / "other", "c-cpp", "security")
            )
            for mocked, before, changed in (
                (inputs, "native-inputs", "changed-inputs"),
                (packages, b"packages", b"changed-packages"),
                (compiler, "compiler", "changed-compiler"),
                (openssl, "openssl", "changed-openssl"),
                (pin, {"pin": "fixture"}, {"pin": "changed"}),
            ):
                mocked.return_value = changed
                self.assertNotEqual(
                    original, cache.cache_key(ROOT, source, prefix, "c-cpp", "security")
                )
                mocked.return_value = before

    def bundle(self, directory: Path, *, result: bool = True) -> JsonObject:
        """Create ordinary cached bytes and their bounded exact receipt."""
        directory.mkdir()
        bundle = directory / "database.zip"
        with zipfile.ZipFile(bundle, "w") as archive:
            archive.writestr("c-cpp/codeql-database.yml", "fixture")
            if result:
                archive.writestr("c-cpp/results/old.bqrs", b"old results")
        receipt: JsonObject = {
            "schemaVersion": 1,
            "key": "exact-key",
            "revision": "a" * 40,
            "bundleSha256": hashlib.sha256(bundle.read_bytes()).hexdigest(),
            "bundleBytes": bundle.stat().st_size,
        }
        _ = (directory / "receipt.json").write_text(json.dumps(receipt))
        return receipt

    def test_restore_preserves_extraction_revision_and_refuses_changed_bundle(self) -> None:
        """A hit has historical extraction provenance; its bytes are verified before unbundling."""
        directory = self.directory / "cache"
        receipt = self.bundle(directory)
        context = Context(ROOT, self.directory)
        with patch.object(context, "run", return_value=(0, b"")) as run:
            restored = cache.restore(
                context, Path("/codeql"), directory, "exact-key", self.directory / "c-cpp"
            )
            self.assertEqual(restored, {**receipt, "reused": True})
            self.assertEqual(run.call_count, 1)
            run.reset_mock()
            with (directory / "database.zip").open("ab") as stream:
                _ = stream.write(b"changed")
            with self.assertRaises(ToolError):
                _ = cache.restore(
                    context, Path("/codeql"), directory, "exact-key", self.directory / "c-cpp"
                )
            run.assert_not_called()

    def test_restore_rejects_missing_queries_wrong_key_and_partial_entries(self) -> None:
        """Absent query results and partial databases cannot become a hit."""
        directory = self.directory / "cache"
        _ = self.bundle(directory, result=False)
        context = Context(ROOT, self.directory)
        with patch.object(context, "run") as run:
            for key in ("wrong-key", "exact-key"):
                with self.assertRaises(ToolError):
                    _ = cache.restore(
                        context, Path("/codeql"), directory, key, self.directory / "c-cpp"
                    )
            (directory / "receipt.json").unlink()
            with self.assertRaises(ToolError):
                _ = cache.restore(
                    context, Path("/codeql"), directory, "exact-key", self.directory / "c-cpp"
                )
            run.assert_not_called()

    def run_analysis(
        self, *, hit: bool = True, passed: bool = True, language: str = "c-cpp"
    ) -> list[JsonObject]:
        """Exercise the real orchestration with controlled external analyzer and policy outcomes."""
        context = Context(ROOT, self.directory)
        source = self.directory / "source"
        source.mkdir()
        _ = (self.directory / "source-manifest.json").write_text("{}")
        args = local.Options()
        args.revision = "b" * 40
        args.database_cache = self.directory / "cache"
        args.openssl_prefix = self.directory / "openssl"
        (args.openssl_prefix / "lib").mkdir(parents=True)
        for name in ("libssl.a", "libcrypto.a"):
            _ = (args.openssl_prefix / "lib" / name).write_bytes(b"fixture")
        calls: list[tuple[str, list[str]]] = []

        def run(name: str, argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes]:
            calls.append((name, list(argv)))
            if name.startswith("codeql-analyze-"):
                _ = (self.directory / (language + "-security.sarif")).write_text("fixture")
            if name.startswith("codeql-policy-"):
                policy = self.directory / (language + "-security-policy")
                policy.mkdir()
                _ = (policy / "report.json").write_text(json.dumps({"passed": passed}))
            return 0, b""

        with (
            patch.object(sys, "platform", "linux"),
            patch.object(resources, "detect", return_value=resources.Budget(2, 4096, 7168)),
            patch.object(context, "run", side_effect=run),
            patch.object(cache, "cache_key", return_value="key"),
            patch.object(
                cache,
                "restore",
                return_value={"revision": "a" * 40, "reused": True} if hit else None,
            ),
            patch.object(cache, "source_integrity") as integrity,
            patch.object(
                cache, "save", return_value={"revision": args.revision, "reused": False}
            ) as save,
            patch.object(local, "database_health") as health,
            patch.object(local, "source_identity"),
            patch.object(local, "native_coverage", return_value=200) as coverage,
            patch.object(tools, "suite", return_value=Path("/queries.qls")),
            patch.object(local, "report_health", return_value={"sarifSha256": "c" * 64}),
        ):
            if passed:
                reports = local.analyze_language(context, args, source, Path("/codeql"), language)
            else:
                with self.assertRaisesRegex(ToolError, "codeql_local_policy_failed"):
                    _ = local.analyze_language(context, args, source, Path("/codeql"), language)
                self.assertTrue(
                    local.upload_ready(self.directory, args.revision, "security", language)
                )
                reports = []
        integrity.assert_called_once()
        health.assert_called_once()
        if language == "c-cpp":
            coverage.assert_called_once()
        else:
            coverage.assert_not_called()
        if hit or not passed:
            save.assert_not_called()
        else:
            save.assert_called_once()
        self.assertEqual(
            [name for name, _ in calls],
            [
                *(["codeql-vendor-integrity"] if language == "c-cpp" else []),
                *([] if hit else ["codeql-create-" + language]),
                "codeql-analyze-" + language + "-security",
                "codeql-policy-" + language + "-security",
            ],
        )
        self.assertEqual("--rerun" in calls[-2][1], not hit)
        self.assertIn("--ram=7168", calls[-2][1])
        if passed:
            self.assertEqual(reports[0]["ramMiB"], 7168)
        self.assertIn("--sarif-run-property=queryReuseEnabled=" + str(hit).lower(), calls[-2][1])
        self.assertIn(
            "--sarif-run-property=originalEvaluationRevision=" + ("a" if hit else "b") * 40,
            calls[-2][1],
        )
        return reports

    def test_hit_regenerates_sarif_and_reruns_coverage_and_policy(self) -> None:
        """CodeQL may reuse exact evaluated results while current coverage and policy always run."""
        reports = self.run_analysis()
        self.assertEqual(reports[0]["extraction"], {"revision": "a" * 40, "reused": True})
        self.assertEqual(reports[0]["queryReuse"], {"enabled": True, "originalRevision": "a" * 40})

    def test_rust_hit_regenerates_current_reports_without_native_compilation(self) -> None:
        """Buildless Rust uses the same exact database proof and current policy boundary."""
        reports = self.run_analysis(language="rust")
        self.assertEqual(reports[0]["nativeCompiledFiles"], None)
        self.assertEqual(reports[0]["queryReuse"], {"enabled": True, "originalRevision": "a" * 40})

    def test_miss_compiles_and_records_the_current_extraction_revision(self) -> None:
        """An absent receipt must build the actual native worker before querying and saving."""
        reports = self.run_analysis(hit=False)
        self.assertEqual(reports[0]["extraction"], {"revision": "b" * 40, "reused": False})
        self.assertEqual(reports[0]["queryReuse"], {"enabled": False, "originalRevision": "b" * 40})

    def test_policy_failure_remains_failed_but_preserves_valid_report_for_upload(self) -> None:
        """Current high findings fail the runner without hiding their original SARIF from GitHub."""
        self.assertEqual(self.run_analysis(passed=False), [])

    def test_new_query_results_are_not_published_after_policy_failure(self) -> None:
        """A failed cold policy result must not create a reusable analyzed database."""
        self.assertEqual(self.run_analysis(hit=False, passed=False), [])

    def test_bundle_requires_language_bound_bqrs_and_forbids_cached_sarif(self) -> None:
        """The CLI may reuse evaluated data, but cannot inherit a previous uploaded report."""
        directory = self.directory / "cache"
        _ = self.bundle(directory)
        bundle = directory / "database.zip"
        cache.bundle_integrity(bundle, "c-cpp")
        with self.assertRaises(ToolError):
            cache.bundle_integrity(bundle, "rust")
        with zipfile.ZipFile(bundle, "a") as archive:
            archive.writestr("c-cpp/results/stale.sarif", "stale")
        with self.assertRaises(ToolError):
            cache.bundle_integrity(bundle, "c-cpp")

    def test_upload_requires_matching_current_revision_and_original_sarif_hash(self) -> None:
        """Policy failure permits valid alerts; absent or changed reports never upload."""
        report = self.directory / "c-cpp-security.sarif"
        self.assertFalse(local.upload_ready(self.directory, "a" * 40, "security", "c-cpp"))
        _ = report.with_suffix(".validated.json").write_text(
            json.dumps({"revision": "a" * 40, "sarifSha256": "hash"})
        )
        with patch.object(local, "report_health", return_value={"sarifSha256": "hash"}):
            self.assertTrue(local.upload_ready(self.directory, "a" * 40, "security", "c-cpp"))
            with self.assertRaises(ToolError):
                _ = local.upload_ready(self.directory, "b" * 40, "security", "c-cpp")
        with (
            patch.object(local, "report_health", return_value={"sarifSha256": "changed"}),
            self.assertRaises(ToolError),
        ):
            _ = local.upload_ready(self.directory, "a" * 40, "security", "c-cpp")


if __name__ == "__main__":
    _ = unittest.main()
