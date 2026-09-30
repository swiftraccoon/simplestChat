"""Exercise report failures and independent security review boundaries."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from datetime import UTC, date, datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_codeql
import security_findings as findings
import security_policy
from security_tools import ToolError


def reviewed() -> security_policy.ExceptionRecord:
    """Bind a sample advisory to just one package version."""
    return security_policy.ExceptionRecord(
        "cargo-audit",
        "RUSTSEC-2099-0001",
        "example@1.0.0",
        "maintainer",
        "Fixture only",
        "Fixture only",
        date(2099, 1, 1),
        "https://example.org/review/1",
    )


def audit_report() -> dict[str, object]:
    """Build an unfiltered, current, empty one-package scanner result."""
    return {
        "settings": {
            "target_arch": [],
            "target_os": [],
            "severity": None,
            "ignore": [],
            "informational_warnings": ["unmaintained", "unsound", "notice"],
        },
        "lockfile": {"dependency-count": 1},
        "database": {
            "last-commit": "a" * 40,
            "advisory-count": 10,
            "last-updated": datetime.now(UTC).isoformat(),
        },
        "vulnerabilities": {"list": [], "found": False, "count": 0},
        "warnings": {},
    }


class FindingTests(unittest.TestCase):
    """Findings remain blocking despite successful report generation."""

    def test_dependency_warning_requires_exact_review(self) -> None:
        """Warning-class advisories receive the same exact identity check as vulnerabilities."""
        item = {
            "package": {"name": "example", "version": "1.0.0"},
            "advisory": {"id": "RUSTSEC-2099-0001"},
        }
        report = audit_report()
        report["warnings"] = {"unmaintained": [item]}
        content = json.dumps(report).encode()
        with self.assertRaisesRegex(ToolError, "unreviewed_cargo_advisory"):
            _ = findings.cargo_findings(content, [], 1)
        self.assertEqual(findings.cargo_findings(content, [reviewed()], 1), 1)
        item["package"]["version"] = "1.0.1"
        with self.assertRaisesRegex(ToolError, "unreviewed_cargo_advisory"):
            _ = findings.cargo_findings(json.dumps(report).encode(), [reviewed()], 1)

    def test_scanner_filters_and_inconsistent_counts_fail(self) -> None:
        """An ignored advisory or incomplete lock cannot disappear into a passing report."""
        replacements: list[dict[str, object]] = [
            {"settings": {"ignore": ["RUSTSEC-2099-0001"]}},
            {"lockfile": {"dependency-count": 0}},
            {"vulnerabilities": {"list": [], "found": True, "count": 1}},
        ]
        for replacement in replacements:
            with self.subTest(replacement=replacement), self.assertRaises(ToolError):
                _ = findings.cargo_findings(
                    json.dumps({**audit_report(), **replacement}).encode(), [], 1
                )

    def test_missing_report_fields_are_not_zero_findings(self) -> None:
        """An empty object or changed scanner schema cannot become a passing gate."""
        with self.assertRaises(KeyError):
            _ = findings.cargo_findings(b"{}", [], 1)
        with self.assertRaises(ToolError):
            _ = findings.zizmor_findings(b"{}", [])

    def test_duplicate_budget_allows_reduction_but_no_new_versions(self) -> None:
        """A third copy or new duplicate group must be deliberately reviewed."""
        with tempfile.TemporaryDirectory() as temporary:
            policy = Path(temporary) / "budget.json"
            _ = policy.write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "reviewedRevision": "a" * 40,
                        "duplicates": {"example": ["1.0.0", "2.0.0"]},
                    }
                )
            )
            for versions in (["1.0.0"], ["1.0.0", "2.0.0"]):
                content = json.dumps(
                    {"packages": [{"name": "example", "version": v} for v in versions]}
                ).encode()
                findings.dependency_budget(content, policy)
            content = json.dumps(
                {
                    "packages": [
                        {"name": "example", "version": v} for v in ["1.0.0", "2.0.0", "3.0.0"]
                    ]
                }
            ).encode()
            with self.assertRaisesRegex(ToolError, "new_duplicate_dependency_version"):
                findings.dependency_budget(content, policy)

    def test_migration_review_detects_old_changes_and_returns_new_files(self) -> None:
        """Initial SQL is immutable and every subsequent migration reaches the linter."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "security").mkdir()
            (root / "migrations").mkdir()
            old = root / "migrations/001_initial.sql"
            _ = old.write_text("CREATE TABLE example (id bigint);\n")
            _ = (root / "security/migration-baseline.json").write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "reviewedRevision": "a" * 40,
                        "files": [
                            {
                                "path": "migrations/001_initial.sql",
                                "sha256": hashlib.sha256(old.read_bytes()).hexdigest(),
                            }
                        ],
                    }
                )
            )
            _ = (root / "migrations/002_next.sql").write_text("SELECT 1;\n")
            self.assertEqual(findings.new_migrations(root), ["migrations/002_next.sql"])
            for directive in (
                "-- squawk-ignore-file",
                "-- squawk-ignore prefer-bigint-over-int",
                "-- squawk-disable-assume-in-transaction",
            ):
                _ = (root / "migrations/002_next.sql").write_text(directive + "\nSELECT 1;\n")
                with (
                    self.subTest(directive=directive),
                    self.assertRaisesRegex(ToolError, "migration_inline_suppression_forbidden"),
                ):
                    _ = findings.new_migrations(root)
            _ = (root / "migrations/002_next.sql").write_text("SELECT 1;\n")
            _ = old.write_text("SELECT 2;\n")
            with self.assertRaisesRegex(ToolError, "reviewed_migration_modified"):
                _ = findings.new_migrations(root)

    def test_native_coverage_requires_each_compiled_protocol(self) -> None:
        """A successful generic C++ scan does not prove the actual worker was extracted."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "coverage.json"
            rows = [[name] for name in sorted(security_codeql.REQUIRED)]
            _ = path.write_text(json.dumps({"#select": {"tuples": rows}}))
            self.assertEqual(security_codeql.verify(path), len(rows))
            _ = rows.pop()
            _ = path.write_text(json.dumps({"#select": {"tuples": rows}}))
            with self.assertRaisesRegex(ToolError, "codeql_native_extraction_incomplete"):
                _ = security_codeql.verify(path)


if __name__ == "__main__":
    _ = unittest.main()
