"""Finite mutation orchestration and result interpretation, without application services."""

from __future__ import annotations

import copy
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import security_mutation as mutation
from release_json import JsonObject, JsonValue, object_value
from security_context import Context
from security_tools import ToolError

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

POLICY = mutation.Policy(
    "27.1.0", 4, 2, 60, 10, 120, {"src/labels.rs": ("is_reserved_name",)}, ("labels::tests::",)
)


def mutant(name: str) -> JsonObject:
    """Use the pinned tool's real JSON record fields, with an inert boolean replacement."""
    return {
        "name": name,
        "file": "src/labels.rs",
        "package": "fixture",
        "function": {"function_name": "is_reserved_name"},
        "span": {"start": {"line": 1, "column": 1}, "end": {"line": 1, "column": 2}},
        "replacement": "true",
        "genre": "FnValue",
    }


def fixture(directory: Path) -> tuple[dict[str, JsonObject], JsonObject]:
    """Build a passing baseline and two test-killed mutants with per-scenario logs."""
    (directory / "log").mkdir()
    expected = {name: mutant(name) for name in ("first", "second")}
    success: JsonObject = {
        "phase": "Build",
        "process_status": "Success",
        "duration": 0.1,
        "argv": [],
    }
    baseline: JsonObject = {
        "scenario": "Baseline",
        "summary": "Success",
        "log_path": "log/baseline.log",
        "phase_results": [success, {**success, "phase": "Test"}],
    }
    rows: list[JsonValue] = [baseline]
    _ = (directory / "log/baseline.log").write_text(
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out\n"
    )
    for name, item in expected.items():
        rows.append(
            {
                "scenario": {"Mutant": item},
                "summary": "CaughtMutant",
                "log_path": f"log/{name}.log",
                "phase_results": [
                    success,
                    {
                        "phase": "Test",
                        "process_status": {"Failure": 101},
                        "duration": 0.1,
                        "argv": [],
                    },
                ],
            }
        )
        _ = (directory / f"log/{name}.log").write_text(
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 20 filtered out\n"
        )
    report: JsonObject = {
        "outcomes": rows,
        "total_mutants": 2,
        "caught": 2,
        "missed": 0,
        "timeout": 0,
        "unviable": 0,
        "success": 0,
        "start_time": "2026-09-30T00:00:00Z",
        "end_time": "2026-09-30T00:01:00Z",
        "cargo_mutants_version": "27.1.0",
    }
    save(directory, report)
    return expected, report


def save(directory: Path, value: JsonObject) -> None:
    """Write one synthetic tool output without changing source fixtures."""
    _ = (directory / "outcomes.json").write_text(json.dumps(value))


