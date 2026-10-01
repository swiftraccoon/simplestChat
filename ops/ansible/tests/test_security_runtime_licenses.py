"""Keep canonical runtime license assessments exact, contextual and expiring."""

from __future__ import annotations

import copy
import unittest
from datetime import date

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as reviews
from release_json import JsonObject, array_value, object_value, string_value
from security_license_identity import digest
from security_tools import ToolError

EVIDENCE = ROOT / "security/license-evidence/fedora-runtime-2026-09-30.json"
GLIBC_EVIDENCE = ROOT / "security/license-evidence/fedora-glibc-2026-10-01.json"
GLIBC_PACKAGES = {"glibc", "glibc-common", "glibc-minimal-langpack"}
TODAY = date(2026, 9, 30)


def evidence() -> JsonObject:
    """Read the committed canonical observation rather than constructing package identities."""
    return object_value(image.report(EVIDENCE))


def packages(value: JsonObject) -> list[JsonObject]:
    """Use unchanged canonical PURLs and full ordered license records."""
    declarations = object_value(value["declarations"])
    result: list[JsonObject] = []
    for item in array_value(value["packages"]):
        entry = object_value(item)
        identity = object_value(entry["canonical"])
        declaration = object_value(declarations[string_value(entry["declaration"])])
        result.append(
            {**identity, "name": entry["name"], "type": "rpm", "licenses": declaration["records"]}
        )
    return result


def current_packages() -> list[JsonObject]:
    """Replace superseded observations with the separately retained release-9 evidence."""
    retained = [
        item for item in packages(evidence()) if string_value(item["name"]) not in GLIBC_PACKAGES
    ]
    return [*retained, *packages(object_value(image.report(GLIBC_EVIDENCE)))]


