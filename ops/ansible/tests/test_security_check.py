"""Audit startup failures remain visible and cannot weaken dependency policy."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_check
from security_context import Context
from security_tools import ToolError
from test_security_findings import audit_report


class CargoAuditTests(unittest.TestCase):
    """Exercise the report boundary before any later dependency scanner can run."""

    def test_missing_or_malformed_report_has_a_fixed_failure_code(self) -> None:
        """Git/cache failure exit one and empty stdout cannot become zero advisories."""
        for status in (0, 1):
            for output in (b"", b"not a JSON report", b"\xff"):
                with (
                    self.subTest(status=status, output=output),
                    tempfile.TemporaryDirectory() as tmp,
                ):
                    root = Path(tmp)
                    _ = (root / "Cargo.lock").write_text(
                        'version = 4\n[[package]]\nname = "example"\nversion = "1.0.0"\n'
                    )
                    context = Context(root, root)
                    context.checks.append({"name": "cargo-audit", "exitStatus": status})
                    with (
                        patch.object(
                            security_check, "tool_path", return_value=root / "cargo-audit"
                        ),
                        patch.object(Context, "run", return_value=(status, output)) as run,
                        self.assertRaisesRegex(ToolError, "cargo_audit_invalid_report"),
                    ):
                        security_check.dependency_checks(context, root, [])
                    run.assert_called_once()
                    arguments = cast("list[str]", run.call_args.args[1])
                    self.assertEqual(
                        arguments[arguments.index("--db") + 1],
                        str(root / ".cache/security-rustsec-db"),
                    )

    def test_valid_audit_findings_and_unsuccessful_empty_reports_still_fail(self) -> None:
        """Improving initialization diagnostics grants no vulnerability or exit-status waiver."""
        report = audit_report()
        finding = {
            "package": {"name": "example", "version": "1.0.0"},
            "advisory": {"id": "RUSTSEC-2099-0001"},
        }
        for warnings, expected in (
            ({"unmaintained": [finding]}, "unreviewed_cargo_advisory"),
            ({}, "cargo_unsuccessful_empty_report"),
        ):
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                _ = (root / "Cargo.lock").write_text(
                    'version = 4\n[[package]]\nname = "example"\nversion = "1.0.0"\n'
                )
                context = Context(root, root)
                context.checks.append({"name": "cargo-audit", "exitStatus": 1})
                with (
                    patch.object(security_check, "tool_path", return_value=root / "cargo-audit"),
                    patch.object(
                        Context,
                        "run",
                        return_value=(1, json.dumps({**report, "warnings": warnings}).encode()),
                    ) as run,
                    self.assertRaisesRegex(ToolError, expected),
                ):
                    security_check.dependency_checks(context, root, [])
                run.assert_called_once()


if __name__ == "__main__":
    _ = unittest.main()
