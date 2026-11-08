"""Review the public RPM index regions without exempting database files or new content."""

from __future__ import annotations

import copy
import unittest
from datetime import date
from typing import TYPE_CHECKING

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as ledger
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

    from security_policy import ExceptionRecord

EVIDENCE = ROOT / "security/secret-evidence/fedora-rpm-2026-09-30-pr6.json"
HISTORICAL = ROOT / "security/secret-evidence/fedora-rpm-2026-09-30.json"
REFRESH_EVIDENCE = ROOT / "security/secret-evidence/fedora-rpm-2026-10-19.json"
DIRECT_EVIDENCE = ROOT / "security/secret-evidence/fedora-rpm-2026-10-29.json"
CURRENT_EVIDENCE = ROOT / "security/secret-evidence/fedora-rpm-2026-11-08.json"
BASE_EVIDENCE = ROOT / "security/secret-evidence/fedora-base-2026-09-30.json"
DATABASE = "001/usr/lib/sysimage/rpm/rpmdb.sqlite"
TODAY = date(2026, 9, 30)


def dormant_fingerprints(evidence_path: Path = REFRESH_EVIDENCE) -> set[str]:
    """Return the database regions a refreshed index no longer contains."""
    trigger = object_value(object_value(image.report(evidence_path))["trigger"])
    return {
        string_value(item)
        for item in array_value(trigger["dormantFingerprintsRemovedFromActiveLedger"])
    }


def observation(
    *, replay: bool = False, evidence_path: Path = EVIDENCE
) -> tuple[list[JsonValue], JsonObject, JsonObject]:
    """Reconstruct only redacted current-contract observations from retained source evidence."""
    evidence = object_value(image.report(evidence_path))
    canonical = object_value(evidence["file"])
    name = string_value(canonical["neutralName"])
    identity = object_value(object_value(evidence["replay"])["file"]) if replay else canonical
    spans = [
        object_value(raw)["replaySpan" if replay else "span"]
        for raw in array_value(evidence["findings"])
    ]
    rows: list[JsonValue] = []
    for raw in spans:
        span = object_value(raw)
        row: JsonObject = {"File": name, "RuleID": span["rule"]}
        for field in ("startLine", "endLine", "startColumn", "endColumn"):
            row[field[0].upper() + field[1:]] = span[field]
        rows.append(row)
    return rows, {name: identity}, {"format": evidence["matchFormat"], "findings": spans}


def current_observation(*, replay: bool = False) -> tuple[list[JsonValue], JsonObject, JsonObject]:
    """Reconstruct the 2026-11-08 multi-file layer-001 observation, canonical or replayed."""
    evidence = object_value(image.report(CURRENT_EVIDENCE))
    paths: JsonObject = {}
    names: dict[str, str] = {}
    for raw in array_value(evidence["files"]):
        file = object_value(raw)
        name = string_value(file["neutralName"])
        names[string_value(file["path"])] = name
        source = object_value(file["replay"]) if replay else file
        paths[name] = {
            "path": file["path"],
            **{
                key: source[key]
                for key in ("sha256", "sourceBytes", "projectionSha256", "projectionBytes")
            },
            "projectionFormat": source["projectionFormat"],
        }
    spans: list[JsonValue] = []
    rows: list[JsonValue] = []
    for raw in array_value(evidence["findings"]):
        span = object_value(object_value(raw)["replaySpan" if replay else "span"])
        spans.append(span)
        row: JsonObject = {"File": names[string_value(span["path"])], "RuleID": span["rule"]}
        for field in ("startLine", "endLine", "startColumn", "endColumn"):
            row[field[0].upper() + field[1:]] = span[field]
        rows.append(row)
    return rows, paths, {"format": evidence["matchFormat"], "findings": spans}


def current_findings() -> list[JsonObject]:
    """Return the eight reviewed 2026-11-08 regions."""
    return [
        object_value(raw)
        for raw in array_value(object_value(image.report(CURRENT_EVIDENCE))["findings"])
    ]