class MutationPolicyTests(unittest.TestCase):
    """Selections and resource ceilings are explicit and cannot silently shrink coverage."""

    def test_current_policy_selects_real_pure_functions_and_existing_matrix(self) -> None:
        """Exercise production policies against existing independent test oracles."""
        policy = mutation.policy_at(mutation.POLICY)
        self.assertEqual(policy.maximum, 64)
        self.assertEqual(
            set(policy.files),
            {"src/room/roles.rs", "src/labels.rs", "src/auth/common_passwords.rs"},
        )
        self.assertIn("Role::can_set_role", policy.files["src/room/roles.rs"])
        self.assertTrue(any("expected_matrix" in name for name in policy.filters))

    def test_policy_rejects_unbounded_limits_and_option_shaped_test_filters(self) -> None:
        """Reject malformed selection and ceilings before starting compiler processes."""
        original = mutation.read(mutation.POLICY)
        cases: tuple[tuple[str, JsonValue], ...] = (
            ("maximumMutants", 0),
            ("maximumMutants", 10000),
            ("totalTimeoutSeconds", 0),
            ("testFilters", ["--ignored"]),
            ("sourceFiles", {"../foreign.rs": ["function"]}),
            ("sourceFiles", {}),
        )
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "policy.json"
            for name, value in cases:
                with self.subTest(name=name, value=value):
                    _ = path.write_text(json.dumps({**original, name: value}))
                    with self.assertRaises((ToolError, ValueError)):
                        _ = mutation.policy_at(path)

    def test_commands_use_private_in_place_target_with_locked_offline_serial_tests(self) -> None:
        """Pass all filters through Cargo to libtest, with bounded serial mutation."""
        source, output = Path("/private/source"), Path("/private/evidence")
        argv = mutation.command(
            Path("/verified/cargo-mutants"), source, output, POLICY, listing=False
        )
        for required in (
            "--in-place",
            "--baseline=run",
            "--cap-lints=true",
            "--no-config",
            "--no-shuffle",
            "--jobserver-tasks=2",
            "-C=--locked",
            "-C=--offline",
            "-C=--lib",
            "--build-timeout=60",
            "--timeout=10",
            "--test-threads=1",
        ):
            self.assertIn(required, argv)
        self.assertEqual(argv[argv.index("--dir") + 1], str(source))
        self.assertNotIn("--jobs", argv)  # Tool rejects jobs with its serial in-place mode.
        self.assertNotIn("--baseline=skip", argv)
        self.assertEqual(
            argv[argv.index("--") :], ["--", "--", *POLICY.filters, "--test-threads=1"]
        )
        listing = mutation.command(
            Path("/verified/cargo-mutants"), source, output, POLICY, listing=True
        )
        self.assertIn("--list", listing)
        self.assertNotIn("--in-place", listing)

    def test_inventory_rejects_empty_missing_duplicate_foreign_and_excess_mutants(self) -> None:
        """Refuse truncated, repeated or out-of-scope mutation inventories."""
        valid = mutant("one")
        self.assertEqual(set(mutation.mutant_inventory([valid], POLICY)), {"one"})
        cases: list[JsonValue] = [
            [],
            [valid, valid],
            [{**valid, "file": "src/other.rs"}],
            [{**valid, "function": {"function_name": "another"}}],
            [mutant(str(index)) for index in range(5)],
        ]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ToolError):
                _ = mutation.mutant_inventory(value, POLICY)

    def test_real_cli_help_works_without_test_import_paths(self) -> None:
        """Check the maintained entry point can import its shared helpers directly."""
        status, output, error = bounded_process.run(
            [sys.executable, str(ROOT / "build/security_mutation.py"), "--help"],
            limits=bounded_process.Limits(timeout=5),
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
        )
        self.assertEqual(status, 0, error.decode())
        self.assertIn(b"--openssl-prefix", output)


