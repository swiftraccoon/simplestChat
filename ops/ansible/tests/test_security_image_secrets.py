"""Exact complete-match reviews cannot approve unrelated image findings or stale evidence."""

from __future__ import annotations

import copy
import hashlib
import json
import tempfile
import unittest
from datetime import date
from pathlib import Path

from test_support import ROOT

# isort: split
import security_image as pipeline
import security_image_policy as image
import security_policy as ledger
import security_secret_spans as spans
from release_json import JsonObject, JsonValue, array_value, object_value
from security_tools import ToolError
from test_security_secret_spans import CANARY, NAME, finding, fixture

_ = ROOT
TODAY = date(2026, 9, 30)


def review(fingerprint: str, scope: str = "001/example") -> ledger.ExceptionRecord:
    """Create a review of one never-issued unit fixture, not a detector allowlist."""
    return ledger.ExceptionRecord(
        "gitleaks",
        fingerprint,
        scope,
        "fixture",
        "Public inert bytes",
        "Never issued",
        date(2026, 11, 29),
        "https://example.invalid/review",
    )


def observation() -> tuple[list[JsonValue], JsonObject, JsonObject]:
    """Small complete report and independently bound region, without candidate payloads."""
    row = finding((32835, 32835, 8, 47))
    paths: JsonObject = {
        NAME: {
            "path": "001/example",
            "sha256": "a" * 64,
            "projectionSha256": "b" * 64,
            "projectionFormat": "ascii-printable",
        }
    }
    region: JsonObject = {
        "rule": "github-pat",
        "path": "001/example",
        "fileSha256": "a" * 64,
        "projectionSha256": "b" * 64,
        "startLine": 32835,
        "endLine": 32835,
        "startColumn": 8,
        "endColumn": 47,
        "status": "resolved",
        "spanBytes": len(CANARY),
        "spanSha256": hashlib.sha256(CANARY).hexdigest(),
    }
    return [row], paths, {"format": spans.FORMAT, "findings": [region]}


