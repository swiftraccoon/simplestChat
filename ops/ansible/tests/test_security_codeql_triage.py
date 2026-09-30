"""Exercise CodeQL policy, API completeness and exact authorized mutation boundaries offline."""

from __future__ import annotations

import copy
import hashlib
import io
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr
from datetime import date
from pathlib import Path
from typing import cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import security_codeql_triage as triage
import security_policy
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value
from security_tools import ToolError

REVISION = "a" * 40
REFERENCE = "refs/heads/main"
HASH = "b" * 64
TODAY = date(2026, 9, 30)
INITIAL_ALERT_COUNT = 69


def alert() -> JsonObject:
    """Use one inert High finding whose API message matches SARIF's rendered message."""
    return {
        "number": 1,
        "state": "open",
        "tool": {"name": "CodeQL", "version": "2.27.1"},
        "rule": {"id": "py/fixture", "security_severity_level": "high", "tags": ["security"]},
        "most_recent_instance": {
            "ref": REFERENCE,
            "commit_sha": REVISION,
            "state": "open",
            "category": "/language:python/security",
            "location": {
                "path": "fixture.py",
                "start_line": 1,
                "end_line": 1,
                "start_column": 1,
                "end_column": 5,
            },
            "message": {"text": "Test source value."},
        },
    }


def sarif() -> JsonObject:
    """Represent actual CodeQL extension-owned rule metadata, not a severity-only report."""
    return {
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {"name": "CodeQL", "semanticVersion": "2.27.1"},
                    "extensions": [
                        {
                            "name": "codeql/python-queries",
                            "rules": [
                                {
                                    "id": "py/fixture",
                                    "properties": {
                                        "security-severity": "7.5",
                                        "tags": ["security"],
                                    },
                                }
                            ],
                        }
                    ],
                },
                "invocations": [{"executionSuccessful": True}],
                "results": [
                    {
                        "ruleId": "py/fixture",
                        "message": {"text": "Test [source value](1)."},
                        "locations": [
                            {
                                "physicalLocation": {
                                    "artifactLocation": {"uri": "fixture.py"},
                                    "region": {
                                        "startLine": 1,
                                        "endLine": 1,
                                        "startColumn": 1,
                                        "endColumn": 5,
                                    },
                                }
                            }
                        ],
                    }
                ],
            }
        ],
    }


def run_record(value: JsonObject) -> JsonObject:
    """Access the single deterministic run while preserving strict JSON types."""
    return object_value(array_value(value["runs"])[0])


def review(finding: JsonObject) -> security_policy.ExceptionRecord:
    """Construct a complete exact record; this is not a path or query exclusion."""
    return security_policy.ExceptionRecord(
        scanner="codeql",
        fingerprint=str(finding["fingerprint"]),
        scope=str(finding["scope"]),
        owner="fixture",
        rationale="Only synthetic fixture",
        reachability="No external input",
        expires=TODAY,
        review="https://example.test/review/fixture",
    )


class FixtureGithub(triage.Github):
    """Record every API operation without credentials, network or remote mutation."""

    def __init__(self) -> None:
        """Supply five successful current analyses and one current alert."""
        with patch.object(shutil, "which", return_value="/fixture/gh"):
            super().__init__("owner/repository")
        self.analyses: list[JsonObject] = [
            {
                "ref": REFERENCE,
                "commit_sha": REVISION,
                "error": "",
                "warning": "",
                "rules_count": 1,
                "tool": {"name": "CodeQL"},
                "category": f"/language:{language}/security",
            }
            for language in sorted(triage.LANGUAGES)
        ]
        self.alerts: list[JsonObject] = [alert()]
        self.calls: list[tuple[str, JsonObject | None]] = []
        self.changed_at_read: bool = False

    @override
    def pages(self, resource: str, reference: str) -> list[JsonObject]:
        """Return independent snapshots as the real JSON boundary does."""
        if reference != REFERENCE:
            raise AssertionError(reference)
        return copy.deepcopy(self.analyses if resource == "analyses" else self.alerts)

    @override
    def request(self, suffix: str, body: JsonObject | None = None) -> JsonValue:
        """Reproduce an alert changing between planning and the immediate pre-PATCH read."""
        self.calls.append((suffix, copy.deepcopy(body)))
        selected = copy.deepcopy(self.alerts[0])
        if body is not None:
            selected.update(body)
        elif self.changed_at_read:
            object_value(selected["rule"])["id"] = "py/different"
        return selected