class MutationVerdictTests(unittest.TestCase):
    """A score needs actual baseline tests and complete assertion-based mutant outcomes."""

    def test_complete_test_killed_inventory_passes(self) -> None:
        """Require a passing baseline and assertion failures for every selected mutant."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            expected, _ = fixture(root)
            result = mutation.assess(root, expected, POLICY)
            self.assertTrue(result["passed"])
            self.assertEqual(result["total"], 2)
            self.assertEqual(result["baselineTests"], 2)

    def test_missed_timeout_or_unviable_is_failure_even_with_tool_success(self) -> None:
        """Keep uncovered or invalid mutations visible as failed quality checks."""
        for summary, counter in (
            ("MissedMutant", "missed"),
            ("Timeout", "timeout"),
            ("Unviable", "unviable"),
        ):
            with self.subTest(summary=summary), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                expected, report = fixture(root)
                rows = report["outcomes"]
                assert isinstance(rows, list)  # noqa: S101 -- Fixture type narrowing.
                object_value(rows[1])["summary"] = summary
                report["caught"], report[counter] = 1, 1
                save(root, report)
                self.assertFalse(mutation.assess(root, expected, POLICY)["passed"])

    def test_missing_duplicate_changed_or_incomplete_results_cannot_pass(self) -> None:
        """Bind every completed outcome to exactly one enumerated source mutation."""
        for mode in ("missing", "duplicate", "changed", "no-end", "wrong-version", "totals"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                expected, report = fixture(root)
                rows = report["outcomes"]
                assert isinstance(rows, list)  # noqa: S101 -- Fixture type narrowing.
                if mode == "missing":
                    _ = rows.pop()
                elif mode == "duplicate":
                    rows.append(copy.deepcopy(rows[-1]))
                elif mode == "changed":
                    object_value(object_value(object_value(rows[1])["scenario"])["Mutant"])[
                        "replacement"
                    ] = "false"
                    expected = {name: mutant(name) for name in expected}
                elif mode == "no-end":
                    report["end_time"] = None
                elif mode == "wrong-version":
                    report["cargo_mutants_version"] = "unreviewed"
                else:
                    report["caught"] = 0
                save(root, report)
                with self.assertRaises((ToolError, ValueError)):
                    _ = mutation.assess(root, expected, POLICY)

    def test_zero_ignored_or_failed_baseline_tests_are_not_coverage(self) -> None:
        """Reject filters that run no tests or leave required tests ignored."""
        for text in (
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out",
            "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out",
            "error: compiler failed before tests",
        ):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                expected, _ = fixture(root)
                _ = (root / "log/baseline.log").write_text(text)
                with self.assertRaises(ToolError):
                    _ = mutation.assess(root, expected, POLICY)

    def test_crash_or_build_failure_does_not_count_as_test_killed(self) -> None:
        """Distinguish test assertions from compiler failures and process crashes."""
        for mode in ("oom", "signal", "compile"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                expected, report = fixture(root)
                rows = report["outcomes"]
                assert isinstance(rows, list)  # noqa: S101 -- Fixture type narrowing.
                phases = object_value(rows[1])["phase_results"]
                assert isinstance(phases, list)  # noqa: S101 -- Fixture type narrowing.
                if mode == "compile":
                    object_value(phases[0])["process_status"] = {"Failure": 101}
                elif mode == "signal":
                    object_value(phases[1])["process_status"] = {"Signalled": 9}
                else:
                    _ = (root / "log/first.log").write_text("killed by out-of-memory supervisor")
                save(root, report)
                with self.assertRaises(ToolError):
                    _ = mutation.assess(root, expected, POLICY)

    def test_report_cannot_read_outside_its_owned_log_directory(self) -> None:
        """Refuse report paths that escape the exact owned evidence directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            expected, report = fixture(root)
            rows = report["outcomes"]
            assert isinstance(rows, list)  # noqa: S101 -- Fixture type narrowing.
            object_value(rows[0])["log_path"] = "../outside"
            save(root, report)
            with self.assertRaises(ToolError):
                _ = mutation.assess(root, expected, POLICY)

    def test_disk_budget_bounds_both_reports_and_private_target(self) -> None:
        """Stop compilation or report growth above independent disk ceilings."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "source", root / "output"
            source.mkdir()
            output.mkdir()
            _ = (source / "file").write_bytes(b"0123456789")
            with patch.object(mutation, "MAX_WORKSPACE", 5):
                self.assertFalse(mutation.DiskBudget(source, output).healthy())
            _ = (output / "log").write_bytes(b"0123456789")
            with patch.object(mutation, "MAX_EVIDENCE", 5):
                self.assertFalse(mutation.DiskBudget(source, output).healthy())

    def test_final_disk_scan_rejects_growth_after_cached_success(self) -> None:
        """A final burst must not borrow an earlier successful periodic budget check."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "source", root / "output"
            source.mkdir()
            output.mkdir()
            budget = mutation.DiskBudget(source, output)
            with patch.object(mutation, "MAX_EVIDENCE", 5):
                self.assertTrue(budget.healthy())
                _ = (output / "final-burst").write_bytes(b"0123456789")
                with patch.object(time, "monotonic", return_value=budget.checked + 0.1):
                    self.assertTrue(budget.healthy())
                    self.assertFalse(budget.healthy(force=True))

    def test_disk_inventory_bounds_empty_directories(self) -> None:
        """Zero-byte entries still consume bounded traversal work."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "source", root / "output"
            source.mkdir()
            output.mkdir()
            (source / "one/two/three").mkdir(parents=True)
            with patch.object(mutation, "MAX_WORKSPACE_ENTRIES", 2):
                self.assertFalse(mutation.DiskBudget(source, output).healthy(force=True))

    def test_successful_child_cannot_publish_a_stale_disk_verdict(self) -> None:
        """Verify the actual process wrapper checks final bytes despite cached periodic health."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "source", root / "output"
            source.mkdir()
            output.mkdir()
            reports = output / "results"
            reports.mkdir()
            context = Context(source, output)
            budget = mutation.DiskBudget(source, reports)
            self.assertTrue(budget.healthy())
            budget.checked += 60  # Deterministically retain the earlier periodic cache.
            argv = [
                sys.executable,
                "-c",
                "from pathlib import Path; import sys; "
                + "Path(sys.argv[1]).write_bytes(b'0123456789')",
                str(reports / "burst"),
            ]
            with (
                patch.object(mutation, "DiskBudget", return_value=budget),
                patch.object(mutation, "MAX_EVIDENCE", 5),
                self.assertRaisesRegex(ToolError, "mutation_disk_budget"),
            ):
                _ = mutation.execute_mutants(context, argv, source, 5)
            receipt = mutation.read(output / "budget.json")
            self.assertFalse(receipt["passed"])
            self.assertEqual(receipt["failure"], "evidence_byte_limit")
            self.assertEqual(receipt["evidenceBytes"], 10)


