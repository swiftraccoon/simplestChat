"""Coverage, policy and privacy regressions for exact image scanner reports."""

from __future__ import annotations

import copy
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING

from test_support import ROOT

# isort: split
import security_image_policy as image
from release_json import json_value, object_value
from security_policy import ExceptionRecord
from security_tools import ToolError

if TYPE_CHECKING:
    from release_json import JsonObject

NOW = datetime(2026, 9, 30, tzinfo=UTC)


def package(name: str = "fixture", expression: str = "MIT") -> JsonObject:
    """Provide complete but inert package metadata."""
    return {
        "id": name,
        "name": name,
        "version": "1",
        "type": "rpm",
        "purl": "pkg:rpm/fedora/" + name + "@1",
        "licenses": [{"spdxExpression": expression}],
    }


def vulnerability(severity: str = "High") -> JsonObject:
    """Use an unfixed synthetic identifier; no external vulnerability reproduction."""
    return {
        "descriptor": {"name": "grype", "version": "0.120.0"},
        "matches": [
            {
                "vulnerability": {
                    "id": "CVE-fixture",
                    "severity": severity,
                    "fix": {"state": "not-fixed"},
                },
                "artifact": {"purl": "pkg:rpm/fedora/fixture@1"},
            }
        ],
        "ignoredMatches": [],
    }


class ImagePolicyTests(unittest.TestCase):
    """Scanner success does not excuse missing evidence or unreviewed findings."""

    def test_database_requires_current_version_status_age_and_timezone(self) -> None:
        """Fresh valid databases pass; future, stale and incomplete evidence does not."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        good: JsonObject = {
            "valid": True,
            "schemaVersion": "v6.1.9",
            "built": NOW.isoformat(),
            "from": "https://grype.anchore.io/databases/v6/fixture.tar.zst?checksum=sha256%3A"
            + "a" * 64,
        }
        image.database_status(good, policy, now=NOW)
        for key, value in (
            ("valid", False),
            ("schemaVersion", "v5.0.0"),
            ("built", (NOW - timedelta(days=6)).isoformat()),
            ("built", (NOW + timedelta(seconds=1)).isoformat()),
            ("built", "2026-09-30T00:00:00"),
        ):
            with self.subTest(key=key, value=value), self.assertRaises(ToolError):
                image.database_status({**good, key: value}, policy, now=NOW)

    def test_inventory_requires_supported_os_rpm_and_rust_and_unique_identities(self) -> None:
        """An empty successful scanner report cannot produce a clean verdict."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        rust = {**package("simplestChat"), "type": "rust-crate"}
        good: JsonObject = {
            "distro": {"id": "fedora", "versionID": "44"},
            "descriptor": {"name": "syft", "version": "1.54.0"},
            "artifacts": [package(), rust],
        }
        self.assertEqual(len(image.inventory(good, policy)), 2)
        for key, value in (
            ("descriptor", {"name": "syft", "version": "0.0.0"}),
            ("artifacts", []),
            ("artifacts", [package()]),
            ("artifacts", [rust]),
            ("artifacts", [package(), rust, rust]),
            ("distro", {"id": "unknown", "versionID": "44"}),
        ):
            with self.subTest(key=key, value=value), self.assertRaises((ToolError, ValueError)):
                _ = image.inventory(object_value(json_value({**good, key: value})), policy)

    def test_vulnerability_report_requires_the_installed_scanner_identity(self) -> None:
        """A successful or empty report from another scanner release is refused."""
        for descriptor in (
            {"name": "grype", "version": "0.0.0"},
            {"name": "other", "version": "0.120.0"},
        ):
            value = vulnerability("Low")
            value["descriptor"] = json_value(descriptor)
            with (
                self.subTest(descriptor=descriptor),
                self.assertRaisesRegex(ToolError, "image_vulnerability_tool_identity"),
            ):
                _ = image.vulnerability_verdict(value, [], [])

    def test_unfixed_high_critical_unknown_and_scoped_exceptions(self) -> None:
        """Never use only-fixed or ignore filters to hide unresolved high severity."""
        for severity in ("High", "Critical", "Unknown"):
            self.assertFalse(image.vulnerability_verdict(vulnerability(severity), [], [])["passed"])
        self.assertTrue(image.vulnerability_verdict(vulnerability("Low"), [], [])["passed"])
        waiver = ExceptionRecord(
            "grype",
            "CVE-fixture",
            "pkg:rpm/fedora/fixture@1",
            "owner",
            "reviewed",
            "scope",
            NOW.date(),
            "https://example.org/review/1",
        )
        self.assertTrue(image.vulnerability_verdict(vulnerability(), [waiver], [])["passed"])
        changed = copy.deepcopy(vulnerability())
        changed["ignoredMatches"] = [{}]
        with self.assertRaises(ToolError):
            _ = image.vulnerability_verdict(changed, [waiver], [])
        with self.assertRaises(ToolError):
            _ = image.vulnerability_verdict(vulnerability("made-up"), [], [])

    def test_spdx_boolean_grammar_precedence_and_invalid_tokens(self) -> None:
        """Choose an allowed OR branch while requiring every AND term and exact exceptions."""
        allowed = {"MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception"}
        for expression, expected in (
            ("MIT", True),
            ("MIT OR unknown", True),
            ("MIT AND unknown", False),
            ("MIT OR unknown AND unknown", True),
            ("(MIT OR unknown) AND Apache-2.0", True),
            ("Apache-2.0 WITH LLVM-exception", True),
            ("Apache-2.0 WITH LLVM-Exception", True),
            ("Apache-2.0 WITH unknown", False),
        ):
            with self.subTest(expression=expression):
                self.assertEqual(
                    image.LicenseExpression(expression, allowed).allowed_expression(), expected
                )
        for expression in (
            "MIT garbage",
            "MIT OR",
            "AND MIT",
            "MIT | Apache-2.0",
            "(MIT",
            "MIT)",
            "(" * 40 + "MIT" + ")" * 40,
        ):
            with self.subTest(expression=expression), self.assertRaises(ToolError):
                _ = image.LicenseExpression(expression, allowed).allowed_expression()

    def test_unknown_license_is_not_an_empty_pass(self) -> None:
        """Missing and unreviewed metadata remain findings even without vulnerability matches."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        self.assertTrue(image.license_verdict([package()], policy, [])["passed"])
        for expression in ("", "LicenseRef-Unreviewed"):
            self.assertFalse(
                image.license_verdict([package(expression=expression)], policy, [])["passed"]
            )

    def test_reports_reject_symlinks_duplicate_keys_and_size_limits(self) -> None:
        """Scanner reports are untrusted input rather than trusted process output."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "report.json"
            _ = path.write_text('{"a":1,"a":2}')
            with self.assertRaises(ValueError):
                _ = image.report(path)
            link = path.with_name("link.json")
            link.symlink_to(path)
            with self.assertRaises((ValueError, OSError)):
                _ = image.report(link)
            with self.assertRaises(ToolError):
                _ = image.report(path, limit=1)
