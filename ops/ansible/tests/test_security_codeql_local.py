"""Require complete local CodeQL evidence and the same hosted analyzer contract."""

from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_codeql_local as local
import security_codeql_resources as resources
import security_codeql_tools as tools
from security_context import Context
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Sequence

    from release_json import JsonObject


def report(language: str, category: str = "security") -> JsonObject:
    """Represent one successful language/suite with independently checked native coverage."""
    return {
        "language": language,
        "category": category,
        "policyPassed": True,
        "rules": 100,
        "nativeCompiledFiles": 200 if language == "c-cpp" else None,
    }


class LocalCodeqlTests(unittest.TestCase):
    """Never accept partial scans, incremental reports or analyzer drift as full CI."""

    directory: Path = Path()

    @override
    def setUp(self) -> None:
        """Own every fixture beneath the repository's ignored results directory."""
        self.assertFalse((ROOT / "results").is_symlink())
        (ROOT / "results").mkdir(mode=0o700, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="codeql-unit-", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)

    def test_complete_health_requires_every_language_and_requested_suite(self) -> None:
        """A missing or duplicate language cannot inherit another language's successful status."""
        reports = [report(language) for language in tools.LANGUAGES]
        local.complete_health(reports, tools.LANGUAGES, "security")
        with self.assertRaisesRegex(ToolError, "codeql_local_incomplete_languages"):
            local.complete_health(reports[:-1], tools.LANGUAGES, "security")
        with self.assertRaisesRegex(ToolError, "codeql_local_report_inventory"):
            local.complete_health([*reports, reports[0]], tools.LANGUAGES, "security")
        with self.assertRaisesRegex(ToolError, "codeql_local_incomplete_languages"):
            local.complete_health(reports, tools.LANGUAGES, "all")
        reports.extend(report(language, "quality-advisory") for language in tools.LANGUAGES)
        local.complete_health(reports, tools.LANGUAGES, "all")

    def test_complete_health_rejects_missing_queries_policy_or_native_evidence(self) -> None:
        """Green commands alone cannot substitute for executed queries or real C++ compilation."""
        for field, value, code in (
            ("rules", 0, "codeql_local_empty_queries"),
            ("policyPassed", False, "codeql_local_policy_failed"),
            ("nativeCompiledFiles", None, "codeql_local_native_coverage"),
        ):
            with self.subTest(field=field):
                row = report("c-cpp")
                row[field] = value
                with self.assertRaisesRegex(ToolError, code):
                    local.complete_health([row], ["c-cpp"], "security")

    def database(self, language: str = "c-cpp") -> Path:
        """Write the relevant generated CodeQL metadata contract."""
        database = self.directory / "database"
        database.mkdir(exist_ok=True)
        _ = (database / "codeql-database.yml").write_text(
            "primaryLanguage: cpp\ncreationMetadata:\n  cliVersion: 2.27.1\n"
            + "finalised: true\noverlayBaseDatabase: false\noverlayDatabase: false\n"
            + ("buildMode: manual\n" if language == "c-cpp" else "buildMode: none\n")
        )
        return database

    def test_database_health_rejects_buildless_native_overlay_and_unfinished_databases(
        self,
    ) -> None:
        """A finalized database must still represent the required fresh traced build."""
        database = self.database()
        local.database_health(database, "c-cpp")
        metadata = database / "codeql-database.yml"
        original = metadata.read_text()
        for before, after in (
            ("buildMode: manual", "buildMode: none"),
            ("buildMode: manual\n", ""),
            ("finalised: true", "finalised: false"),
            ("overlayDatabase: false", "overlayDatabase: true"),
            ("cliVersion: 2.27.1", "cliVersion: 2.27.0"),
            ("primaryLanguage: cpp", "primaryLanguage: javascript"),
        ):
            with self.subTest(change=after):
                _ = metadata.write_text(original.replace(before, after))
                with self.assertRaisesRegex(ToolError, "codeql_metadata_"):
                    local.database_health(database, "c-cpp")

    def test_native_creation_times_each_phase_and_preserves_the_bounded_traced_build(self) -> None:
        """Manual extraction retains compiler/import budgets while exposing separate timing."""
        output = self.directory / "output"
        output.mkdir()
        context = Context(ROOT, output)
        source = self.directory / "source"
        source.mkdir()
        args = local.Options()
        args.openssl_prefix = self.directory / "openssl"
        (args.openssl_prefix / "lib").mkdir(parents=True)
        for archive in ("libssl.a", "libcrypto.a"):
            _ = (args.openssl_prefix / "lib" / archive).write_bytes(b"fixture")
        calls: list[tuple[str, list[str], dict[str, object]]] = []

        def record(name: str, argv: Sequence[str], **kwargs: object) -> tuple[int, bytes]:
            """Capture the real invocation boundary without claiming analyzer execution."""
            calls.append((name, list(argv), kwargs))
            return 0, b""

        with (
            patch.object(sys, "platform", "linux"),
            patch.object(resources, "detect", return_value=resources.Budget(2, 4096, 7168)),
            patch.object(context, "run", side_effect=record),
            patch.object(local, "database_health", side_effect=ToolError("fixture_after_create")),
            self.assertRaisesRegex(ToolError, "fixture_after_create"),
        ):
            _ = local.analyze_language(context, args, source, self.directory / "codeql", "c-cpp")
        self.assertEqual(
            [name for name, _command, _kwargs in calls],
            [
                "codeql-vendor-integrity",
                "codeql-init-c-cpp",
                "codeql-trace-c-cpp",
                "codeql-finalize-c-cpp",
            ],
        )
        command = calls[1][1]
        self.assertEqual(
            [argument for argument in command if argument.startswith("--build-mode=")],
            ["--build-mode=manual"],
        )
        self.assertIn("--source-root=" + str(source), command)
        command = calls[2][1]
        self.assertEqual(command[-3:], ["--", "bash", "build/codeql-native-build.sh"])
        self.assertIn("--threads=2", command)
        self.assertIn("--ram=4096", command)
        self.assertEqual(
            calls[2][2]["env_updates"],
            {"OPENSSL_DIR": str(args.openssl_prefix), "CODEQL_BUILD_JOBS": "2"},
        )
        self.assertIn("--threads=2", calls[3][1])
        self.assertIn("--ram=4096", calls[3][1])
        self.assertTrue(all(kwargs["cwd"] == source for _name, _command, kwargs in calls[1:]))

    def test_native_creation_stops_at_the_first_failed_phase(self) -> None:
        """A failed init or traced build must never continue to import incomplete extraction."""
        phases = ("codeql-init-c-cpp", "codeql-trace-c-cpp", "codeql-finalize-c-cpp")
        for failed_index, failed_phase in enumerate(phases):
            with self.subTest(phase=failed_phase):
                output = self.directory / str(failed_index)
                output.mkdir()
                context = Context(ROOT, output)
                failure = ToolError("fixture_phase_failed")
                with (
                    patch.object(
                        context, "run", side_effect=[*([(0, b"")] * failed_index), failure]
                    ) as run,
                    self.assertRaisesRegex(ToolError, "fixture_phase_failed"),
                ):
                    local.create_native(
                        context,
                        self.directory / "source",
                        self.directory / "codeql",
                        self.directory / "openssl",
                        resources.Budget(4, 6144, 14950),
                    )
                self.assertEqual(
                    [call.args[0] for call in run.call_args_list], list(phases[: failed_index + 1])
                )

    def test_report_health_rejects_wrong_category_version_empty_queries_and_incremental_mode(
        self,
    ) -> None:
        """Unfiltered original SARIF must have the selected query/version identity."""
        path = self.directory / "report.sarif"
        driver: JsonObject = {
            "name": "CodeQL",
            "semanticVersion": "2.27.1",
            "rules": [{"id": "fixture/query"}],
        }
        original: JsonObject = {
            "tool": {"driver": driver},
            "automationDetails": {"id": "/language:actions/security/"},
            "results": [],
            "invocations": [{"executionSuccessful": True}],
        }
        _ = path.write_text(json.dumps({"version": "2.1.0", "runs": [original]}))
        checked = local.report_health(path, "actions", "security")
        self.assertEqual(checked["rules"], 1)
        self.assertEqual(checked["sarifSha256"], hashlib.sha256(path.read_bytes()).hexdigest())
        for update, code in (
            ({"automationDetails": {"id": "/language:python/security/"}}, "category"),
            ({"tool": {"driver": {**driver, "semanticVersion": "2.28.0"}}}, "version"),
            ({"tool": {"driver": {**driver, "rules": []}}}, "empty_queries"),
            ({"properties": {"incrementalMode": "diff-informed"}}, "incremental_report"),
        ):
            with self.subTest(code=code):
                changed = {**copy.deepcopy(original), **update}
                _ = path.write_text(json.dumps({"version": "2.1.0", "runs": [changed]}))
                with self.assertRaisesRegex(ToolError, "codeql_local_.*" + code):
                    _ = local.report_health(path, "actions", "security")
        for invocation in (
            {"executionSuccessful": False},
            {"executionSuccessful": True, "toolExecutionNotifications": [{"level": "warning"}]},
        ):
            changed = {**original, "invocations": [invocation]}
            _ = path.write_text(json.dumps({"version": "2.1.0", "runs": [changed]}))
            with self.assertRaises(ToolError):
                _ = local.report_health(path, "actions", "security")

    def test_pin_requires_workflow_revision_and_all_languages(self) -> None:
        """Changing only the hosted action cannot silently change the query contract."""
        current = tools.pin()
        fixture = self.directory / "pin.json"
        changed = copy.deepcopy(current)
        changed["actionRevision"] = "a" * 40
        _ = fixture.write_text(json.dumps(changed))
        with (
            patch.object(tools, "PIN", fixture),
            self.assertRaisesRegex(ToolError, "codeql_action_pin_mismatch"),
        ):
            _ = tools.pin()

    def test_archive_identity_rejects_wrong_size_and_symlink(self) -> None:
        """The installer checks authenticated bytes before extracting a bundle."""
        path = self.directory / "archive"
        _ = path.write_bytes(b"fixture archive")
        self.assertEqual(
            tools.archive_digest(path, 15), hashlib.sha256(path.read_bytes()).hexdigest()
        )
        with self.assertRaisesRegex(ToolError, "codeql_archive_size"):
            _ = tools.archive_digest(path, 16)
        link = self.directory / "link"
        link.symlink_to(path)
        with self.assertRaisesRegex(ToolError, "codeql_archive_file"):
            _ = tools.archive_digest(link, 15)

    def test_install_rejects_download_mismatch_before_archive_inspection(self) -> None:
        """A failed checksum never reaches tar, even when the download returned success."""
        output = self.directory / "output"
        output.mkdir()
        context = Context(ROOT, output)
        fixture = tools.pin()
        payload = b"fixture archive"
        fixture["bundles"] = {
            target: {
                "url": "https://example.invalid/codeql",
                "bytes": len(payload),
                "sha256": "0" * 64,
            }
            for target in ("darwin", "linux-x86_64", "linux-aarch64")
        }
        calls: list[str] = []

        def download(name: str, _argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes]:
            """Return only the simulated downloaded bytes, never invoke a real downloader."""
            calls.append(name)
            _ = (output / "codeql-bundle.tar.zst").write_bytes(payload)
            return 0, b""

        with (
            patch.object(tools, "ROOT", self.directory),
            patch.object(tools, "pin", return_value=fixture),
            patch.object(context, "run", side_effect=download),
            self.assertRaisesRegex(ToolError, "codeql_archive_digest"),
        ):
            _ = tools.install(context)
        self.assertEqual(calls, ["codeql-download"])

    def test_install_failure_never_publishes_a_partial_cached_bundle(self) -> None:
        """Interrupted extraction removes its private staging tree before another run."""
        output = self.directory / "output"
        output.mkdir()
        context = Context(ROOT, output)
        payload = b"fixture archive"
        digest = hashlib.sha256(payload).hexdigest()
        fixture = tools.pin()
        fixture["bundles"] = {
            target: {
                "url": "https://example.invalid/codeql",
                "bytes": len(payload),
                "sha256": digest,
            }
            for target in ("darwin", "linux-x86_64", "linux-aarch64")
        }

        def interrupted(name: str, argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes]:
            """Write a partial extracted file, then simulate an interrupted tar process."""
            if name == "codeql-download":
                _ = (output / "codeql-bundle.tar.zst").write_bytes(payload)
                return 0, b""
            if name == "codeql-archive-list":
                return 0, b"codeql/codeql\n"
            staging = Path(argv[-1])
            _ = (staging / "partial").write_text("incomplete")
            code = "fixture_extraction_interrupted"
            raise ToolError(code)

        with (
            patch.object(tools, "ROOT", self.directory),
            patch.object(tools, "pin", return_value=fixture),
            patch.object(context, "run", side_effect=interrupted),
            self.assertRaisesRegex(ToolError, "fixture_extraction_interrupted"),
        ):
            _ = tools.install(context)
        self.assertEqual(list((self.directory / ".cache/codeql-tools").iterdir()), [])

    def test_shared_bundle_reuse_stays_outside_cargo_target_and_checks_its_receipt(self) -> None:
        """The new cache reuses authenticated bytes without trusting an old target bundle."""
        output = self.directory / "output"
        output.mkdir()
        context = Context(ROOT, output)
        digest = "a" * 64
        fixture = tools.pin()
        fixture["bundles"] = {
            target: {"url": "https://example.invalid/codeql", "bytes": 123, "sha256": digest}
            for target in ("darwin", "linux-x86_64", "linux-aarch64")
        }
        receipt = {"archiveSha256": digest, "archiveBytes": 123}
        directory = self.directory / ".cache/codeql-tools" / digest
        old_directory = self.directory / "target/codeql-tools" / digest
        for cache in (directory, old_directory):
            cache.mkdir(parents=True)
            _ = (cache / "receipt.json").write_text(json.dumps(receipt))
        with (
            patch.object(tools, "ROOT", self.directory),
            patch.object(tools, "pin", return_value=fixture),
            patch.object(context, "run") as run,
        ):
            self.assertEqual(tools.install(context), directory / "codeql/codeql")
            _ = (directory / "receipt.json").write_text(
                json.dumps({**receipt, "archiveBytes": 124})
            )
            with self.assertRaisesRegex(ToolError, "codeql_installation_receipt"):
                _ = tools.install(context)
            run.assert_not_called()

    def test_source_identity_accepts_unpushed_commit_and_rejects_dirty_source(self) -> None:
        """Bind local CI to real Git bytes without requiring publication or a pull request."""
        source = self.directory / "source"
        source.mkdir()
        output = self.directory / "output"
        output.mkdir()
        context = Context(source, output)
        _ = context.run("fixture-init", ["git", "init", "--template="])
        _ = (source / "fixture.txt").write_text("committed\n")
        _ = context.run("fixture-add", ["git", "add", "fixture.txt"])
        _ = context.run(
            "fixture-commit",
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "test: fixture",
            ],
        )
        _, output_bytes = context.run("fixture-head", ["git", "rev-parse", "HEAD"])
        revision = output_bytes.decode().strip()
        local.source_identity(context, revision)
        _ = (source / "fixture.txt").write_text("uncommitted\n")
        with self.assertRaisesRegex(ToolError, "codeql_source_not_committed"):
            local.source_identity(context, revision)


if __name__ == "__main__":
    _ = unittest.main()
