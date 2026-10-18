"""Prove failed nested scans remain blocking and publish only bounded coordinates."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_check
import security_semgrep_summary as summary
from release_json import array_value, json_value
from security_context import Context
from security_tools import ToolError

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

RULE = "simplestchat.python-network-timeout"
SENTINEL = "private-source-sentinel"


class SummaryTests(unittest.TestCase):
    """Raw scanner text cannot enter the artifact even along error paths."""

    directory: Path = Path()

    @override
    def setUp(self) -> None:
        """Keep all fixture reports inside a test-owned directory."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.write("source-manifest.json", {"src/example.py": "a" * 64})

    def write(self, name: str, value: JsonValue) -> None:
        """Write inert scanner JSON, never execute fixture content."""
        path = self.directory / name
        _ = path.write_text(json.dumps(value))
        path.chmod(0o600)

    def report(self, findings: list[JsonValue]) -> JsonObject:
        """Model the nested helper's safe completed-result schema."""
        return {
            "schemaVersion": 1,
            "includeVendor": False,
            "toolLockSha256": "a" * 64,
            "engineVersion": "1.179.0",
            "rulesSha256": "b" * 64,
            "sourceManifestSha256": "c" * 64,
            "goldenAssertions": 40,
            "sourceFiles": 1,
            "findings": findings,
            "passed": not findings,
        }

    def diagnose(self, status: int, value: JsonValue) -> JsonObject:
        """Call the same boundary used by the blocking source gate."""
        return summary.diagnosis(status, json.dumps(value).encode(), self.directory)

    def test_completed_scan_retains_only_validated_bounded_coordinates(self) -> None:
        """A finding is still a failed gate; raw report fields never pass through."""
        found: JsonObject = {"rule": RULE, "path": "src/example.py", "line": 4}
        result = self.diagnose(1, self.report([found]))
        self.assertEqual(
            result,
            {"passed": False, "findingCount": 1, "findings": [found], "findingsTruncated": False},
        )
        result = self.diagnose(0, self.report([]))
        self.assertTrue(result["passed"] is True)

    def test_every_finding_is_validated_including_a_truncated_tail(self) -> None:
        """Truncation cannot hide a malformed or foreign trailing coordinate."""
        entries: list[JsonValue] = [
            {"rule": RULE, "path": "src/example.py", "line": i + 1}
            for i in range(summary.MAX_ITEMS + 1)
        ]
        result = self.diagnose(1, self.report(entries))
        self.assertEqual(result["findingCount"], summary.MAX_ITEMS + 1)
        self.assertEqual(len(array_value(result["findings"])), summary.MAX_ITEMS)
        self.assertTrue(result["findingsTruncated"] is True)
        invalid: list[JsonValue] = [
            {"rule": SENTINEL, "path": "src/example.py", "line": 1},
            {"rule": RULE, "path": "../private-source-sentinel.py", "line": 1},
            {"rule": RULE, "path": "src/example.py", "line": True},
            {"rule": RULE, "path": "src/example.py", "line": 0},
            {"rule": RULE, "path": "src/example.py", "line": 1, "message": SENTINEL},
        ]
        for extra in invalid:
            with self.subTest(extra=extra):
                result = self.diagnose(1, self.report([*entries, extra]))
                self.assertEqual(result, {"passed": False, "error": "semgrep_invalid_result"})
                self.assertNotIn(SENTINEL, json.dumps(result))

    def test_fixed_errors_distinguish_failures_without_echoing_unknown_text(self) -> None:
        """Only explicitly allowlisted helper codes may reach public artifacts."""
        for code in ["semgrep_engine_errors", "semgrep_fixpoint_timeout", SENTINEL]:
            result = self.diagnose(1, {"passed": False, "error": code})
            self.assertEqual(result["error"], code if code != SENTINEL else "semgrep_check_failed")
            self.assertNotIn(SENTINEL, json.dumps(result))

    def test_engine_errors_expose_only_known_kinds_and_inventoried_coordinates(self) -> None:
        """Private messages, absolute paths, unknown kinds and source are discarded."""
        self.write(
            "04-source-engine.stdout",
            {
                "errors": [
                    {
                        "error_type": "Timeout",
                        "message": SENTINEL,
                        "location": {
                            "path": "src/example.py",
                            "start": {"line": 12, "text": SENTINEL},
                        },
                    },
                    {
                        "error_type": SENTINEL,
                        "message": SENTINEL,
                        "location": {"path": "/private-source-sentinel", "start": {"line": 4}},
                    },
                ]
            },
        )
        result = self.diagnose(1, {"passed": False, "error": "semgrep_engine_errors"})
        self.assertEqual(
            result["engineErrors"],
            [{"kind": "Timeout", "path": "src/example.py", "line": 12}, {"kind": "other"}],
        )
        self.assertNotIn(SENTINEL, json.dumps(result))

    def test_engine_error_list_is_bounded(self) -> None:
        """A failed parser cannot expand the public summary without a fixed cap."""
        self.write(
            "04-source-engine.stdout", json_value({"errors": [{"error_type": "ParseError"}] * 100})
        )
        result = self.diagnose(1, {"passed": False, "error": "semgrep_engine_errors"})
        self.assertEqual(len(array_value(result["engineErrors"])), summary.MAX_ITEMS)
        self.assertTrue(result["engineErrorsTruncated"] is True)

    def test_pinned_core_partial_parsing_variant_discards_its_payload(self) -> None:
        """Match the actual standalone 1.179.0 core syntax-error envelope."""
        # Captured by the inert scanner-only syntax fixture retained under
        # results/three-items-20261018/semgrep-core-schema/01-source-engine.stdout.
        self.write(
            "04-source-engine.stdout",
            {
                "errors": [
                    {
                        "error_type": ["PartialParsing", [{"path": SENTINEL, "text": SENTINEL}]],
                        "severity": "warn",
                        "message": SENTINEL,
                        "location": {
                            "path": "src/example.py",
                            "start": {"line": 1, "col": 1, "offset": 0},
                            "end": {"line": 1, "col": 17, "offset": 16},
                        },
                    }
                ]
            },
        )
        result = self.diagnose(1, {"passed": False, "error": "semgrep_engine_errors"})
        self.assertEqual(
            result["engineErrors"],
            [{"kind": "PartialParsing", "path": "src/example.py", "line": 1}],
        )
        self.assertNotIn(SENTINEL, json.dumps(result))

    def test_fixpoint_metadata_exposes_only_known_path_and_rule(self) -> None:
        """Ignore metadata layout and private text while retaining exact allowlisted strings."""
        self.write(
            "04-source-engine.stdout",
            {
                "errors": [],
                "time": {
                    "fixpoint_timeouts": [
                        ["src/example.py", RULE, SENTINEL],
                        {"path": "src/example.py", "rule": RULE, "function": SENTINEL},
                        {"path": "../private-source-sentinel.py", "rule": SENTINEL},
                    ]
                },
            },
        )
        result = self.diagnose(1, {"passed": False, "error": "semgrep_fixpoint_timeout"})
        self.assertEqual(
            result["fixpointTimeouts"],
            [
                {"path": "src/example.py", "rule": RULE},
                {"path": "src/example.py", "rule": RULE},
                {},
            ],
        )
        self.assertNotIn(SENTINEL, json.dumps(result))

    def test_fixpoint_metadata_is_bounded_and_ambiguous_paths_are_omitted(self) -> None:
        """Bound entries and recursive inspection; never select an ambiguous filename."""
        self.write("source-manifest.json", {"src/example.py": "a" * 64, "src/other.py": "b" * 64})
        self.write(
            "04-source-engine.stdout",
            json_value(
                {
                    "errors": [],
                    "time": {"fixpoint_timeouts": [["src/example.py", "src/other.py", RULE]] * 100},
                }
            ),
        )
        result = self.diagnose(1, {"passed": False, "error": "semgrep_fixpoint_timeout"})
        entries = array_value(result["fixpointTimeouts"])
        self.assertEqual(len(entries), summary.MAX_ITEMS)
        self.assertTrue(result["fixpointTimeoutsTruncated"] is True)
        self.assertEqual(entries[0], {"rule": RULE})

    def test_malformed_or_inconsistent_results_fail_closed(self) -> None:
        """Exit codes, envelope truth and completeness must agree."""
        finding: JsonObject = {"rule": RULE, "path": "src/example.py", "line": 4}
        cases: list[tuple[int, JsonValue]] = [
            (1, self.report([])),
            (0, self.report([finding])),
            (0, {"passed": True}),
            (0, {"passed": False, "error": SENTINEL}),
            (1, {"passed": False, "error": SENTINEL, "message": SENTINEL}),
        ]
        for status, value in cases:
            with self.subTest(status=status, value=value):
                self.assertEqual(
                    self.diagnose(status, value),
                    {"passed": False, "error": "semgrep_invalid_result"},
                )
        for raw in [b"", b"not-json-private-source-sentinel", b'{"passed":true,"passed":false}']:
            self.assertEqual(
                summary.diagnosis(0, raw, self.directory),
                {"passed": False, "error": "semgrep_invalid_result"},
            )

    def test_gate_records_diagnosis_before_rejecting_failed_nested_scan(self) -> None:
        """Accepting exit one for parsing must never let migrations or later checks run."""
        (self.directory / "outer").mkdir()
        context = Context(ROOT, self.directory / "outer")
        raw = json.dumps({"passed": False, "error": "semgrep_engine_errors"}).encode()
        with (
            patch.object(Context, "run", return_value=(1, raw)) as run,
            self.assertRaisesRegex(ToolError, "semgrep_source_check_failed"),
        ):
            security_check.semgrep_checks(context)
        self.assertEqual(run.call_args.kwargs["accepted"], (0, 1))
        self.assertEqual(len(context.checks), 1)
        self.assertEqual(context.checks[0]["name"], "semgrep-diagnosis")
        self.assertTrue(context.checks[0]["passed"] is False)
