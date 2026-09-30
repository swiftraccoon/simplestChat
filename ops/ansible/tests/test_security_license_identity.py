"""Equivalent image layers do not erase any part of a raw license declaration."""

from __future__ import annotations

import copy
import unittest
from datetime import date
from typing import TYPE_CHECKING

from test_support import ROOT, obj, objects, string

# isort: split
import security_image_policy as image
from security_license_identity import raw_license_identity
from security_policy import ExceptionRecord
from security_tools import ToolError

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue


def package() -> JsonObject:
    """Use two distinct unparsed records to exercise ordered declaration semantics."""
    return {
        "name": "fixture",
        "version": "1-2.fc44",
        "type": "rpm",
        "purl": "pkg:rpm/fedora/fixture@1-2.fc44?arch=aarch64&distro=fedora-44",
        "licenses": [
            {
                "value": value,
                "spdxExpression": "",
                "type": "declared",
                "urls": ["https://example.invalid/first", "https://example.invalid/second"],
                "locations": [
                    {
                        "path": "/usr/lib/sysimage/rpm/rpmdb.sqlite",
                        "accessPath": "/usr/lib/sysimage/rpm/rpmdb.sqlite",
                        "layerID": "sha256:" + "a" * 64,
                        "annotations": {"evidence": "primary"},
                    }
                ],
            }
            for value in ("UnparsedFirst", "UnparsedSecond")
        ],
    }


def waiver(selected: JsonObject) -> ExceptionRecord:
    """Review only this exact package and complete normalized record array."""
    policy = image.load_policy(ROOT / "security/image-policy.json")
    finding = objects(image.license_verdict([selected], policy, []), "blocked")[0]
    return ExceptionRecord(
        "image-license",
        string(finding, "fingerprint"),
        string(selected, "purl"),
        "security",
        "inert raw declaration fixture",
        "test-owned package",
        date(2026, 11, 29),
        "https://example.invalid/license-review",
    )


class RawLicenseIdentityTests(unittest.TestCase):
    """A changed layer may reuse review; changed or incomplete license evidence may not."""

    def test_valid_layer_change_preserves_review_and_changes_full_provenance(self) -> None:
        """Only layer bytes vary; original scanner records remain visible and unmodified."""
        selected = package()
        original = copy.deepcopy(selected)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        review = waiver(selected)
        first = objects(image.license_verdict([selected], policy, [review]), "waived")[0]
        changed = copy.deepcopy(selected)
        obj(changed, "licenses", 0, "locations", 0)["layerID"] = "sha256:" + "b" * 64
        verdict = image.license_verdict([changed], policy, [review])
        self.assertTrue(verdict["passed"])
        second = objects(verdict, "waived")[0]
        self.assertEqual(first["fingerprint"], second["fingerprint"])
        self.assertNotEqual(first["rawLicenseRecordsSha256"], second["rawLicenseRecordsSha256"])
        self.assertEqual(second["rawLicenseRecords"], changed["licenses"])
        self.assertEqual(selected, original)

    def test_every_other_declaration_field_and_unknown_field_remains_exact(self) -> None:
        """Values, types, URLs, paths, annotations and future fields all invalidate review."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        selected = package()
        review = waiver(selected)
        changes: list[tuple[tuple[str | int, ...], str, JsonValue]] = [
            (("licenses", 0), "value", "Different"),
            (("licenses", 0), "type", "concluded"),
            (("licenses", 0), "spdxExpression", "MIT"),
            (("licenses", 0), "urls", ["https://example.invalid/changed"]),
            (("licenses", 0), "futureEvidence", "new"),
            (("licenses", 0, "locations", 0), "path", "/different/rpmdb.sqlite"),
            (("licenses", 0, "locations", 0), "accessPath", "/different/rpmdb.sqlite"),
            (("licenses", 0, "locations", 0), "annotations", {"evidence": "secondary"}),
            (("licenses", 0, "locations", 0), "futureEvidence", "new"),
            (("licenses", 0, "locations", 0, "annotations"), "layerID", "sha256:" + "b" * 64),
        ]
        for path, field, value in changes:
            with self.subTest(path=path, field=field):
                changed = copy.deepcopy(selected)
                obj(changed, *path)[field] = value
                self.assertFalse(image.license_verdict([changed], policy, [review])["passed"])

    def test_record_and_evidence_order_and_layer_presence_remain_exact(self) -> None:
        """No sorting or layer-field removal can borrow a prior declaration review."""
        selected = package()
        policy = image.load_policy(ROOT / "security/image-policy.json")
        review = waiver(selected)
        reversed_records = copy.deepcopy(selected)
        reversed_records["licenses"] = list(reversed(objects(selected, "licenses")))
        reversed_urls = copy.deepcopy(selected)
        obj(reversed_urls, "licenses", 0)["urls"] = [
            "https://example.invalid/second",
            "https://example.invalid/first",
        ]
        missing_layer = copy.deepcopy(selected)
        del obj(missing_layer, "licenses", 0, "locations", 0)["layerID"]
        missing_record = copy.deepcopy(selected)
        missing_record["licenses"] = [objects(selected, "licenses")[0]]
        for changed in (reversed_records, reversed_urls, missing_layer, missing_record):
            with self.subTest(changed=changed):
                self.assertFalse(image.license_verdict([changed], policy, [review])["passed"])

    def test_malformed_layer_identity_never_enters_normalized_review(self) -> None:
        """The marker itself, missing digest bytes and wrong types cannot be normalized."""
        selected = package()
        policy = image.load_policy(ROOT / "security/image-policy.json")
        review = waiver(selected)
        invalid: list[JsonValue] = [
            "",
            "sha256:" + "a" * 63,
            "sha256:" + "A" * 64,
            "sha512:" + "a" * 64,
            "sha256:<image-layer>",
            None,
            1,
        ]
        for value in invalid:
            with self.subTest(value=value):
                changed = copy.deepcopy(selected)
                obj(changed, "licenses", 0, "locations", 0)["layerID"] = value
                with self.assertRaises((ValueError, ToolError)):
                    _ = image.license_verdict([changed], policy, [review])

    def test_exact_package_scope_and_new_domain_are_required(self) -> None:
        """A different architecture/version and the previous hash domain remain unreviewed."""
        selected = package()
        policy = image.load_policy(ROOT / "security/image-policy.json")
        review = waiver(selected)
        for change in ("arch=x86_64", "arch=aarch64&epoch=1"):
            changed = {**selected, "purl": string(selected, "purl").replace("arch=aarch64", change)}
            self.assertFalse(image.license_verdict([changed], policy, [review])["passed"])
        changed = {
            **selected,
            "purl": string(selected, "purl").replace("@1-2.fc44", "@1-3.fc44"),
        }
        self.assertFalse(image.license_verdict([changed], policy, [review])["passed"])
        identity = raw_license_identity(list(objects(selected, "licenses")))
        legacy = ExceptionRecord(
            review.scanner,
            "license-raw:" + identity.records_sha256,
            review.scope,
            review.owner,
            review.rationale,
            review.reachability,
            review.expires,
            review.review,
        )
        self.assertFalse(image.license_verdict([selected], policy, [legacy])["passed"])