class ImageSecretIdentityTests(unittest.TestCase):
    """Current content identity is exact; surrounding changes remain visible as evidence."""

    def test_new_surroundings_and_line_use_fresh_evidence_for_same_public_region(self) -> None:
        """Exercise actual projector/collector/integration across two different complete files."""
        reviews: list[ledger.ExceptionRecord] = []
        results: list[JsonObject] = []
        for heading, line in ((b"heading\n", 32835), (b"different\ncontext\n", 32836)):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                _, paths = fixture(root, heading + b"TOKEN=" + CANARY + b"\n")
                report: list[JsonValue] = [finding((line, line, 8, 47))]
                secret = root / "secrets"
                secret.mkdir()
                pipeline.write(root / "secret-paths.json", paths)
                pipeline.write(secret / "gitleaks.json", report)
                blocked = pipeline.secret_checks(secret, root, [])
                region = object_value(array_value(blocked["blocked"])[0])
                if not reviews:
                    reviews.append(review(str(region["fingerprint"])))
                result = pipeline.secret_checks(secret, root, reviews)
                self.assertTrue(result["passed"])
                results.append(object_value(array_value(result["waived"])[0]))
        self.assertEqual(results[0]["fingerprint"], results[1]["fingerprint"])
        for field in ("fileSha256", "projectionSha256", "startLine"):
            self.assertNotEqual(results[0][field], results[1][field])
        self.assertNotIn(CANARY.decode(), json.dumps(results))

    def test_each_identity_component_is_exact_and_unreviewed_match_still_blocks(self) -> None:
        """Changed bytes, length, rule or path cannot borrow the reviewed public region."""
        rows, paths, regions = observation()
        original = object_value(array_value(regions["findings"])[0])
        approved = review(image.secret_fingerprint(original))
        for field, value in (
            ("spanSha256", "c" * 64),
            ("spanBytes", 39),
            ("rule", "generic-api-key"),
            ("path", "002/example"),
        ):
            current_rows, current_paths, current_regions = copy.deepcopy((rows, paths, regions))
            span = object_value(array_value(current_regions["findings"])[0])
            span[field] = value
            if field == "rule":
                object_value(current_rows[0])["RuleID"] = value
            if field == "path":
                object_value(current_paths[NAME])["path"] = value
            with self.subTest(field=field):
                result = image.secret_verdict(
                    current_rows, [approved], current_paths, current_regions
                )
                self.assertFalse(result["passed"])
        extra = {**original, "spanSha256": "d" * 64}
        result = image.secret_verdict(
            rows + rows, [approved], paths, {"format": spans.FORMAT, "findings": [original, extra]}
        )
        self.assertEqual(len(array_value(result["waived"])), 1)
        self.assertEqual(len(array_value(result["blocked"])), 1)

    def test_complete_binding_rejects_stale_misaligned_or_missing_span_rows(self) -> None:
        """All report coordinates and both whole-file identities bind one corresponding row."""
        rows, paths, regions = observation()
        for field, value in (
            ("rule", "fixture"),
            ("path", "002/example"),
            ("fileSha256", "c" * 64),
            ("projectionSha256", "c" * 64),
            ("startLine", 1),
            ("endLine", 1),
            ("startColumn", 1),
            ("endColumn", 1),
            ("startColumn", True),
        ):
            altered = copy.deepcopy(regions)
            object_value(array_value(altered["findings"])[0])[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ToolError, "span_binding"):
                _ = image.secret_verdict(rows, [], paths, altered)
        invalid_reports: list[JsonObject] = [
            {"format": spans.FORMAT, "findings": []},
            {**regions, "format": "unreviewed-scanner-format"},
        ]
        for changed in invalid_reports:
            with self.assertRaisesRegex(ToolError, "span_report"):
                _ = image.secret_verdict(rows, [], paths, changed)
        altered_paths = copy.deepcopy(paths)
        object_value(altered_paths[NAME])["projectionFormat"] = "unreviewed-projection"
        with self.assertRaises(ToolError):
            _ = image.secret_verdict(rows, [], altered_paths, regions)

    def test_unresolved_regions_never_receive_fingerprint_or_inherit_review(self) -> None:
        """Unknown and ambiguous regions block, even if stale digest fields are injected."""
        rows, paths, regions = observation()
        original = object_value(array_value(regions["findings"])[0])
        approved = review(image.secret_fingerprint(original))
        for status in ("ambiguous", "unresolved"):
            span = {
                key: value
                for key, value in original.items()
                if key not in {"spanBytes", "spanSha256"}
            }
            span["status"] = status
            current: JsonObject = {"format": spans.FORMAT, "findings": [span]}
            result = image.secret_verdict(rows, [approved], paths, current)
            self.assertFalse(result["passed"])
            self.assertNotIn("fingerprint", object_value(array_value(result["blocked"])[0]))
            span["spanSha256"] = original["spanSha256"]
            with self.assertRaisesRegex(ToolError, "span_fields"):
                _ = image.secret_verdict(rows, [approved], paths, current)

    def test_malformed_regions_and_untrusted_payloads_cannot_be_published(self) -> None:
        """A strict field set prevents raw values or invalid hashes reaching public evidence."""
        rows, paths, regions = observation()
        for field, value in (
            ("spanBytes", True),
            ("spanBytes", 0),
            ("spanBytes", spans.MAX_FILE + 1),
            ("spanSha256", "NOT-A-DIGEST"),
            ("Secret", "SENSITIVE-FIXTURE"),
        ):
            changed = copy.deepcopy(regions)
            object_value(array_value(changed["findings"])[0])[field] = value
            with self.subTest(field=field), self.assertRaises(ToolError):
                _ = image.secret_verdict(rows, [], paths, changed)
        object_value(rows[0]).update({"Secret": "SENSITIVE-FIXTURE", "Match": "SENSITIVE-FIXTURE"})
        result = image.secret_verdict(rows, [], paths, regions)
        self.assertNotIn("SENSITIVE", json.dumps(result))

    def test_superseded_image_identity_and_expired_or_missing_review_cannot_pass(self) -> None:
        """There is one current image matcher; existing source-fixture identities remain exact."""
        rows, paths, regions = observation()
        old = review("github-pat:001/example:32835:" + "a" * 64)
        for reviews in ([], [old]):
            self.assertFalse(image.secret_verdict(rows, reviews, paths, regions)["passed"])
        self.assertTrue(ledger.permitted([old], "gitleaks", old.fingerprint, old.scope))
        with self.assertRaisesRegex(ToolError, "expired_security_exception"):
            _ = ledger.parse_exception(
                {
                    "scanner": old.scanner,
                    "fingerprint": old.fingerprint,
                    "scope": old.scope,
                    "owner": old.owner,
                    "rationale": old.rationale,
                    "reachability": old.reachability,
                    "expires": "2026-09-29",
                    "review": old.review,
                },
                TODAY,
            )
