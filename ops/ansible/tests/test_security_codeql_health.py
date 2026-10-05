"""Exercise stored-analysis health independently of job status, SARIF and alert reviews."""

from __future__ import annotations

import copy
import io
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from typing import override
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from test_support import obj

# isort: split
import security_codeql_triage as triage
import security_policy
from release_json import JsonObject, JsonValue, array_value, decode_json
from security_tools import ToolError

REVISION = "a" * 40
REFERENCE = "refs/heads/main"


def records(reference: str = REFERENCE) -> list[JsonObject]:
    """Model current API analyses with no findings but real executed queries."""
    return [
        {
            "id": index,
            "ref": reference,
            "commit_sha": REVISION,
            "category": f"/language:{language}/security",
            "tool": {"name": "CodeQL"},
            "rules_count": 200,
            "results_count": 0,
            "error": "",
            "warning": "",
        }
        for index, language in enumerate(sorted(triage.AUTOMATED_LANGUAGES), start=1)
    ]


class HealthGithub(triage.Github):
    """Use actual pagination/reference validation over an inert, read-only API transport."""

    def __init__(self, reference: str = REFERENCE) -> None:
        """Supply a current repository ref and independently mutable server records."""
        with patch.object(shutil, "which", return_value="/fixture/gh"):
            super().__init__("owner/repository")
        self.reference: str = reference
        self.analyses: list[JsonObject] = records(reference)
        self.calls: list[str] = []
        self.head_reads: int = 0
        self.last_head: str = REVISION

    @override
    def request(self, suffix: str, body: JsonObject | None = None) -> JsonValue:
        """Reject every mutation or alert/source query instead of simulating it."""
        if body is not None:
            message = "health_must_be_read_only"
            raise AssertionError(message)
        self.calls.append(suffix)
        if suffix == "git/ref/" + self.reference.removeprefix("refs/"):
            self.head_reads += 1
            return {
                "ref": self.reference,
                "object": {
                    "type": "commit",
                    "sha": REVISION if self.head_reads == 1 else self.last_head,
                },
            }
        parsed = urlsplit(suffix)
        query = parse_qs(parsed.query)
        if parsed.path != "analyses" or query.get("ref") != [self.reference]:
            message = "unexpected_health_endpoint"
            raise AssertionError(message)
        if query.get("sort") != ["created"] or query.get("direction") != ["desc"]:
            message = "health_requires_newest_first"
            raise AssertionError(message)
        page = int(query["page"][0])
        begin = (page - 1) * triage.MAX_PAGE
        return list[JsonValue](copy.deepcopy(self.analyses[begin : begin + triage.MAX_PAGE]))


