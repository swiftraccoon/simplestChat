"""Keep the Fedora static-runtime review exact without rewriting observed license evidence."""

from __future__ import annotations

import copy
import hashlib
import unittest
from datetime import date

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as reviews
from release_json import JsonObject, array_value, object_value, string_value
from security_tools import ToolError

EVIDENCE = ROOT / "security/license-evidence/fedora-libstdcxx-16.2.1-2.fc44.json"
TODAY = date(2026, 9, 30)


def observation() -> JsonObject:
    """Read the retained real ARM observation independently of the policy entries."""
    evidence = object_value(image.report(EVIDENCE))
    return object_value(evidence["observation"])


def package(architecture: str = "aarch64", *, epoch: str = "0") -> JsonObject:
    """Use the real declared aggregate with explicitly selected RPM identity fields."""
    observed = observation()
    owner = string_value(observed["owner"]).split("\t")
    owner[1] = epoch + ":16.2.1-2.fc44." + architecture
    value: JsonObject = {
        "name": "libstdc++-static",
        "type": "binary",
        "licenses": [{"spdxExpression": observed["license"]}],
    }
    return image.static_rpm(value, owner)


class FedoraRuntimeReviewTests(unittest.TestCase):
    """Exercise the maintained verdict with actual metadata and independently changed identities."""

    def test_observed_aggregate_is_retained_and_requires_explicit_review(self) -> None:
        """The broad Fedora aggregate is not silently added to the global allowlist."""
        selected = package()
        original = copy.deepcopy(selected)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        self.assertFalse(image.license_verdict([selected], policy, [])["passed"])
        current = reviews.read_exceptions(today=TODAY)
        verdict = image.license_verdict([selected], policy, current)
        self.assertTrue(verdict["passed"])
        self.assertEqual(selected, original)
        waived = array_value(verdict["waived"])
        self.assertEqual(len(waived), 1)
        expression = "(" + string_value(observation()["license"]) + ")"
        self.assertEqual(object_value(waived[0])["expression"], expression)
        self.assertEqual(
            object_value(waived[0])["fingerprint"],
            "license:" + hashlib.sha256(expression.encode()).hexdigest(),
        )
        self.assertNotIn("LicenseRef-Fedora-Public-Domain", array_value(policy["allowedLicenses"]))

    def test_same_source_x86_policy_is_explicit_and_distinct_from_arm_observation(self) -> None:
        """Supported release architecture gets its own review rather than a wildcard scope."""
        self.assertEqual(observation()["platform"], "linux/arm64")
        current = reviews.read_exceptions(today=TODAY)
        scoped = [entry for entry in current if entry.scanner == "image-license"]
        policy = image.load_policy(ROOT / "security/image-policy.json")
        for architecture in ("aarch64", "x86_64"):
            selected = package(architecture)
            with self.subTest(architecture=architecture):
                matching = [entry for entry in scoped if entry.scope == selected["purl"]]
                self.assertEqual(len(matching), 1)
                self.assertTrue(image.license_verdict([selected], policy, matching)["passed"])
                if architecture == "x86_64":
                    self.assertIn("inference", matching[0].reachability)
        self.assertNotEqual(package()["purl"], package("x86_64")["purl"])

    def test_changed_identity_or_license_does_not_borrow_review(self) -> None:
        """Release, source, architecture, epoch and unknown expression changes all block."""
        current = reviews.read_exceptions(today=TODAY)
        policy = image.load_policy(ROOT / "security/image-policy.json")
        selected = package()
        scope = string_value(selected["purl"])
        changes: list[JsonObject] = [
            {"purl": scope.replace("libstdc%2B%2B-static", "different-runtime")},
            {"purl": scope.replace("@16.2.1-2.fc44", "@16.2.1-3.fc44")},
            {"purl": scope.replace("gcc-16.2.1-2.fc44.src.rpm", "gcc-16.2.1-3.fc44.src.rpm")},
            {"purl": scope.replace("arch=aarch64", "arch=ppc64le")},
            {"licenses": [{"spdxExpression": "UNKNOWN"}]},
            {"licenses": [{"spdxExpression": "LicenseRef-Fedora-Public-Domain"}]},
            {"licenses": [{"spdxExpression": string_value(observation()["license"]) + " AND MIT"}]},
            {"licenses": []},
        ]
        for changeset in changes:
            with self.subTest(changes=changeset):
                self.assertFalse(
                    image.license_verdict([{**selected, **changeset}], policy, current)["passed"]
                )
        nonzero = package(epoch="1")
        self.assertIn("epoch=1", string_value(nonzero["purl"]))
        self.assertEqual(object_value(nonzero["metadata"])["epoch"], 1)
        self.assertFalse(image.license_verdict([nonzero], policy, current)["passed"])

    def test_review_expiry_cannot_be_renewed_implicitly(self) -> None:
        """The exact exception uses the central validator and stops after its stated expiry."""
        policy = object_value(image.report(ROOT / "security/exceptions.json"))
        entries = [
            object_value(item)
            for item in array_value(policy["exceptions"])
            if object_value(item)["scanner"] == "image-license"
            and object_value(item)["scope"] == package()["purl"]
        ]
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0]["expires"], "2026-11-29")
        with self.assertRaisesRegex(ToolError, "expired_security_exception"):
            _ = reviews.parse_exception(entries[0], date(2026, 11, 30))


if __name__ == "__main__":
    _ = unittest.main()