class FixtureContext(Context):
    """Use the real Git source copier while substituting finite tool enumeration."""

    @override
    def run(
        self,
        name: str,
        argv: Sequence[str],
        *,
        cwd: Path | None = None,
        timeout: int = 180,
        accepted: tuple[int, ...] = (0,),
        input_data: bytes = b"",
        env_updates: Mapping[str, str] | None = None,
    ) -> tuple[int, bytes]:
        """Keep source acquisition real and replace only the external mutation process."""
        if name == "mutation-inventory":
            return 0, json.dumps([mutant("first"), mutant("second")]).encode()
        return super().run(
            name,
            argv,
            cwd=cwd,
            timeout=timeout,
            accepted=accepted,
            input_data=input_data,
            env_updates=env_updates,
        )


class MutationIsolationTests(unittest.TestCase):
    """Process failures cannot change the checkout or turn incomplete runs into passes."""

    def test_private_snapshot_cleanup_preserves_original_on_all_verdicts(self) -> None:
        """Mutate a real copied Git tree and verify source preservation and owned cleanup."""
        for mode in ("pass", "tool-failure", "exception"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                root, output = directory / "repo", directory / "evidence"
                root.mkdir()
                status, _, error = bounded_process.run(
                    ["git", "init", "--quiet", str(root)],
                    limits=bounded_process.Limits(timeout=5),
                    env={"PATH": os.environ["PATH"], "LC_ALL": "C"},
                )
                self.assertEqual(status, 0, error.decode())
                original = root / "src/labels.rs"
                original.parent.mkdir()
                _ = original.write_text("original source\n")
                (root / "build").mkdir()
                _ = (root / "build/security-tools.lock.json").write_text("{}")
                tool = directory / "tool"
                _ = tool.write_text("inert verified tool fixture")

                def execute(
                    context: Context,
                    argv: Sequence[str],
                    source: Path,
                    timeout: int,
                    mode: str = mode,
                ) -> int:
                    self.assertNotEqual(source, context.root)
                    self.assertEqual(source, context.output / "source")
                    self.assertEqual(
                        (source / "src/labels.rs").read_text(),
                        (context.root / "src/labels.rs").read_text(),
                    )
                    self.assertIn(str(source), argv)
                    self.assertGreater(timeout, 0)
                    _ = (source / "src/labels.rs").write_text("mutation in owned copy only\n")
                    if mode == "exception":
                        raise OSError
                    reports = context.output / "results/mutants.out"
                    reports.mkdir(parents=True)
                    _ = fixture(reports)
                    return 1 if mode == "tool-failure" else 0

                platform = "darwin-x86_64" if sys.platform == "darwin" else "linux-x86_64"
                with (
                    patch.object(mutation, "ROOT", root),
                    patch.object(mutation, "Context", FixtureContext),
                    patch.object(mutation, "policy_at", return_value=POLICY),
                    patch.object(mutation, "tool_path", return_value=tool),
                    patch.object(mutation, "cargo_environment"),
                    patch.object(mutation, "execute_mutants", side_effect=execute),
                ):
                    passed = mutation.run(mutation.Options(output=output, tool_platform=platform))
                self.assertEqual(passed, mode == "pass")
                self.assertEqual(original.read_text(), "original source\n")
                self.assertFalse((output / "source").exists())
                self.assertTrue((output / "source-manifest.json").is_file())
                self.assertEqual(mutation.read(output / "outcome.json")["passed"], passed)