class CodeqlHealthTests(unittest.TestCase):
    """Require current complete successful ingestion, even after successful scanner jobs."""

    def test_current_main_and_fork_merge_ref_require_four_automated_analyses(self) -> None:
        """No findings is healthy only when each exact ref has nonempty executed queries."""
        for reference in (REFERENCE, "refs/pull/42/merge"):
            client = HealthGithub(reference)
            with self.subTest(reference=reference):
                report = triage.analysis_health(client, REVISION, reference)
                self.assertTrue(report["passed"] is True)
                self.assertEqual(report["ref"], reference)
                self.assertEqual(report["revision"], REVISION)
                self.assertEqual(
                    len(array_value(report["analyses"])), len(triage.AUTOMATED_LANGUAGES)
                )
                self.assertEqual(client.head_reads, 2)
                self.assertEqual(len(client.calls), 3)

    def test_historical_native_failures_do_not_block_first_party_health(self) -> None:
        """Only enabled categories participate, while native SARIF remains locally supported."""
        client = HealthGithub()
        native = copy.deepcopy(client.analyses[0])
        native.update(
            {
                "category": "/language:c-cpp/security",
                "commit_sha": "b" * 40,
                "error": "old native result",
            }
        )
        client.analyses.insert(0, native)
        result = triage.analysis_health(client, REVISION, REFERENCE)
        self.assertTrue(result["passed"])
        self.assertEqual(len(array_value(result["analyses"])), 4)
        self.assertIn("c-cpp", triage.LANGUAGES)
        self.assertNotIn("c-cpp", triage.AUTOMATED_LANGUAGES)

    def test_successful_job_cannot_override_failed_server_ingestion(self) -> None:
        """The observed Unknown Error/zero-query server shape fails without trusting job state."""
        client = HealthGithub()
        javascript = next(
            value
            for value in client.analyses
            if value["category"] == "/language:javascript-typescript/security"
        )
        javascript.update(
            {"id": 1868844760, "error": "Unknown Error", "rules_count": 0, "results_count": 0}
        )
        with self.assertRaisesRegex(ToolError, "^codeql_analysis_failed$"):
            _ = triage.analysis_health(client, REVISION, REFERENCE)

    def test_unhealthy_incomplete_or_foreign_analysis_cannot_pass(self) -> None:
        """Schema, identity and coverage are independently mandatory."""
        cases: tuple[tuple[str, JsonValue], ...] = (
            ("commit_sha", "b" * 40),
            ("ref", "refs/heads/other"),
            ("tool", {"name": "other"}),
            ("warning", "partial analysis"),
            ("error", None),
            ("rules_count", 0),
            ("rules_count", True),
            ("rules_count", -1),
            ("id", True),
            ("category", "/language:rust/quality-advisory"),
        )
        for key, value in cases:
            client = HealthGithub()
            client.analyses[0][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ToolError):
                _ = triage.analysis_health(client, REVISION, REFERENCE)
        client = HealthGithub()
        _ = client.analyses.pop()
        with self.assertRaisesRegex(ToolError, "^codeql_missing_current_analysis$"):
            _ = triage.analysis_health(client, REVISION, REFERENCE)
        client = HealthGithub()
        del client.analyses[0]["warning"]
        with self.assertRaises(KeyError):
            _ = triage.analysis_health(client, REVISION, REFERENCE)

    def test_latest_expected_category_cannot_be_replaced_by_older_good_record(self) -> None:
        """Newer stale or failed security records remain authoritative over older good ones."""
        for field, value in (("commit_sha", "b" * 40), ("error", "ingestion failed")):
            client = HealthGithub()
            latest = copy.deepcopy(client.analyses[0])
            latest[field] = value
            client.analyses.insert(0, latest)
            with self.subTest(field=field), self.assertRaises(ToolError):
                _ = triage.analysis_health(client, REVISION, REFERENCE)

    def test_unrelated_newer_category_does_not_replace_current_security_coverage(self) -> None:
        """A later quality-advisory run is separate; pagination still finds all security records."""
        client = HealthGithub()
        unrelated = copy.deepcopy(client.analyses[0])
        unrelated.update(
            {
                "category": "/language:actions/quality-advisory",
                "commit_sha": "b" * 40,
                "error": "unrelated failed analysis",
            }
        )
        client.analyses = [unrelated] * triage.MAX_PAGE + client.analyses
        report = triage.analysis_health(client, REVISION, REFERENCE)
        self.assertTrue(report["passed"] is True)
        self.assertEqual(len(client.calls), 4)

    def test_current_ref_is_rechecked_after_server_reads(self) -> None:
        """A changed branch or PR merge commit cannot borrow the prior commit's health."""
        client = HealthGithub()
        client.last_head = "b" * 40
        with self.assertRaisesRegex(ToolError, "^codeql_ref_head_changed$"):
            _ = triage.analysis_health(client, REVISION, REFERENCE)
        client = HealthGithub()
        with self.assertRaisesRegex(ToolError, "^codeql_ref_head_changed$"):
            _ = triage.analysis_health(client, "b" * 40, REFERENCE)
        self.assertEqual(len(client.calls), 1)

    def test_cli_health_reads_no_review_or_source_and_publishes_no_raw_errors(self) -> None:
        """Health has no policy/cache dependency; server errors produce fixed evidence."""
        for failed in (False, True):
            client = HealthGithub()
            if failed:
                client.analyses[0]["error"] = "private-server-diagnostic"
            with (
                self.subTest(failed=failed),
                tempfile.TemporaryDirectory() as temporary,
                redirect_stderr(io.StringIO()),
                patch.object(triage, "Github", return_value=client),
                patch.object(security_policy, "read_exceptions") as reviews,
                patch.object(triage, "NativeSources") as sources,
            ):
                output = Path(temporary) / "health"
                status = triage.main(["health", "--revision", REVISION, "--output", str(output)])
                self.assertEqual(status, int(failed))
                reviews.assert_not_called()
                sources.assert_not_called()
                name = "failure.json" if failed else "report.json"
                evidence = obj(decode_json((output / name).read_bytes()))
                self.assertIs(evidence["passed"], not failed)
                if failed:
                    self.assertEqual(evidence["reason"], "codeql_analysis_failed")
                    self.assertFalse((output / "report.json").exists())
                self.assertNotIn("private-server-diagnostic", str(evidence))

    def test_health_rejects_cached_inputs_before_api_access(self) -> None:
        """Health cannot substitute local plans, source caches or authorized writes."""
        for extra in (
            ["--input", "/fixture/report.json"],
            ["--source-cache", "/fixture/cache"],
            ["--authorize-dismissals"],
        ):
            with (
                self.subTest(extra=extra),
                tempfile.TemporaryDirectory() as temporary,
                redirect_stderr(io.StringIO()),
                patch.object(triage, "Github") as client,
            ):
                status = triage.main(
                    [
                        "health",
                        "--revision",
                        REVISION,
                        "--output",
                        str(Path(temporary) / "health"),
                        *extra,
                    ]
                )
                self.assertEqual(status, 1)
                client.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