class RpmSecretReviewTests(unittest.TestCase):
    """Exact canonical spans are reviewed; arbitrary RPM metadata remains blocking."""

    def test_each_current_region_requires_its_individual_review(self) -> None:
        """Removing any current review leaves exactly that observation blocking."""
        rows, paths, spans = current_observation()
        current = ledger.read_exceptions(today=TODAY)
        self.assertEqual(len(rows), 8)
        self.assertEqual(
            len(array_value(image.secret_verdict(rows, [], paths, spans)["blocked"])), 8
        )
        result = image.secret_verdict(rows, current, paths, spans)
        self.assertTrue(result["passed"])
        waived = array_value(result["waived"])
        self.assertEqual(len(waived), 8)
        self.assertEqual(
            {string_value(object_value(raw)["fingerprint"]) for raw in waived},
            {string_value(finding["fingerprint"]) for finding in current_findings()},
        )
        for raw in waived:
            finding = object_value(raw)
            remaining = [item for item in current if item.fingerprint != finding["fingerprint"]]
            with self.subTest(fingerprint=finding["fingerprint"]):
                blocked_now = {
                    string_value(object_value(item)["fingerprint"])
                    for item in array_value(
                        image.secret_verdict(rows, remaining, paths, spans)["blocked"]
                    )
                }
                self.assertEqual(blocked_now, {string_value(finding["fingerprint"])})

    def test_changed_region_length_rule_or_path_cannot_inherit_review(self) -> None:
        """A different candidate in the same database or public data elsewhere remains blocked."""
        current = ledger.read_exceptions(today=TODAY)
        observations = [current_observation(), current_observation(replay=True)]
        for rows, paths, spans in observations:
            self.assert_changed_regions_blocked(rows, paths, spans, current)

    def assert_changed_regions_blocked(
        self,
        rows: list[JsonValue],
        paths: JsonObject,
        spans: JsonObject,
        current: Sequence[ExceptionRecord],
    ) -> None:
        """Reject any region whose digest, length, rule or path differs from its review."""
        for raw, raw_span in zip(rows, array_value(spans["findings"]), strict=True):
            finding, original = object_value(raw), object_value(raw_span)
            name = string_value(finding["File"])
            for field, value in (
                ("spanSha256", "a" * 64),
                ("spanBytes", 1),
                ("rule", "private-key" if original["rule"] != "private-key" else "generic-api-key"),
                ("path", "002/etc/private.env"),
            ):
                changed_paths = copy.deepcopy(paths)
                changed_row = dict(finding)
                changed_span = {**original, field: value}
                if field == "rule":
                    changed_row["RuleID"] = value
                if field == "path":
                    object_value(changed_paths[name])["path"] = value
                with self.subTest(fingerprint=image.secret_fingerprint(original), field=field):
                    result = image.secret_verdict(
                        [changed_row],
                        current,
                        changed_paths,
                        {"format": spans["format"], "findings": [changed_span]},
                    )
                    self.assertFalse(result["passed"])

    def test_replay_and_canonical_bind_same_regions_with_different_complete_files(self) -> None:
        """The observed transaction differences do not change exact public match identities."""
        current = ledger.read_exceptions(today=TODAY)
        results = [
            image.secret_verdict(rows, current, paths, spans)
            for rows, paths, spans in (current_observation(), current_observation(replay=True))
        ]
        self.assertTrue(results[0]["passed"])
        self.assertTrue(results[1]["passed"])
        for left, right in zip(
            array_value(results[0]["waived"]), array_value(results[1]["waived"]), strict=True
        ):
            canonical, replay = object_value(left), object_value(right)
            self.assertEqual(canonical["fingerprint"], replay["fingerprint"])
            self.assertEqual(canonical["spanSha256"], replay["spanSha256"])
            self.assertEqual(canonical["spanBytes"], replay["spanBytes"])
            for field in ("startColumn", "endColumn"):
                self.assertEqual(canonical[field], replay[field])
            database = canonical["path"] == DATABASE
            for field in ("fileSha256", "projectionSha256", "startLine", "endLine"):
                with self.subTest(fingerprint=canonical["fingerprint"], field=field):
                    if database:
                        self.assertNotEqual(canonical[field], replay[field])
                    else:
                        self.assertEqual(canonical[field], replay[field])
        # Historical replays still bind the same identities, now uniformly inactive.
        for evidence_path, same_coordinates in ((EVIDENCE, True), (REFRESH_EVIDENCE, False)):
            historical = [
                image.secret_verdict(rows, current, paths, spans)
                for rows, paths, spans in (
                    observation(evidence_path=evidence_path),
                    observation(replay=True, evidence_path=evidence_path),
                )
            ]
            self.assertFalse(historical[0]["passed"])
            self.assertFalse(historical[1]["passed"])
            self.assertEqual(array_value(historical[0]["waived"]), [])
            for left, right in zip(
                array_value(historical[0]["blocked"]),
                array_value(historical[1]["blocked"]),
                strict=True,
            ):
                canonical, replay = object_value(left), object_value(right)
                self.assertEqual(canonical["fingerprint"], replay["fingerprint"])
                self.assertNotEqual(canonical["fileSha256"], replay["fileSha256"])
                with self.subTest(evidence=evidence_path.name):
                    if same_coordinates:
                        self.assertEqual(canonical["startLine"], replay["startLine"])
                    else:
                        self.assertNotEqual(canonical["startLine"], replay["startLine"])

    def test_superseded_fingerprints_are_historical_not_active_reviews(self) -> None:
        """Every earlier database observation remains auditable without current permission."""
        current = ledger.read_exceptions(today=TODAY)
        active = {item.fingerprint for item in current}
        observed: dict[str, set[str]] = {}
        for evidence_path in (HISTORICAL, EVIDENCE, REFRESH_EVIDENCE, DIRECT_EVIDENCE):
            rows, paths, spans = observation(evidence_path=evidence_path)
            previous = {
                image.secret_fingerprint(object_value(raw))
                for raw in array_value(spans["findings"])
            }
            observed[evidence_path.name] = previous
            with self.subTest(evidence=evidence_path.name):
                self.assertTrue(previous.isdisjoint(active))
                result = image.secret_verdict(rows, current, paths, spans)
                self.assertFalse(result["passed"])
                self.assertEqual(len(array_value(result["blocked"])), len(previous))
                self.assertEqual(array_value(result["waived"]), [])
        self.assertEqual(
            [len(observed[path.name]) for path in (HISTORICAL, EVIDENCE, REFRESH_EVIDENCE)],
            [4, 4, 2],
        )
        first_dormant = dormant_fingerprints()
        self.assertEqual(len(first_dormant), 2)
        self.assertTrue(first_dormant <= observed[EVIDENCE.name])
        refreshed = object_value(object_value(image.report(REFRESH_EVIDENCE))["trigger"])
        recurring = {
            string_value(item) for item in array_value(refreshed["recurringReviewedFingerprints"])
        }
        self.assertEqual(recurring | first_dormant, observed[EVIDENCE.name])
        # The six regions active before 2026-11-08 are exactly the ones its refresh retires.
        retired = dormant_fingerprints(CURRENT_EVIDENCE)
        self.assertEqual(
            retired,
            recurring | observed[REFRESH_EVIDENCE.name] | observed[DIRECT_EVIDENCE.name],
        )
        self.assertEqual(len(retired), 6)
        direct = object_value(object_value(image.report(DIRECT_EVIDENCE))["trigger"])
        self.assertTrue(
            {string_value(item) for item in array_value(direct["recurringReviewedFingerprints"])}
            <= retired
        )
        self.assertTrue(retired.isdisjoint(active))
        now = object_value(object_value(image.report(CURRENT_EVIDENCE))["trigger"])
        self.assertEqual(array_value(now["recurringReviewedFingerprints"]), [])
        self.assertTrue(
            retired.isdisjoint(
                string_value(finding["fingerprint"]) for finding in current_findings()
            )
        )

    def test_complete_source_regions_have_bounded_structural_coverage(self) -> None:
        """Every detected byte is accounted for, including the adjacent deleted-record block."""
        evidence = object_value(image.report(EVIDENCE))
        replay = object_value(evidence["replay"])
        self.assertTrue(replay["wholeProjectionEqual"])
        self.assertFalse(replay["wholeDatabaseEqual"])
        self.assertEqual(replay["installedRuntimePackageIdentitiesEqual"], 148)
        regions = [object_value(raw) for raw in array_value(evidence["findings"])]
        for region in regions:
            context = object_value(region["context"])
            span = object_value(region["span"])
            self.assertEqual(context["coveredSourceBytes"], span["spanBytes"])
            self.assertEqual(context["spanSha256"], span["spanSha256"])
            self.assertTrue(context["allBytesAreFilenameIndexRecordsOrFreeblockMetadata"])
            self.assertEqual(context["index"], "Basenames_key_idx")
            for raw in array_value(context["records"]):
                self.assertEqual(object_value(raw)["package"], "fedora-gpg-keys")
        freeblocks = [
            object_value(raw)
            for region in regions
            for raw in array_value(object_value(region["context"])["freeblocks"])
        ]
        self.assertEqual(len(freeblocks), 1)
        self.assertEqual(freeblocks[0]["bytes"], 34)
        self.assertEqual(
            object_value(freeblocks[0]["authenticatedBaseLiveRecord"])["package"],
            "fedora-gpg-keys",
        )

    def test_refreshed_regions_have_bounded_structural_coverage(self) -> None:
        """Both 2026-10-19 regions are live signing-key filename records plus one freed copy."""
        evidence = object_value(image.report(REFRESH_EVIDENCE))
        replay = object_value(evidence["replay"])
        self.assertFalse(replay["wholeProjectionEqual"])
        self.assertFalse(replay["wholeDatabaseEqual"])
        self.assertTrue(replay["bothCompleteMatchDigestsAndLengthsEqual"])
        self.assertEqual(replay["installedRuntimePackageIdentitiesEqual"], 148)
        self.assertEqual(replay["sqliteQuickCheck"], "ok")
        self.assertEqual(replay["scannerVersion"], "8.30.1")
        regions = [object_value(raw) for raw in array_value(evidence["findings"])]
        self.assertEqual(len(regions), 2)
        self.assertEqual(object_value(evidence["package"])["name"], "fedora-gpg-keys")
        for region in regions:
            context = object_value(region["context"])
            span = object_value(region["span"])
            self.assertEqual(span["path"], DATABASE)
            self.assertEqual(span["rule"], "generic-api-key")
            self.assertEqual(context["coveredSourceBytes"], span["spanBytes"])
            self.assertEqual(context["spanSha256"], span["spanSha256"])
            self.assertTrue(context["allBytesAreFilenameIndexRecordsOrFreeblockMetadata"])
            self.assertEqual(context["index"], "Basenames_key_idx")
            self.assertEqual(image.secret_fingerprint(span), string_value(region["fingerprint"]))
            for raw in array_value(context["records"]):
                record = object_value(raw)
                self.assertEqual(record["package"], "fedora-gpg-keys")
                self.assertRegex(string_value(record["filenameSha256"]), r"^[a-f0-9]{64}$")
        freeblocks = [
            object_value(raw)
            for region in regions
            for raw in array_value(object_value(region["context"])["freeblocks"])
        ]
        self.assertEqual(len(freeblocks), 1)
        self.assertEqual(freeblocks[0]["bytes"], 32)
        self.assertTrue(freeblocks[0]["coveredContentIsTheLiveFilenameBytes"])
        self.assertEqual(
            object_value(freeblocks[0]["authenticatedBaseLiveRecord"])["package"],
            "fedora-gpg-keys",
        )

    def test_current_database_regions_have_bounded_structural_coverage(self) -> None:
        """All four 2026-11-08 regions are live public key-file index records or freed copies."""
        evidence = object_value(image.report(CURRENT_EVIDENCE))
        replay = object_value(evidence["replay"])
        self.assertFalse(replay["wholeDatabaseEqual"])
        self.assertFalse(replay["wholeProjectionEqual"])
        self.assertTrue(replay["allFingerprintsEqualCanonicalBlocked"])
        self.assertTrue(replay["profileAndMagicFilesWholeFileEqual"])
        self.assertEqual(replay["installedRuntimePackageIdentitiesEqual"], 148)
        self.assertEqual(replay["sqliteIntegrityCheck"], "ok")
        self.assertEqual(replay["scannerVersion"], "8.30.1")
        self.assertFalse(evidence["canonicalImagePassed"])
        regions = [
            item for item in current_findings() if object_value(item["span"])["path"] == DATABASE
        ]
        self.assertEqual(len(regions), 4)
        self.assertEqual(
            sorted(integer_value(object_value(region["span"])["spanBytes"]) for region in regions),
            [58, 58, 58, 62],
        )
        freeblocks = 0
        for region in regions:
            context = object_value(region["context"])
            span = object_value(region["span"])
            with self.subTest(fingerprint=region["fingerprint"]):
                self.assertEqual(span["rule"], "generic-api-key")
                self.assertEqual(image.secret_fingerprint(span), region["fingerprint"])
                self.assertEqual(context["coveredSourceBytes"], span["spanBytes"])
                self.assertEqual(context["rawBytes"], span["spanBytes"])
                self.assertTrue(context["allBytesAreFilenameIndexRecordsOrFreeblockMetadata"])
                self.assertTrue(context["projectionOfRawEqualsMatch"])
                self.assertEqual(context["btree"], "Basenames_key_idx")
                records = [object_value(raw) for raw in array_value(context["liveRecords"])]
                self.assertTrue(records)
                for record in records:
                    self.assertEqual(record["package"], "fedora-gpg-keys")
                    self.assertEqual(record["packageVersion"], "44-2")
                    bases = [object_value(raw) for raw in array_value(record["baseLiveRecords"])]
                    self.assertTrue(bases)
                    for base in bases:
                        self.assertEqual(base["package"], "fedora-gpg-keys")
                        self.assertEqual(base["packageVersion"], "44-1")
                        self.assertEqual(base["filenameSha256"], record["filenameSha256"])
                for raw in array_value(context["freeblocks"]):
                    block = object_value(raw)
                    freeblocks += 1
                    self.assertTrue(block["headerFullyCovered"])
                    self.assertTrue(block["coveredContentEqualsLiveFilenameInSameRegion"])
                    self.assertTrue(block["coveredContentIsCompleteLiveKeyInCurrentDatabase"])
                    self.assertTrue(block["coveredContentIsCompleteLiveKeyInImmutableBase"])
                candidate = object_value(region["candidateContext"])
                self.assertRegex(string_value(candidate["candidateSha256"]), r"^[a-f0-9]{64}$")
        self.assertEqual(freeblocks, 2)

    def test_refreshed_layer_copies_repeat_reviewed_base_bytes_at_their_own_path(self) -> None:
        """Byte-identical base regions at the 001/ path need, and have, separate exact reviews."""
        base = object_value(image.report(BASE_EVIDENCE))
        reviewed = {
            string_value(object_value(finding)["fingerprint"]): object_value(
                object_value(finding)["span"]
            )
            for raw in array_value(base["files"])
            for finding in array_value(object_value(raw)["findings"])
        }
        current = ledger.read_exceptions(today=TODAY)
        rows, paths, spans = current_observation()
        copies = [
            item for item in current_findings() if object_value(item["span"])["path"] != DATABASE
        ]
        self.assertEqual(len(copies), 4)
        for finding in copies:
            span = object_value(finding["span"])
            context = object_value(finding["context"])
            counterpart = object_value(context["reviewedBaseCounterpart"])
            original = reviewed[string_value(counterpart["fingerprint"])]
            with self.subTest(fingerprint=finding["fingerprint"]):
                self.assertEqual(original["path"], "000/" + string_value(span["path"])[4:])
                for field in ("rule", "spanBytes", "spanSha256"):
                    self.assertEqual(original[field], span[field])
                self.assertNotEqual(counterpart["fingerprint"], finding["fingerprint"])
                self.assertIn(counterpart["fingerprint"], {item.fingerprint for item in current})
                without = [item for item in current if item.fingerprint != finding["fingerprint"]]
                blocked = {
                    string_value(object_value(item)["fingerprint"])
                    for item in array_value(
                        image.secret_verdict(rows, without, paths, spans)["blocked"]
                    )
                }
                self.assertEqual(blocked, {string_value(finding["fingerprint"])})
                entry = next(item for item in current if item.fingerprint == finding["fingerprint"])
                self.assertEqual(entry.scope, span["path"])

    def test_additional_database_finding_is_not_covered(self) -> None:
        """Reviewing the known public regions never exempts another match in the file."""
        rows, paths, spans = current_observation()
        database = next(
            index
            for index, raw in enumerate(array_value(spans["findings"]))
            if object_value(raw)["path"] == DATABASE
        )
        extra = {**object_value(array_value(spans["findings"])[database]), "spanSha256": "b" * 64}
        result = image.secret_verdict(
            [*rows, rows[database]],
            ledger.read_exceptions(today=TODAY),
            paths,
            {"format": spans["format"], "findings": [*array_value(spans["findings"]), extra]},
        )
        self.assertFalse(result["passed"])
        self.assertEqual(len(array_value(result["waived"])), 8)
        self.assertEqual(len(array_value(result["blocked"])), 1)


if __name__ == "__main__":
    _ = unittest.main()
