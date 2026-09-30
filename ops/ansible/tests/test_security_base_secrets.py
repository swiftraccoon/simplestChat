"""Keep public base-image test data distinct from unreviewed secret findings."""

from __future__ import annotations

import copy
import unittest
from datetime import date

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as reviews
from release_json import JsonObject, JsonValue, array_value, object_value, string_value

EVIDENCE = ROOT / "security/secret-evidence/fedora-base-2026-09-30.json"
TODAY = date(2026, 9, 30)


def observation() -> tuple[list[JsonValue], JsonObject]:
    """Replay original canonical identities without copying candidate secret text."""
    evidence = object_value(image.report(EVIDENCE))
    findings: list[JsonValue] = []
    paths: JsonObject = {}
    for raw in array_value(evidence["files"]):
        file = object_value(raw)
        name = string_value(file["neutralName"])
        paths[name] = {"path": file["path"], "sha256": file["sha256"]}
        for value in array_value(file["findings"]):
            finding = object_value(value)
            findings.append({"File": name, "RuleID": finding["rule"], "StartLine": finding["line"]})
    return findings, paths


class BaseImageSecretReviewTests(unittest.TestCase):
    """Public self-test material needs complete exact-file review, never a detector bypass."""

    def test_only_the_fifteen_observed_findings_are_reviewed(self) -> None:
        """Each original observation blocks when its individual assessment is absent."""
        findings, paths = observation()
        current = reviews.read_exceptions(today=TODAY)
        self.assertEqual(len(findings), 15)
        self.assertEqual(len(paths), 5)
        self.assertEqual(len(array_value(image.secret_verdict(findings, [], paths)["blocked"])), 15)
        result = image.secret_verdict(findings, current, paths)
        self.assertTrue(result["passed"])
        self.assertEqual(len(array_value(result["waived"])), 15)
        for raw in array_value(result["waived"]):
            finding = object_value(raw)
            remaining = [item for item in current if item.fingerprint != finding["fingerprint"]]
            with self.subTest(fingerprint=finding["fingerprint"]):
                blocked = array_value(image.secret_verdict(findings, remaining, paths)["blocked"])
                self.assertEqual(len(blocked), 1)
                self.assertEqual(object_value(blocked[0])["fingerprint"], finding["fingerprint"])

    def test_changed_file_path_line_or_rule_cannot_inherit_review(self) -> None:
        """New content or a different finding in the same binary remains a blocking result."""
        findings, paths = observation()
        current = reviews.read_exceptions(today=TODAY)
        for raw in findings:
            finding = object_value(raw)
            name = string_value(finding["File"])
            for field, value in (("sha256", "a" * 64), ("path", "other/unreviewed-file")):
                altered = copy.deepcopy(paths)
                object_value(altered[name])[field] = value
                with self.subTest(file=name, field=field):
                    self.assertFalse(image.secret_verdict([finding], current, altered)["passed"])
            for field, value in (("RuleID", "github-pat"), ("StartLine", 1)):
                altered_finding = {**finding, field: value}
                with self.subTest(file=name, field=field):
                    self.assertFalse(
                        image.secret_verdict([altered_finding], current, paths)["passed"]
                    )

    def test_unreviewed_database_and_application_content_remain_blocking(self) -> None:
        """The reviewed base layer cannot approve updated metadata or a browser asset."""
        findings, paths = observation()
        current = reviews.read_exceptions(today=TODAY)
        for name in ("001/usr/lib/sysimage/rpm/rpmdb.sqlite", "008/app/web/dist/assets/app.js"):
            with self.subTest(path=name):
                altered: JsonObject = {**paths, "new": {"path": name, "sha256": "b" * 64}}
                result = image.secret_verdict(
                    [*findings, {"File": "new", "RuleID": "generic-api-key", "StartLine": 1}],
                    current,
                    altered,
                )
                self.assertFalse(result["passed"])
                self.assertEqual(len(array_value(result["waived"])), 15)
                self.assertEqual(len(array_value(result["blocked"])), 1)


if __name__ == "__main__":
    _ = unittest.main()
