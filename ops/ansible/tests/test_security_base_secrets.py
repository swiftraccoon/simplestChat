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


def observation() -> tuple[list[JsonValue], JsonObject, JsonObject]:
    """Replay authenticated complete-match identities without candidate secret text."""
    evidence = object_value(image.report(EVIDENCE))
    findings: list[JsonValue] = []
    paths: JsonObject = {}
    spans: list[JsonValue] = []
    for raw in array_value(evidence["files"]):
        file = object_value(raw)
        name = string_value(file["neutralName"])
        paths[name] = {
            key: file[key]
            for key in (
                "path",
                "sha256",
                "projectionSha256",
                "projectionFormat",
            )
        }
        for value in array_value(file["findings"]):
            finding = object_value(value)
            span = object_value(finding["span"])
            spans.append(span)
            row: JsonObject = {"File": name, "RuleID": finding["rule"]}
            for field in ("startLine", "endLine", "startColumn", "endColumn"):
                row[field[0].upper() + field[1:]] = span[field]
            findings.append(row)
    return findings, paths, {"format": evidence["matchFormat"], "findings": spans}


class BaseImageSecretReviewTests(unittest.TestCase):
    """Public test material needs exact complete-match review, never a detector bypass."""

    def test_only_the_fifteen_observed_findings_are_reviewed(self) -> None:
        """Each original observation blocks when its individual assessment is absent."""
        findings, paths, spans = observation()
        current = reviews.read_exceptions(today=TODAY)
        self.assertEqual(len(findings), 15)
        self.assertEqual(len(paths), 5)
        self.assertEqual(
            len(array_value(image.secret_verdict(findings, [], paths, spans)["blocked"])), 15
        )
        result = image.secret_verdict(findings, current, paths, spans)
        self.assertTrue(result["passed"])
        self.assertEqual(len(array_value(result["waived"])), 15)
        for raw in array_value(result["waived"]):
            finding = object_value(raw)
            remaining = [item for item in current if item.fingerprint != finding["fingerprint"]]
            with self.subTest(fingerprint=finding["fingerprint"]):
                blocked = array_value(
                    image.secret_verdict(findings, remaining, paths, spans)["blocked"]
                )
                self.assertEqual(len(blocked), 1)
                self.assertEqual(object_value(blocked[0])["fingerprint"], finding["fingerprint"])

    def test_changed_span_path_or_rule_cannot_inherit_review(self) -> None:
        """Only reviewed exact public regions can be accepted, including within the same binary."""
        findings, paths, spans = observation()
        current = reviews.read_exceptions(today=TODAY)
        for raw, raw_span in zip(findings, array_value(spans["findings"]), strict=True):
            finding, original = object_value(raw), object_value(raw_span)
            name = string_value(finding["File"])
            for field, value in (
                ("spanSha256", "a" * 64),
                ("spanBytes", 1),
                ("path", "other/unreviewed-file"),
                ("rule", "github-pat"),
            ):
                altered_paths = copy.deepcopy(paths)
                altered_finding = dict(finding)
                span = {**original, field: value}
                if field == "path":
                    object_value(altered_paths[name])["path"] = value
                if field == "rule":
                    altered_finding["RuleID"] = value
                with self.subTest(file=name, field=field):
                    result = image.secret_verdict(
                        [altered_finding],
                        current,
                        altered_paths,
                        {"format": spans["format"], "findings": [span]},
                    )
                    self.assertFalse(result["passed"])

    def test_unreviewed_database_and_application_content_remain_blocking(self) -> None:
        """Exact base data cannot approve the same region in metadata or a browser asset."""
        findings, paths, spans = observation()
        current = reviews.read_exceptions(today=TODAY)
        name = string_value(object_value(findings[0])["File"])
        original = object_value(array_value(spans["findings"])[0])
        for path in ("001/usr/lib/sysimage/rpm/rpmdb.sqlite", "008/app/web/dist/assets/app.js"):
            with self.subTest(path=path):
                altered: JsonObject = {
                    **paths,
                    "content-999999": {**object_value(paths[name]), "path": path},
                }
                result = image.secret_verdict(
                    [*findings, {**object_value(findings[0]), "File": "content-999999"}],
                    current,
                    altered,
                    {
                        "format": spans["format"],
                        "findings": [
                            *array_value(spans["findings"]),
                            {**original, "path": path},
                        ],
                    },
                )
                self.assertFalse(result["passed"])
                self.assertEqual(len(array_value(result["waived"])), 15)
                self.assertEqual(len(array_value(result["blocked"])), 1)


if __name__ == "__main__":
    _ = unittest.main()
