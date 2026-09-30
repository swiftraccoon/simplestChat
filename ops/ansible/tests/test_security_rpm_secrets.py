"""Review four public RPM index regions without exempting database files or new content."""

from __future__ import annotations

import copy
import unittest
from datetime import date

from test_support import ROOT

# isort: split
import security_image_policy as image
import security_policy as ledger
from release_json import JsonObject, JsonValue, array_value, object_value, string_value

EVIDENCE = ROOT / "security/secret-evidence/fedora-rpm-2026-09-30.json"
TODAY = date(2026, 9, 30)


def observation(*, replay: bool = False) -> tuple[list[JsonValue], JsonObject, JsonObject]:
    """Reconstruct only redacted current-contract observations from retained source evidence."""
    evidence = object_value(image.report(EVIDENCE))
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


class RpmSecretReviewTests(unittest.TestCase):
    """Exact canonical spans are reviewed; arbitrary RPM metadata remains blocking."""

    def test_each_canonical_region_requires_its_individual_review(self) -> None:
        """Removing any of the four reviews leaves exactly that observation blocking."""
        rows, paths, spans = observation()
        current = ledger.read_exceptions(today=TODAY)
        self.assertEqual(len(rows), 4)
        self.assertEqual(
            len(array_value(image.secret_verdict(rows, [], paths, spans)["blocked"])), 4
        )
        result = image.secret_verdict(rows, current, paths, spans)
        self.assertTrue(result["passed"])
        self.assertEqual(len(array_value(result["waived"])), 4)
        for raw in array_value(result["waived"]):
            finding = object_value(raw)
            remaining = [item for item in current if item.fingerprint != finding["fingerprint"]]
            with self.subTest(fingerprint=finding["fingerprint"]):
                blocked = array_value(
                    image.secret_verdict(rows, remaining, paths, spans)["blocked"]
                )
                self.assertEqual(len(blocked), 1)
                self.assertEqual(object_value(blocked[0])["fingerprint"], finding["fingerprint"])

    def test_changed_region_length_rule_or_path_cannot_inherit_review(self) -> None:
        """A different candidate in the same database or public data elsewhere remains blocked."""
        rows, paths, spans = observation()
        current = ledger.read_exceptions(today=TODAY)
        for raw, raw_span in zip(rows, array_value(spans["findings"]), strict=True):
            finding, original = object_value(raw), object_value(raw_span)
            name = string_value(finding["File"])
            for field, value in (
                ("spanSha256", "a" * 64),
                ("spanBytes", 1),
                ("rule", "private-key"),
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
        observations = [observation(), observation(replay=True)]
        results = [
            image.secret_verdict(rows, current, paths, spans) for rows, paths, spans in observations
        ]
        for result in results:
            self.assertTrue(result["passed"])
        for left, right in zip(
            array_value(results[0]["waived"]), array_value(results[1]["waived"]), strict=True
        ):
            canonical, replay = object_value(left), object_value(right)
            self.assertEqual(canonical["fingerprint"], replay["fingerprint"])
            self.assertEqual(canonical["spanSha256"], replay["spanSha256"])
            self.assertEqual(canonical["spanBytes"], replay["spanBytes"])
            for field in ("fileSha256", "projectionSha256", "startLine"):
                self.assertNotEqual(canonical[field], replay[field])

    def test_additional_database_finding_is_not_covered(self) -> None:
        """Reviewing all four known public regions never exempts another match in the file."""
        rows, paths, spans = observation()
        extra = {**object_value(array_value(spans["findings"])[0]), "spanSha256": "b" * 64}
        result = image.secret_verdict(
            [*rows, rows[0]],
            ledger.read_exceptions(today=TODAY),
            paths,
            {"format": spans["format"], "findings": [*array_value(spans["findings"]), extra]},
        )
        self.assertFalse(result["passed"])
        self.assertEqual(len(array_value(result["waived"])), 4)
        self.assertEqual(len(array_value(result["blocked"])), 1)


if __name__ == "__main__":
    _ = unittest.main()
