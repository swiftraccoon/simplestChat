"""Retain raw RPM license evidence and allow only individually reviewed standard terms."""

from __future__ import annotations

import copy
import hashlib
import json
import unittest
from datetime import date

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as reviews
from release_json import JsonObject, JsonValue, array_value, object_value, string_value

EVIDENCE = ROOT / "security/license-evidence/fedora-rpm-review-2026-09-30.json"
TODAY = date(2026, 9, 30)
REVIEWED_SOFTWARE_TERMS = 55
CONTEXTUAL_TERMS = 7


def observations() -> list[JsonObject]:
    """Load the exact raw records from the separately retained ARM observation."""
    value = object_value(image.report(EVIDENCE))
    return [object_value(item) for item in array_value(value["rawObservations"])]


class RpmLicenseReviewTests(unittest.TestCase):
    """Policy success cannot discard declarations or substitute absent metadata."""

    def test_raw_reviews_bind_full_records_and_preserve_observations(self) -> None:
        """Review binds declarations and paths; complete layer provenance remains separate."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        current = reviews.read_exceptions(today=TODAY)
        for selected in observations():
            with self.subTest(name=selected["name"]):
                original = copy.deepcopy(selected)
                self.assertFalse(image.license_verdict([selected], policy, [])["passed"])
                verdict = image.license_verdict([selected], policy, current)
                self.assertTrue(verdict["passed"])
                self.assertEqual(selected, original)
                finding = object_value(array_value(verdict["waived"])[0])
                raw = array_value(selected["licenses"])
                normalized = copy.deepcopy(raw)
                for item in normalized:
                    for location in array_value(object_value(item)["locations"]):
                        object_value(location)["layerID"] = "sha256:<image-layer>"
                expected = (
                    "license-raw-v2:"
                    + hashlib.sha256(
                        json.dumps(
                            normalized, sort_keys=True, separators=(",", ":"), ensure_ascii=True
                        ).encode()
                    ).hexdigest()
                )
                self.assertEqual(finding["fingerprint"], expected)
                self.assertEqual(finding["fingerprint"], selected["fingerprint"])
                self.assertEqual(finding["rawLicenseRecords"], raw)
                self.assertEqual(
                    finding["rawLicenseRecordsSha256"], selected["rawLicenseRecordsSha256"]
                )
                self.assertEqual(finding["expression"], "UNKNOWN")

    def test_changed_or_missing_raw_records_cannot_borrow_review(self) -> None:
        """Package, value, classification, record set and source evidence all stay exact."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        current = reviews.read_exceptions(today=TODAY)
        for observed in observations():
            record = object_value(array_value(observed["licenses"])[0])
            variants: list[JsonValue] = [
                [],
                [{}],
                [{**record, "value": ""}],
                [{**record, "value": "unreviewed"}],
                [{**record, "type": "concluded"}],
                [{**record, "spdxExpression": "LicenseRef-Unreviewed"}],
                [{**record, "locations": []}],
                [{**record, "urls": ["https://example.invalid/different"]}],
                [record, {"value": "unreviewed", "spdxExpression": "", "type": "declared"}],
            ]
            for raw in variants:
                with self.subTest(name=observed["name"], raw=raw):
                    changed = {**observed, "licenses": raw}
                    self.assertFalse(image.license_verdict([changed], policy, current)["passed"])
            changed = {**observed, "purl": string_value(observed["purl"]) + "&epoch=1"}
            self.assertFalse(image.license_verdict([changed], policy, current)["passed"])

    def test_recognized_license_does_not_hide_an_unparsed_record(self) -> None:
        """Every declared record participates even if a sibling record is approved."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        selected = observations()[0]
        known: JsonObject = {"value": "MIT", "spdxExpression": "MIT", "type": "declared"}
        self.assertTrue(
            image.license_verdict([{**selected, "licenses": [known]}], policy, [])["passed"]
        )
        variants: list[list[JsonValue]] = [
            [known, *array_value(selected["licenses"])],
            [*array_value(selected["licenses"]), known],
            [known, {}],
        ]
        for records in variants:
            with self.subTest(records=records):
                value: JsonObject = {**selected, "licenses": records}
                self.assertFalse(image.license_verdict([value], policy, [])["passed"])

    def test_standard_additions_have_pinned_software_approval(self) -> None:
        """SPDX identity is accompanied by individual Fedora software approval evidence."""
        evidence = object_value(image.report(EVIDENCE))
        records = array_value(evidence["standardTerms"])
        self.assertEqual(len(records), REVIEWED_SOFTWARE_TERMS)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        allowed = {string_value(item) for item in array_value(policy["allowedLicenses"])}
        for item in records:
            record = object_value(item)
            term = string_value(record["term"])
            with self.subTest(term=term):
                self.assertEqual(record["fedoraStatus"], ["allowed"])
                self.assertRegex(string_value(record["fedoraSha256"]), r"^[a-f0-9]{64}$")
                self.assertIn(
                    "ref=cbd8b74cd481a598668f05d1878fcd53a8ef78e4", string_value(record["source"])
                )
                self.assertIn(term, allowed)
                self.assertTrue(image.LicenseExpression(term, allowed).allowed_expression())
                self.assertFalse(
                    image.LicenseExpression(
                        term + " AND LicenseRef-Unreviewed", allowed
                    ).allowed_expression()
                )

    def test_documentation_custom_and_unknown_terms_remain_contextual(self) -> None:
        """A permissive policy expansion cannot approve unrelated conditional restrictions."""
        evidence = object_value(image.report(EVIDENCE))
        contextual = array_value(evidence["contextualTerms"])
        self.assertEqual(len(contextual), CONTEXTUAL_TERMS)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        allowed = {string_value(item) for item in array_value(policy["allowedLicenses"])}
        forbidden = [string_value(object_value(item)["term"]) for item in contextual]
        forbidden.extend(
            (
                "LicenseRef-Fedora-Public-Domain",
                "LicenseRef-Callaway-BSD",
                "LicenseRef-Not-Copyrightable",
                "UNKNOWN",
                "NOASSERTION",
                "GPL-3.0-or-later WITH Unreviewed-exception",
            )
        )
        for term in forbidden:
            with self.subTest(term=term):
                self.assertNotIn(term, allowed)
                self.assertFalse(image.LicenseExpression(term, allowed).allowed_expression())
