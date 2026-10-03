"""Exercise real workflow inventory and local CI completion receipts offline."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import override

from test_support import obj, string, strings, yaml_value

ROOT = Path(__file__).resolve().parents[3]
CHECKS = {
    "browser-accounts",
    "browser-media",
    "browser-stress",
    "security-fast",
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-actions",
    "codeql-javascript-typescript",
    "codeql-python",
    "codeql-rust",
    "codeql-c-cpp",
}


class LocalCiEvidenceTests(unittest.TestCase):
    """Prevent dry runs, filtered matrices or stale results from claiming full CI."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize typed fixture state before unittest calls setUp."""
        super().__init__(method_name)
        self.output: Path = ROOT
        self.event: Path = ROOT
        self.identity: dict[str, str] = {}
        self.needs: dict[str, dict[str, str]] = {}
        self.environment: dict[str, str] = {}

    @override
    def setUp(self) -> None:
        """Keep every fixture under the ignored repository results directory."""
        (ROOT / "results").mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-receipts.", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.output = Path(temporary.name)
        self.event = self.output / "event.json"
        _ = self.event.write_text(json.dumps({"before": "b" * 40}))
        self.identity = {"runId": "1" * 32, "revision": "a" * 40, "base": "b" * 40}
        workflow = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True)
        )
        needs = strings(workflow, "jobs", "required", "needs")
        self.needs = {name: {"result": "success"} for name in needs}
        self.environment = {
            **os.environ,
            "ACT": "true",
            "LOCAL_CI_DISPOSABLE": "1",
            "LOCAL_CI_EVIDENCE": str(self.output),
            "LOCAL_CI_RUN_ID": self.identity["runId"],
            "GITHUB_SHA": self.identity["revision"],
            "GITHUB_EVENT_PATH": str(self.event),
            "GATE_RESULTS": json.dumps(self.needs),
        }

    def run_helper(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        """Run the actual helper, with no engine, scanner or network command."""
        return subprocess.run(  # noqa: S603 -- Fixed owned helper and test arguments.
            [sys.executable, str(ROOT / "build/ci-local-evidence.py"), *arguments],
            env=self.environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )

    def populate(self) -> None:
        """Provide one success record for each independent hosted coverage contract."""
        directory = self.output / "receipts"
        directory.mkdir()
        for identifier in CHECKS:
            _ = (directory / f"{identifier}.json").write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "status": "passed",
                        "check": identifier,
                        **self.identity,
                    }
                )
            )

    def test_actual_workflow_and_all_receipts_complete_once(self) -> None:
        """The current workflow shape and shared language/native contracts must agree."""
        self.populate()
        result = self.run_helper("complete")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = obj(yaml_value((self.output / "required.json").read_text()))
        self.assertEqual(report["checks"], sorted(CHECKS))
        self.assertEqual(report["gates"], sorted(self.needs))
        self.assertEqual(set(obj(report, "workflows")), {"ci.yml", "security.yml", "codeql.yml"})
        self.assertNotEqual(self.run_helper("complete").returncode, 0)

    def test_matrix_limits_are_numeric_and_cover_each_declared_entry(self) -> None:
        """Act parses literal limits with Atoi and otherwise silently serializes matrices."""
        for name, job, dimension, limit in (
            ("ci.yml", "browser", "group", 3),
            ("security.yml", "native-security", "mode", 3),
            ("codeql.yml", "source-analysis", "language", 4),
        ):
            workflow = obj(
                yaml_value((ROOT / ".github/workflows" / name).read_text(), scalars_as_strings=True)
            )
            strategy = obj(workflow, "jobs", job, "strategy")
            self.assertEqual(string(strategy, "max-parallel"), str(limit))
            self.assertEqual(len(strings(strategy, "matrix", dimension)), limit)

    def test_missing_or_extra_matrix_receipts_fail(self) -> None:
        """Filtering any supported matrix family cannot satisfy the final aggregate."""
        self.populate()
        for identifier in ("browser-media", "native-replay", "codeql-rust", "security-fast"):
            path = self.output / "receipts" / f"{identifier}.json"
            original = path.read_bytes()
            path.unlink()
            self.assertNotEqual(self.run_helper("complete").returncode, 0)
            _ = path.write_bytes(original)
        _ = (self.output / "receipts/unknown.json").write_text("{}")
        self.assertNotEqual(self.run_helper("complete").returncode, 0)
        self.assertFalse((self.output / "required.json").exists())

    def test_failed_or_skipped_gates_fail_despite_complete_matrix_receipts(self) -> None:
        """A matrix success cannot override a failed or unexecuted top-level gate."""
        self.populate()
        for status in ("failure", "skipped", "cancelled"):
            self.needs["deployment"]["result"] = status
            self.environment["GATE_RESULTS"] = json.dumps(self.needs)
            self.assertNotEqual(self.run_helper("complete").returncode, 0)

    def test_stale_identity_and_duplicate_receipts_are_rejected(self) -> None:
        """A previous run, source or base cannot authorize the current local gate."""
        self.populate()
        for key in ("runId", "revision", "base"):
            self.identity[key] = "f" * len(self.identity[key])
            path = self.output / "receipts/browser-media.json"
            _ = path.write_text(
                json.dumps(
                    {"schema": 1, "status": "passed", "check": "browser-media", **self.identity}
                )
            )
            self.assertNotEqual(self.run_helper("complete").returncode, 0)
        self.assertNotEqual(self.run_helper("record", "--id", "browser-media").returncode, 0)

    def test_successful_record_is_bound_to_the_current_run(self) -> None:
        """Record writes the exact caller identity and cannot be rewritten."""
        result = self.run_helper("record", "--id", "browser-media")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = obj(yaml_value((self.output / "receipts/browser-media.json").read_text()))
        self.assertEqual(
            report, {"schema": 1, "status": "passed", "check": "browser-media", **self.identity}
        )
        self.assertNotEqual(self.run_helper("record", "--id", "browser-media").returncode, 0)
