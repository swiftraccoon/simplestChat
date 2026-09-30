"""Known upstream advisory applicability remains independent of scanner ingestion."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path

from test_support import ROOT

# isort: split
import security_image_advisories as advisory
import security_image_policy as policy
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value
from security_policy import ExceptionRecord
from security_tools import ToolError
from test_security_image_policy import NOW, vulnerability


def openssl(
    version: str = "3.5.8",
    *,
    name: str = "openssl-libs",
    release: str = "1.fc44",
    epoch: int = 1,
    architecture: str = "aarch64",
) -> JsonObject:
    """Use actual Syft 1.52 RPM metadata shape with inert owned package identities."""
    source = f"openssl-{version}-{release}.src.rpm"
    suffix = f"?arch={architecture}&distro=fedora-44"
    if epoch:
        suffix += f"&epoch={epoch}"
    suffix += f"&upstream={source}"
    return {
        "id": f"observed-{name}-{version}",
        "name": name,
        "version": (f"{epoch}:" if epoch else "") + f"{version}-{release}",
        "type": "rpm",
        "purl": f"pkg:rpm/fedora/{name}@{version}-{release}{suffix}",
        "metadataType": "rpm-db-entry",
        "metadata": {
            "name": name,
            "version": version,
            "release": release,
            "epoch": epoch,
            "architecture": architecture,
            "sourceRpm": source,
        },
    }


def empty_grype() -> JsonObject:
    """Successful scanner output can contain no indexed matches."""
    return {**vulnerability(), "matches": []}


class ImageAdvisoryTests(unittest.TestCase):
    """Bounds, current identity and fixed releases cannot silently weaken the safeguard."""

    def test_zero_grype_matches_still_blocks_actual_affected_rpm(self) -> None:
        """The observed image package remains blocked without pretending Grype matched it."""
        result = policy.vulnerability_verdict(empty_grype(), [], [openssl()])
        self.assertFalse(result["passed"])
        self.assertEqual(result["matches"], 0)
        findings = array_value(result["blocked"])
        self.assertEqual(len(findings), 1)
        finding = object_value(findings[0])
        self.assertEqual(finding["id"], "CVE-2026-84782")
        self.assertEqual(finding["scope"], openssl()["purl"])
        self.assertEqual(finding["source"], "reviewed-openssl-advisory")
        self.assertEqual(finding["upstreamFixedVersion"], "3.5.9")
        self.assertNotIn("fixState", finding)
        evidence = object_value(result["reviewedAdvisories"])
        self.assertEqual(len(array_value(evidence["sources"])), 2)
        self.assertEqual(len(str(evidence["policySha256"])), 64)

    def test_all_affected_patches_and_subpackages_are_checked(self) -> None:
        """The policy covers source RPM subpackages on both supported image architectures."""
        for patch in range(9):
            for name in ("openssl", "openssl-libs", "openssl-devel", "openssl-perl"):
                with self.subTest(patch=patch, name=name):
                    result = advisory.verdict([openssl(f"3.5.{patch}", name=name)])
                    self.assertFalse(result["passed"])
        for architecture in ("aarch64", "x86_64", "noarch"):
            with self.subTest(architecture=architecture):
                self.assertFalse(advisory.verdict([openssl(architecture=architecture)])["passed"])

    def test_fixed_patch_passes_without_relying_on_rpm_release_or_epoch(self) -> None:
        """Only the upstream patch determines this advisory; RPM suffixes are not backports."""
        for version, expected in (("3.5.8", False), ("3.5.9", True), ("3.5.10", True)):
            for release in ("1.fc44", "999.fc44", "1.fc44.security"):
                for epoch in (0, 1, 2):
                    with self.subTest(version=version, release=release, epoch=epoch):
                        result = advisory.verdict([openssl(version, release=release, epoch=epoch)])
                        self.assertEqual(result["passed"], expected)

    def test_grype_findings_and_waivers_cannot_hide_independent_block(self) -> None:
        """Existing scanner evidence survives; a scanner waiver cannot waive this review."""
        waived = ExceptionRecord(
            "grype",
            "CVE-fixture",
            "pkg:rpm/fedora/fixture@1",
            "owner",
            "review",
            "scope",
            NOW.date(),
            "https://example.org/review/1",
        )
        result = policy.vulnerability_verdict(vulnerability(), [], [openssl()])
        self.assertEqual(result["matches"], 1)
        self.assertEqual(len(array_value(result["blocked"])), 2)
        result = policy.vulnerability_verdict(vulnerability(), [waived], [openssl()])
        self.assertFalse(result["passed"])
        self.assertEqual(len(array_value(result["blocked"])), 1)
        self.assertEqual(len(array_value(result["waived"])), 1)
        same_finding = ExceptionRecord(
            "grype",
            "CVE-2026-84782",
            str(openssl()["purl"]),
            "owner",
            "review",
            "scope",
            NOW.date(),
            "https://example.org/review/1",
        )
        result = policy.vulnerability_verdict(empty_grype(), [same_finding], [openssl()])
        self.assertFalse(result["passed"])
        self.assertEqual(result["waived"], [])

    def test_unknown_series_and_malformed_versions_fail_closed(self) -> None:
        """No generic comparison guesses applicability outside the current reviewed series."""
        for version in ("3.4.8", "3.6.5", "4.0.3", "3.5.9-beta1", "3.5.09", "3.5", "3.5.9.0"):
            with self.subTest(version=version), self.assertRaises(ToolError):
                _ = advisory.verdict([openssl(version)])

    def test_inconsistent_identity_fields_and_missing_metadata_fail(self) -> None:
        """Version/source/PURL confusion cannot turn an affected package into a passing one."""
        changes: tuple[tuple[str, JsonValue], ...] = (
            ("name", "other"),
            ("version", "1:3.5.9-1.fc44"),
            ("metadataType", "binary"),
            ("metadata", {}),
            ("purl", str(openssl()["purl"]).replace("fedora-44", "fedora-45")),
            ("purl", str(openssl()["purl"]) + "&epoch=2"),
            ("purl", str(openssl()["purl"]).replace("fedora/", "other/")),
        )
        for field, replacement in changes:
            value = openssl()
            value[field] = replacement
            with self.subTest(field=field), self.assertRaises((ToolError, KeyError, ValueError)):
                _ = advisory.verdict([value])
        for field, replacement in (
            ("name", "other"),
            ("version", "3.5.9"),
            ("epoch", True),
            ("sourceRpm", "other-3.5.8-1.fc44.src.rpm"),
            ("architecture", "unknown"),
        ):
            value = openssl()
            object_value(value["metadata"])[field] = replacement
            with self.subTest(field=field), self.assertRaises((ToolError, KeyError, ValueError)):
                _ = advisory.verdict([value])

    def test_non_rpm_native_input_is_not_misrepresented_as_a_fedora_package(self) -> None:
        """The native source pin has its own receipt; this safeguard makes no invented CPE."""
        value: JsonObject = {"name": "OpenSSL", "type": "binary", "version": "3.5.9"}
        result = advisory.verdict([value])
        self.assertTrue(result["passed"])
        self.assertEqual(result["assessed"], [])

    def test_policy_changes_missing_files_links_and_oversize_fail_closed(self) -> None:
        """The reviewed official source/range contract is mandatory, bounded local evidence."""
        original = object_value(decode_json(advisory.POLICY_PATH.read_text()))
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "policy.json"
            changes: tuple[tuple[str, JsonValue], ...] = (
                ("id", "CVE-other"),
                ("severity", "Low"),
                ("schemaVersion", True),
                ("sources", []),
                ("scope", {**object_value(original["scope"]), "fixedVersion": "3.5.8"}),
            )
            for field, replacement in changes:
                changed = copy.deepcopy(original)
                changed[field] = replacement
                _ = path.write_text(json.dumps(changed))
                with self.subTest(field=field), self.assertRaises((ToolError, ValueError)):
                    _ = advisory.verdict([], path=path)
            path.unlink()
            with self.assertRaises(OSError):
                _ = advisory.verdict([], path=path)
            path.symlink_to(advisory.POLICY_PATH)
            with self.assertRaises((ToolError, OSError)):
                _ = advisory.verdict([], path=path)
            path.unlink()
            _ = path.write_bytes(b" " * (advisory.MAX_POLICY + 1))
            with self.assertRaises(ToolError):
                _ = advisory.verdict([], path=path)


_ = ROOT