class CodeqlTriageTests(unittest.TestCase):
    """A complete analysis and exact current review are required for every approval."""

    def test_api_and_sarif_agree_on_source_bound_fingerprint(self) -> None:
        """GitHub's rendered related-location labels preserve one exact policy identity."""
        with patch.object(triage, "source_hash", return_value=HASH):
            api = triage.api_finding(alert(), ROOT, REVISION, REFERENCE)
            local = triage.sarif_findings(sarif(), ROOT, REVISION)[0]
            self.assertEqual(api["fingerprint"], local["fingerprint"])
            self.assertFalse(triage.verdict([local], [])["passed"])
            self.assertTrue(triage.verdict([local], [review(api)])["passed"])

    def test_source_query_location_message_and_tool_changes_require_review(self) -> None:
        """An exception cannot float to modified source or a similar result."""
        with patch.object(triage, "source_hash", return_value=HASH):
            original = triage.api_finding(alert(), ROOT, REVISION, REFERENCE)
            changes = [alert() for _ in range(4)]
            object_value(changes[0]["rule"])["id"] = "py/other"
            object_value(changes[1]["tool"])["version"] = "2.28.0"
            object_value(object_value(changes[2]["most_recent_instance"])["location"])[
                "start_column"
            ] = 2
            object_value(changes[3]["most_recent_instance"])["message"] = {
                "text": "Different path."
            }
            for changed in changes:
                item = triage.api_finding(changed, ROOT, REVISION, REFERENCE)
                self.assertFalse(triage.verdict([item], [review(original)])["passed"])
        with patch.object(triage, "source_hash", return_value="c" * 64):
            item = triage.api_finding(alert(), ROOT, REVISION, REFERENCE)
            self.assertFalse(triage.verdict([item], [review(original)])["passed"])

    def test_missing_failed_or_filtered_sarif_never_passes(self) -> None:
        """Scanner execution, query coverage and the complete result set are mandatory."""
        cases: list[JsonObject] = [
            {"version": "2.1.0", "runs": []},
            {**sarif(), "version": "1"},
        ]
        changes: list[tuple[str, JsonValue]] = [
            ("invocations", []),
            ("invocations", [{"executionSuccessful": False}]),
            (
                "invocations",
                [{"executionSuccessful": True, "toolExecutionNotifications": [{"level": "error"}]}],
            ),
            ("tool", {"driver": {"name": "CodeQL", "version": "2.27.1", "rules": []}}),
        ]
        for key, value in changes:
            candidate = sarif()
            run_record(candidate)[key] = value
            cases.append(candidate)
        result_changes: list[tuple[str, JsonValue]] = [
            ("suppressions", [{"kind": "external"}]),
            ("baselineState", "absent"),
            ("ruleId", "py/unlisted"),
        ]
        for key, value in result_changes:
            candidate = sarif()
            object_value(array_value(run_record(candidate)["results"])[0])[key] = value
            cases.append(candidate)
        with patch.object(triage, "source_hash", return_value=HASH):
            for candidate in cases:
                with self.subTest(candidate=candidate), self.assertRaises((ToolError, KeyError)):
                    _ = triage.sarif_findings(candidate, ROOT, REVISION)

    def test_successful_empty_analysis_requires_real_rules_and_execution(self) -> None:
        """A genuine zero-result analysis is permitted, unlike missing analysis metadata."""
        candidate = sarif()
        run_record(candidate)["results"] = []
        self.assertEqual(triage.sarif_findings(candidate, ROOT, REVISION), [])

    def test_missing_stale_or_failed_language_analysis_blocks_api_check(self) -> None:
        """An absent native or Rust analysis cannot be mistaken for no alerts."""
        for change in ("missing", "stale", "error", "warning", "zero_rules"):
            client = FixtureGithub()
            if change == "missing":
                _ = client.analyses.pop()
            else:
                key, value = {
                    "stale": ("commit_sha", "d" * 40),
                    "error": ("error", "failed"),
                    "warning": ("warning", "partial"),
                    "zero_rules": ("rules_count", 0),
                }[change]
                client.analyses[0][key] = value
            with self.subTest(change=change), self.assertRaises(ToolError):
                _ = triage.api_plan(client, ROOT, REVISION, REFERENCE, [])

    def test_manual_dismissal_does_not_bypass_policy(self) -> None:
        """An active dismissed High result still blocks without its repository review."""
        client = FixtureGithub()
        client.alerts[0]["state"] = "dismissed"
        with patch.object(triage, "source_hash", return_value=HASH):
            report = triage.api_plan(client, ROOT, REVISION, REFERENCE, [])
            self.assertFalse(report["passed"])
            self.assertEqual(len(array_value(report["blocked"])), 1)

    def test_medium_is_retained_and_unknown_security_severity_refused(self) -> None:
        """Lower severity is advisory, never silently deleted; missing security scores fail."""
        candidate = alert()
        with patch.object(triage, "source_hash", return_value=HASH):
            object_value(candidate["rule"])["security_severity_level"] = "medium"
            report = triage.verdict([triage.api_finding(candidate, ROOT, REVISION, REFERENCE)], [])
            self.assertTrue(report["passed"])
            self.assertEqual(len(array_value(report["advisory"])), 1)
            object_value(candidate["rule"])["security_severity_level"] = None
            with self.assertRaises(ToolError):
                _ = triage.api_finding(candidate, ROOT, REVISION, REFERENCE)

    def test_authorized_apply_rechecks_and_journals_only_exact_review(self) -> None:
        """Read-only planning makes no calls that mutate; apply records intent and confirmation."""
        client = FixtureGithub()
        journal: list[JsonObject] = []
        with patch.object(triage, "source_hash", return_value=HASH):
            selected = triage.api_finding(alert(), ROOT, REVISION, REFERENCE)
            reviews = [review(selected)]
            plan = triage.api_plan(client, ROOT, REVISION, REFERENCE, reviews)
            self.assertEqual(client.calls, [])
            with patch.object(triage, "false_positive", return_value=True):
                result = triage.apply_plan(client, ROOT, plan, reviews, journal.append)
        self.assertEqual(result["dismissed"], [1])
        self.assertEqual(client.calls[0], ("alerts/1", None))
        self.assertEqual(object_value(client.calls[1][1])["dismissed_reason"], "false positive")
        self.assertEqual(
            [entry["phase"] for entry in journal], ["dismissal_requested", "dismissal_confirmed"]
        )

    def test_changed_plan_or_immediate_alert_prevents_patch(self) -> None:
        """No action occurs after source/policy/alert drift between review and application."""
        for phase in ("plan", "immediate"):
            client = FixtureGithub()
            with patch.object(triage, "source_hash", return_value=HASH):
                reviews = [review(triage.api_finding(alert(), ROOT, REVISION, REFERENCE))]
                plan = triage.api_plan(client, ROOT, REVISION, REFERENCE, reviews)
                if phase == "plan":
                    plan["policySha256"] = "different"
                else:
                    client.changed_at_read = True
                with (
                    self.subTest(phase=phase),
                    self.assertRaises(ToolError),
                    patch.object(triage, "false_positive", return_value=True),
                ):
                    _ = triage.apply_plan(client, ROOT, plan, reviews, lambda _: None)
            self.assertFalse(any(body is not None for _, body in client.calls))

    def test_source_hash_refuses_edits_symlinks_and_path_escape(self) -> None:
        """Hash the complete descriptor-read file only if it equals the selected Git revision."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            path = root / "fixture.py"
            _ = path.write_bytes(b"fixture\n")
            with patch.object(bounded_process, "run", return_value=(0, b"fixture\n", b"")):
                self.assertEqual(
                    triage.source_hash(root, REVISION, path.name),
                    hashlib.sha256(b"fixture\n").hexdigest(),
                )
                for name in ("../fixture.py", str(path)):
                    with self.assertRaises(ToolError):
                        _ = triage.source_hash(root, REVISION, name)
                link = root / "link.py"
                link.symlink_to(path)
                with self.assertRaises(ToolError):
                    _ = triage.source_hash(root, REVISION, link.name)
                _ = path.write_bytes(b"edited\n")
                with self.assertRaises(ToolError):
                    _ = triage.source_hash(root, REVISION, path.name)

    def test_partial_apply_retains_the_unconfirmed_requested_action(self) -> None:
        """A failed PATCH keeps its preceding intent record; it is never reported confirmed."""
        client = FixtureGithub()
        journal: list[JsonObject] = []
        with patch.object(triage, "source_hash", return_value=HASH):
            reviews = [review(triage.api_finding(alert(), ROOT, REVISION, REFERENCE))]
            plan = triage.api_plan(client, ROOT, REVISION, REFERENCE, reviews)
            with (
                patch.object(triage, "false_positive", return_value=True),
                patch.object(client, "request", side_effect=[alert(), ToolError("network_failed")]),
                self.assertRaises(ToolError),
            ):
                _ = triage.apply_plan(client, ROOT, plan, reviews, journal.append)
        self.assertEqual(journal, [{"phase": "dismissal_requested", "number": 1, "dismissed": []}])

    def test_non_false_positive_acceptance_cannot_be_dismissed(self) -> None:
        """A ledger risk acceptance without an exact false-positive disposition is read-only."""
        client = FixtureGithub()
        with patch.object(triage, "source_hash", return_value=HASH):
            reviews = [review(triage.api_finding(alert(), ROOT, REVISION, REFERENCE))]
            plan = triage.api_plan(client, ROOT, REVISION, REFERENCE, reviews)
            with self.assertRaises(ToolError):
                _ = triage.apply_plan(client, ROOT, plan, reviews, lambda _: None)
        self.assertEqual(client.calls, [])

    def test_pagination_and_response_processes_have_fixed_limits(self) -> None:
        """A full page forever fails instead of truncating into a passing report."""
        with patch.object(shutil, "which", return_value="/fixture/gh"):
            client = triage.Github("owner/repository")
        with patch.object(client, "request", return_value=[alert()] * triage.MAX_PAGE) as request:
            with self.assertRaises(ToolError):
                _ = client.pages("alerts", REFERENCE)
            self.assertEqual(request.call_count, triage.MAX_PAGES)
        with patch.object(bounded_process, "run", return_value=(0, b"[]", b"")) as process:
            self.assertEqual(client.request("alerts?per_page=100"), [])
            argv = cast("list[str]", process.call_args.args[0])
            self.assertIn("github.com", argv)
            self.assertIn("GET", argv)
            self.assertNotIn("--paginate", argv)
            limits = cast("bounded_process.Limits", process.call_args.kwargs["limits"])
            self.assertLessEqual(limits.timeout, 20)
            self.assertEqual(limits.stdout, triage.MAX_SOURCE)

    def test_cli_requires_explicit_apply_intent_and_fresh_private_output(self) -> None:
        """No API client is created when authorization or evidence ownership is absent."""
        with tempfile.TemporaryDirectory() as temporary, redirect_stderr(io.StringIO()):
            output = Path(temporary) / "new"
            with patch.object(triage, "Github") as client:
                self.assertEqual(
                    triage.main(
                        [
                            "apply",
                            "--revision",
                            REVISION,
                            "--output",
                            str(output),
                        ]
                    ),
                    1,
                )
                self.assertFalse(output.exists())
                client.assert_not_called()
            output.mkdir()
            with patch.object(triage, "Github") as client:
                self.assertEqual(
                    triage.main(
                        [
                            "check",
                            "--revision",
                            REVISION,
                            "--output",
                            str(output),
                        ]
                    ),
                    1,
                )
                client.assert_not_called()

    def test_all_reviewed_records_are_exact_and_fix_findings_remain_unwaived(self) -> None:
        """The checked-in review covers every initial alert while retaining real remediation."""
        raw = object_value(
            decode_json((ROOT / "security/codeql-review-2026-09-30.json").read_bytes())
        )
        entries = [object_value(item) for item in array_value(raw["alerts"])]
        self.assertEqual(
            {triage.positive(item["number"]) for item in entries},
            set(range(1, INITIAL_ALERT_COUNT + 1)),
        )
        reviews = security_policy.read_exceptions(today=TODAY)
        for item in entries:
            expected = item["disposition"] == "false-positive"
            self.assertEqual(
                security_policy.permitted(
                    reviews, "codeql", str(item["fingerprint"]), str(item["scope"])
                ),
                expected,
            )
            self.assertRegex(str(item["sourceSha256"]), r"^[a-f0-9]{64}$")
            self.assertEqual(triage.false_positive(item, "swiftraccoon/simplestChat"), expected)
            self.assertFalse(triage.false_positive(item, "another/repository"))
        self.assertEqual(
            {
                triage.positive(item["number"])
                for item in entries
                if item["disposition"] == "fix-pending"
            },
            {5, 24, 25, 26},
        )