class CanonicalRuntimeLicenseTests(unittest.TestCase):
    """Evidence supports exact scanner dispositions, never a blanket software license grant."""

    def test_every_record_needs_its_exact_current_review(self) -> None:
        """No new global license allowance substitutes for one of the 26 assessments."""
        observed = current_packages()
        original = copy.deepcopy(observed)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        current = reviews.read_exceptions(today=TODAY)
        self.assertEqual(len(observed), 26)
        self.assertEqual(
            len(array_value(image.license_verdict(observed, policy, [])["blocked"])), 26
        )
        verdict = image.license_verdict(observed, policy, current)
        self.assertTrue(verdict["passed"])
        self.assertEqual(len(array_value(verdict["waived"])), 26)
        self.assertEqual(observed, original)
        for package in observed:
            scoped = [entry for entry in current if entry.scope != package["purl"]]
            with self.subTest(name=package["name"]):
                self.assertFalse(image.license_verdict([package], policy, scoped)["passed"])

    def test_changed_scope_or_declarations_cannot_borrow_review(self) -> None:
        """Exact release/source/architecture and complete declaration records remain necessary."""
        policy = image.load_policy(ROOT / "security/image-policy.json")
        current = reviews.read_exceptions(today=TODAY)
        for package in current_packages():
            scope = string_value(package["purl"])
            variants: list[JsonObject] = [
                {**package, "purl": scope + "&unreviewed=1"},
                {**package, "licenses": []},
                {
                    **package,
                    "licenses": [
                        *array_value(package["licenses"]),
                        {"spdxExpression": "LicenseRef-Unreviewed", "value": "unreviewed"},
                    ],
                },
            ]
            for changed in variants:
                with self.subTest(name=package["name"], variant=changed):
                    self.assertFalse(image.license_verdict([changed], policy, current)["passed"])
        libtool = next(
            package for package in current_packages() if package["name"] == "libtool-ltdl"
        )
        old_scope = string_value(libtool["purl"]).replace("arch=x86_64", "arch=aarch64")
        self.assertFalse(
            image.license_verdict([{**libtool, "purl": old_scope}], policy, current)["passed"]
        )

    def test_superseded_glibc_scopes_are_historical_evidence_only(self) -> None:
        """The reviewed update does not accumulate compatibility approvals for release 8."""
        updated = object_value(image.report(GLIBC_EVIDENCE))
        old = [
            item for item in packages(evidence()) if string_value(item["name"]) in GLIBC_PACKAGES
        ]
        new = packages(updated)
        self.assertEqual({string_value(item["name"]) for item in new}, GLIBC_PACKAGES)
        self.assertEqual(len(new), 3)
        self.assertEqual(
            {string_value(scope) for scope in array_value(updated["supersedes"])},
            {string_value(item["purl"]) for item in old},
        )
        current = reviews.read_exceptions(today=TODAY)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        self.assertEqual(
            len(array_value(image.license_verdict(old, policy, current)["blocked"])), 3
        )
        self.assertTrue(image.license_verdict(new, policy, current)["passed"])
        old_declaration = object_value(object_value(evidence()["declarations"])["D03"])
        new_declaration = object_value(object_value(updated["declarations"])["D03"])
        self.assertEqual(old_declaration["reviewFingerprint"], new_declaration["reviewFingerprint"])
        old_notices = object_value(object_value(evidence()["noticeSets"])["N02"])
        new_notices = object_value(object_value(updated["noticeSets"])["N02"])
        self.assertEqual(old_notices["files"], new_notices["files"])
        self.assertNotEqual(old_notices["ownerPurl"], new_notices["ownerPurl"])

    def test_declaration_provenance_and_shared_notices_are_retained(self) -> None:
        """Original record hashes and explicit shared owners remain reviewable evidence."""
        value = evidence()
        declarations = object_value(value["declarations"])
        observed = {string_value(package["name"]): package for package in packages(value)}
        self.assertEqual(len(declarations), 18)
        for declaration in declarations.values():
            entry = object_value(declaration)
            self.assertEqual(digest(entry["records"]), entry["recordsSha256"])
        notice_sets = object_value(value["noticeSets"])
        self.assertEqual(len(notice_sets), 18)
        total = 0
        for item in notice_sets.values():
            notice_set = object_value(item)
            owner = observed[string_value(notice_set["ownerPackage"])]
            self.assertEqual(notice_set["ownerPurl"], owner["purl"])
            self.assertEqual(notice_set["sourceRpm"], owner["sourceRpm"])
            for raw in array_value(notice_set["files"]):
                notice = object_value(raw)
                self.assertTrue(notice["matchesRpmDigest"] is not False)
                self.assertRegex(string_value(notice["sha256"]), r"^[a-f0-9]{64}$")
                total += 1
        self.assertEqual(total, 46)

    def test_artifact_binding_preserves_failed_image_status_and_payload_evidence(self) -> None:
        """A passing license-only decision cannot be described as a passing release."""
        value = evidence()
        canonical = object_value(value["canonical"])
        self.assertEqual(canonical["platform"], "linux/amd64")
        self.assertFalse(canonical["imageCheckPassed"])
        self.assertEqual(canonical["imageCheckError"], "image_runtime_rpm_owner")
        self.assertTrue(canonical["noticeCheckPassed"])
        self.assertEqual(canonical["ciRunId"], 36738045459)
        self.assertEqual(canonical["artifactId"], 11109374252)
        selected = object_value(canonical["selectedImage"])
        self.assertEqual(
            canonical["imageId"], "sha256:" + string_value(selected["archiveConfigSha256"])
        )
        for key in ("outcomeSha256", "noticeSha256", "spdxSha256", "syftSha256"):
            self.assertRegex(string_value(canonical[key]), r"^[a-f0-9]{64}$")
        rootfiles = next(
            object_value(item)
            for item in array_value(value["packages"])
            if object_value(item)["name"] == "rootfiles"
        )
        payloads = array_value(rootfiles["payloadExpectations"])
        self.assertEqual(len(payloads), 6)
        for item in payloads:
            payload = object_value(item)
            self.assertRegex(string_value(payload["sha256"]), r"^[a-f0-9]{64}$")
            self.assertTrue(string_value(payload["spdxFileId"]).startswith("SPDXRef-File-"))

    def test_expiry_and_contextual_terms_remain_restrictive(self) -> None:
        """Changed distribution context cannot be approved by an unbounded term allowance."""
        current = reviews.read_exceptions(today=TODAY)
        scopes = {string_value(package["purl"]) for package in current_packages()}
        selected = [entry for entry in current if entry.scope in scopes]
        self.assertEqual(len(selected), 26)
        self.assertTrue(all(entry.expires == date(2026, 11, 29) for entry in selected))
        with self.assertRaisesRegex(ToolError, "expired_security_exception"):
            _ = reviews.read_exceptions(today=date(2026, 11, 30))
        policy = image.load_policy(ROOT / "security/image-policy.json")
        allowed = array_value(policy["allowedLicenses"])
        for term in ("LicenseRef-Fedora-Public-Domain", "LicenseRef-Callaway-BSD", "UNKNOWN"):
            self.assertNotIn(term, allowed)


if __name__ == "__main__":
    _ = unittest.main()
