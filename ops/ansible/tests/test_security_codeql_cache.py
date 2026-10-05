"""Verify exact extraction provenance without ever caching current scan successes."""

from __future__ import annotations

import hashlib
import json
import stat
import sys
import tempfile
import unittest
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import ci_verified
import security_codeql
import security_codeql_cache as cache
import security_codeql_cargo as cargo_cache
import security_codeql_local as local
import security_codeql_resources as resources
import security_codeql_tools as tools
from security_context import Context
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject


@dataclass(kw_only=True)
class AnalysisFixture:
    """Record external commands and control analyzer/policy outcomes without running CodeQL."""

    directory: Path
    language: str
    passed: bool
    failure: str | None
    calls: list[tuple[str, list[str]]] = field(default_factory=list)

    def run(self, name: str, argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes]:
        """Produce fixture report files or stop at the selected failed analysis command."""
        self.calls.append((name, list(argv)))
        if name == self.failure:
            raise ToolError("fixture_command_failed")  # noqa: EM101 -- Fixed safe code.
        if name.startswith("codeql-analyze-"):
            _ = (self.directory / (self.language + "-security.sarif")).write_text(
                json.dumps({"runs": [{"results": []}]})
            )
        if name.startswith("codeql-policy-"):
            policy = self.directory / (self.language + "-security-policy")
            policy.mkdir()
            _ = (policy / "report.json").write_text(json.dumps({"passed": self.passed}))
        return 0, b""

    def publish(self, *_args: object, **_kwargs: object) -> JsonObject:
        """Record atomic cache publication alongside analysis and policy commands."""
        self.calls.append(("cache-save", []))
        return {"revision": "b" * 40, "reused": False}


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

    def test_source_archives_reject_policy_data_excluded_from_analysis_keys(self) -> None:
        """Review-only reuse fails closed if extraction actually consumed an excluded file."""
        database, source, manifest = self.database()
        for language in ("rust", "c-cpp"):
            for name in ci_verified.CODEQL_POLICY_DATA:
                with self.subTest(language=language, source=name):
                    with zipfile.ZipFile(database / "src.zip", "w") as archive:
                        archive.writestr(str(source / name).lstrip("/"), b"review data")
                    with self.assertRaisesRegex(ToolError, "codeql_cache_unkeyed_source"):
                        cache.source_integrity(database, source, manifest, language)

    def test_rust_source_archive_rejects_tracked_bytes_outside_its_input_key(self) -> None:
        """A newly embedded unkeyed document cannot inherit an earlier Rust database."""
        database, source, _ = self.database()
        for name in ("README.md", "docs/example.md", "web/package.json", "build/unrelated.py"):
            with self.subTest(source=name):
                bodies = {"src/main.rs": b"fn main() {}", name: b"new embedded input"}
                manifest: JsonObject = {
                    path: hashlib.sha256(body).hexdigest() for path, body in bodies.items()
                }
                with zipfile.ZipFile(database / "src.zip", "w") as archive:
                    for path, body in bodies.items():
                        archive.writestr(str(source / path).lstrip("/"), body)
                with self.assertRaisesRegex(ToolError, "codeql_cache_unkeyed_source"):
                    cache.source_integrity(database, source, manifest, "rust")

    def test_rust_source_archive_allows_new_targets_and_checks_embedded_inputs(self) -> None:
        """Keyed Rust targets and non-Rust embedded bytes keep exact archive validation."""
        database, source, _ = self.database()
        bodies = {
            "src/main.rs": b"fn main() {}",
            "web/new_target/helper.rs": b"fn helper() {}",
            "migrations/001_example.sql": b"SELECT 1;",
            "security/authorization/operations.json": b"{}",
            "vendor/seclists-passwords/10k-most-common.txt": b"fixture",
        }
        manifest: JsonObject = {
            name: hashlib.sha256(body).hexdigest() for name, body in bodies.items()
        }
        with zipfile.ZipFile(database / "src.zip", "w") as archive:
            for name, body in bodies.items():
                archive.writestr(str(source / name).lstrip("/"), body)
        cache.source_integrity(database, source, manifest, "rust")
        manifest["migrations/001_example.sql"] = "0" * 64
        with self.assertRaisesRegex(ToolError, "codeql_cache_source_changed"):
            cache.source_integrity(database, source, manifest, "rust")

    def test_native_source_archive_refuses_unkeyed_tracked_inputs_but_allows_generated_sources(
        self,
    ) -> None:
        """Generated/external compiler inputs remain valid; tracked source must be in the key."""
        database, source, manifest = self.database()
        with zipfile.ZipFile(database / "src.zip", "a") as archive:
            archive.writestr(str(source / "target/generated.cpp").lstrip("/"), b"generated")
            archive.writestr("usr/include/stdlib.h", b"external")
        cache.source_integrity(database, source, manifest, "c-cpp")
        name = "build/unrelated.cpp"
        manifest[name] = hashlib.sha256(b"unkeyed").hexdigest()
        with zipfile.ZipFile(database / "src.zip", "a") as archive:
            archive.writestr(str(source / name).lstrip("/"), b"unkeyed")
        with self.assertRaisesRegex(ToolError, "codeql_cache_unkeyed_source"):
            cache.source_integrity(database, source, manifest, "c-cpp")

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
                cache.cache_key(ROOT, source, None, "rust", "security"),
                cache.cache_key(ROOT, source, None, "rust", "security", include_vendor=True),
            )
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

    def test_save_restore_keeps_final_results_when_clearing_intermediates(self) -> None:
        """Keep evaluated bytes and diagnostics when clearing either language's intermediates."""
        contents = {
            "codeql-database.yml": b"extraction identity",
            "src.zip": b"archived source bytes",
            "results/security.bqrs": b"evaluated security results",
            "results/diagnostics.bqrs": b"completed extraction diagnostics",
        }
        for language in ("c-cpp", "rust"):
            with self.subTest(language=language):
                output = self.directory / language
                output.mkdir()
                directory = output / "cache"
                database = output / "restored" / language
                context = Context(ROOT, output)

                def run(
                    name: str,
                    argv: Sequence[str],
                    *,
                    language: str = language,
                    output: Path = output,
                    database: Path = database,
                    **_kwargs: object,
                ) -> tuple[int, bytes]:
                    if name == "codeql-cache-bundle":
                        self.assertIn("--include-results", argv)
                        self.assertIn("--include-diagnostics", argv)
                        self.assertIn("--no-include-logs", argv)
                        self.assertIn("--cache-cleanup=clear", argv)
                        self.assertEqual(argv[-1], str(output / "databases" / language))
                        path = Path(
                            next(
                                a.removeprefix("--output=")
                                for a in argv
                                if a.startswith("--output=")
                            )
                        )
                        with zipfile.ZipFile(path, "w") as archive:
                            for member, data in contents.items():
                                archive.writestr(language + "/" + member, data)
                    else:
                        self.assertEqual(name, "codeql-cache-unbundle")
                        self.assertIn("--name=" + language, argv)
                        self.assertIn("--target=" + str(database.parent), argv)
                        with zipfile.ZipFile(argv[-1]) as archive:
                            for member in contents:
                                target = database / member
                                target.parent.mkdir(parents=True, exist_ok=True)
                                _ = target.write_bytes(archive.read(language + "/" + member))
                    return 0, b""

                with patch.object(context, "run", side_effect=run):
                    receipt = cache.save(
                        context,
                        Path("/codeql"),
                        directory,
                        "exact-key",
                        "a" * 40,
                        language=language,
                    )
                    restored = cache.restore(
                        context, Path("/codeql"), directory, "exact-key", database
                    )
                self.assertEqual(restored, {**receipt, "reused": True})
                self.assertEqual(
                    {member: (database / member).read_bytes() for member in contents}, contents
                )
                self.assertEqual(
                    {p.name for p in directory.iterdir()}, {"database.zip", "receipt.json"}
                )
                self.assertFalse(list(output.glob(".codeql-analyzed-*")))

    def test_save_refuses_missing_final_results_before_publication(self) -> None:
        """Cleanup can never publish a bundle that lost its evaluated query results."""
        context = Context(ROOT, self.directory)
        directory = self.directory / "cache"

        def run(_name: str, argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes]:
            path = Path(
                next(a.removeprefix("--output=") for a in argv if a.startswith("--output="))
            )
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("c-cpp/codeql-database.yml", b"fixture")
            return 0, b""

        with (
            patch.object(context, "run", side_effect=run),
            self.assertRaisesRegex(ToolError, "codeql_cache_query_results_missing"),
        ):
            _ = cache.save(
                context, Path("/codeql"), directory, "exact-key", "a" * 40, language="c-cpp"
            )
        self.assertFalse(directory.exists())
        self.assertFalse(list(self.directory.glob(".codeql-analyzed-*")))

    def run_analysis(
        self,
        *,
        hit: bool = True,
        passed: bool = True,
        language: str = "c-cpp",
        failure: str | None = None,
        cargo: bool = False,
    ) -> list[JsonObject]:
        """Exercise the real orchestration with controlled external analyzer and policy outcomes."""
        context = Context(ROOT, self.directory)
        source = self.directory / "source"
        source.mkdir()
        _ = (self.directory / "source-manifest.json").write_text("{}")
        args = local.Options(include_vendor=language == "c-cpp")
        args.revision = "b" * 40
        args.suite = "all" if failure and failure.endswith("quality-advisory") else "security"
        args.database_cache = self.directory / "cache"
        args.rust_cargo_cache = self.directory / "cargo" if cargo else None
        args.openssl_prefix = self.directory / "openssl"
        (args.openssl_prefix / "lib").mkdir(parents=True)
        for name in ("libssl.a", "libcrypto.a"):
            _ = (args.openssl_prefix / "lib" / name).write_bytes(b"fixture")
        fixture = AnalysisFixture(
            directory=self.directory, language=language, passed=passed, failure=failure
        )
        calls = fixture.calls

        with (
            patch.object(sys, "platform", "linux"),
            patch.object(resources, "detect", return_value=resources.Budget(2, 4096, 7168)),
            patch.object(context, "run", side_effect=fixture.run),
            patch.object(cache, "cache_key", return_value="key"),
            patch.object(
                cache,
                "restore",
                return_value={"revision": "a" * 40, "reused": True} if hit else None,
            ),
            patch.object(cache, "source_integrity") as integrity,
            patch.object(cache, "save", side_effect=fixture.publish) as save,
            patch.object(
                cargo_cache, "prepare", return_value=self.directory / "cargo" / "target"
            ) as prepare,
            patch.object(local, "database_health") as health,
            patch.object(local, "source_identity"),
            patch.object(local, "native_coverage", return_value=200) as coverage,
            patch.object(tools, "suite", return_value=Path("/queries.qls")),
            patch.object(
                local,
                "report_health",
                return_value={
                    "language": language,
                    "category": "security",
                    "sarifSha256": "c" * 64,
                },
            ),
        ):
            if failure:
                with self.assertRaisesRegex(ToolError, "fixture_command_failed"):
                    _ = local.analyze_language(context, args, source, Path("/codeql"), language)
                save.assert_not_called()
                self.assertFalse((self.directory / (language + "-cache-ready.json")).exists())
                self.assertFalse(any(name.startswith("codeql-policy-") for name, _ in calls))
                return []
            if passed:
                reports = local.analyze_language(context, args, source, Path("/codeql"), language)
            else:
                with self.assertRaisesRegex(ToolError, "codeql_local_policy_failed"):
                    _ = local.analyze_language(context, args, source, Path("/codeql"), language)
                if language in tools.AUTOMATED_LANGUAGES:
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
        if hit:
            save.assert_not_called()
        else:
            save.assert_called_once()
        self.assertEqual((self.directory / (language + "-cache-ready.json")).exists(), not hit)
        self.check_analysis_calls(calls, language=language, hit=hit)
        if cargo and language == "rust" and not hit:
            prepare.assert_called_once_with(context.root, source, args.rust_cargo_cache)
            create = next(argv for name, argv in calls if name == "codeql-create-rust")
            self.assertIn(
                "--extractor-option=cargo_target_dir=" + str(self.directory / "cargo" / "target"),
                create,
            )
        else:
            prepare.assert_not_called()
        if passed:
            self.assertEqual(reports[0]["ramMiB"], 7168)
        return reports

    def check_analysis_calls(
        self, calls: list[tuple[str, list[str]]], *, language: str, hit: bool
    ) -> None:
        """Check query reuse and fresh policy run in the required publication order."""
        self.assertEqual(
            [name for name, _ in calls],
            [
                *(["codeql-vendor-integrity"] if language == "c-cpp" else []),
                *(
                    []
                    if hit
                    else (
                        ["codeql-init-c-cpp", "codeql-trace-c-cpp", "codeql-finalize-c-cpp"]
                        if language == "c-cpp"
                        else ["codeql-create-" + language]
                    )
                ),
                "codeql-analyze-" + language + "-security",
                *([] if hit else ["cache-save"]),
                "codeql-policy-" + language + "-security",
            ],
        )
        analyze = next(command for name, command in calls if name.startswith("codeql-analyze-"))
        self.assertEqual("--rerun" in analyze, not hit)
        self.assertIn("--ram=7168", analyze)
        self.assertIn("--sarif-run-property=queryReuseEnabled=" + str(hit).lower(), analyze)
        self.assertIn(
            "--sarif-run-property=originalEvaluationRevision=" + ("a" if hit else "b") * 40,
            analyze,
        )

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

    def test_cargo_output_reuse_still_extracts_and_queries_changed_rust_source(self) -> None:
        """Warm dependency output never substitutes for fresh source analysis or current policy."""
        reports = self.run_analysis(language="rust", hit=False, cargo=True)
        self.assertEqual(reports[0]["queryReuse"], {"enabled": False, "originalRevision": "b" * 40})

    def test_evaluated_database_hit_does_not_consume_cargo_build_output(self) -> None:
        """A database hit needs no restored dependency cache or installed Rust compiler."""
        _ = self.run_analysis(language="rust", cargo=True)

    def test_miss_compiles_and_records_the_current_extraction_revision(self) -> None:
        """An absent receipt must build the actual native worker before querying and saving."""
        reports = self.run_analysis(hit=False)
        self.assertEqual(reports[0]["extraction"], {"revision": "b" * 40, "reused": False})
        self.assertEqual(reports[0]["queryReuse"], {"enabled": False, "originalRevision": "b" * 40})

    def test_policy_failure_remains_failed_but_preserves_valid_report_for_upload(self) -> None:
        """Current high findings fail the runner without hiding their original SARIF from GitHub."""
        self.assertEqual(self.run_analysis(passed=False), [])

    def test_healthy_query_results_survive_policy_failure_without_passing_the_gate(self) -> None:
        """Complete evaluated data is reusable even when fresh policy still blocks the job."""
        self.assertEqual(self.run_analysis(hit=False, passed=False), [])

    def test_extraction_failure_never_publishes_analysis_or_runs_policy(self) -> None:
        """An incomplete database cannot acquire a reusable query cache or ready marker."""
        self.assertEqual(
            self.run_analysis(hit=False, language="rust", failure="codeql-create-rust"), []
        )

    def test_later_query_suite_failure_never_publishes_partial_analysis(self) -> None:
        """Security success cannot hide an unfinished requested advisory suite in the cache."""
        self.assertEqual(
            self.run_analysis(
                hit=False, language="rust", failure="codeql-analyze-rust-quality-advisory"
            ),
            [],
        )

    def test_stale_ready_marker_is_rejected_before_analysis(self) -> None:
        """A previous invocation's marker cannot authorize this attempt's cache publication."""
        context = Context(ROOT, self.directory)
        _ = (self.directory / "rust-cache-ready.json").write_text("stale")
        with (
            patch.object(context, "run") as run,
            patch.object(cache, "save") as save,
            self.assertRaisesRegex(ToolError, "codeql_cache_ready_exists"),
        ):
            _ = local.analyze_language(
                context, local.Options(), self.directory / "source", Path("/codeql"), "rust"
            )
        run.assert_not_called()
        save.assert_not_called()

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
        with self.assertRaisesRegex(ToolError, "codeql_vendor_upload_forbidden"):
            _ = local.upload_ready(self.directory, "a" * 40, "security", "c-cpp")
        report = self.directory / "python-security.sarif"
        _ = report.write_text(json.dumps({"runs": [{"results": []}]}))
        self.assertFalse(local.upload_ready(self.directory, "a" * 40, "security", "python"))
        _ = report.with_suffix(".validated.json").write_text(
            json.dumps({"revision": "a" * 40, "sarifSha256": "hash", "includeVendor": False})
        )
        with patch.object(local, "report_health", return_value={"sarifSha256": "hash"}):
            self.assertTrue(local.upload_ready(self.directory, "a" * 40, "security", "python"))
            with self.assertRaises(ToolError):
                _ = local.upload_ready(self.directory, "b" * 40, "security", "python")
            _ = report.with_suffix(".validated.json").write_text(
                json.dumps({"revision": "a" * 40, "sarifSha256": "hash", "includeVendor": True})
            )
            with self.assertRaisesRegex(ToolError, "codeql_upload_identity"):
                _ = local.upload_ready(self.directory, "a" * 40, "security", "python")
        with (
            patch.object(local, "report_health", return_value={"sarifSha256": "changed"}),
            self.assertRaises(ToolError),
        ):
            _ = local.upload_ready(self.directory, "a" * 40, "security", "python")


if __name__ == "__main__":
    _ = unittest.main()
