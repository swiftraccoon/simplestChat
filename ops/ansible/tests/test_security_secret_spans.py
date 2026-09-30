"""Replay actual pinned-detector coordinates using inert, never-issued input bytes."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import tempfile
import unittest
from datetime import date
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_image as image
import security_image_policy as policy
import security_secret_projection as projection
import security_secret_spans as spans
from release_json import JsonObject, JsonValue, array_value, object_value
from security_policy import ExceptionRecord
from security_tools import ToolError

_ = ROOT
NAME = "content-000000"
CANARY = (
    "ghp_" + hashlib.sha256(b"never-issued secret-span coordinate fixture").hexdigest()[:36]
).encode()


def fixture(root: Path, content: bytes) -> tuple[Path, JsonObject]:
    """Create the exact image projection mapping without scanner or network execution."""
    source, directory = root / "original", root / "secret-input"
    directory.mkdir()
    _ = source.write_bytes(content)
    projected = projection.project(source, directory / NAME, max_bytes=len(content))
    return directory, {
        NAME: {
            "path": "001/example",
            "sha256": projected.source_sha256,
            "projectionSha256": projected.projection_sha256,
            "projectionBytes": projected.projection_bytes,
            "projectionFormat": projected.format,
        }
    }


def finding(coordinates: tuple[int, int, int, int], rule: str = "github-pat") -> JsonObject:
    """Use coordinates observed with checksum-pinned Gitleaks 8.30.1 and full redaction."""
    return {
        "File": "/layers/" + NAME,
        "RuleID": rule,
        **dict(zip(spans.COORDINATES, coordinates, strict=True)),
        "Match": "REDACTED",
        "Secret": "REDACTED",
    }


class SecretSpanTests(unittest.TestCase):
    """Candidate hashes identify exact projection regions, never an approval or secret value."""

    def test_real_detector_lf_crlf_multiline_and_encoded_coordinates(self) -> None:
        """Reported columns describe the match region, including encoded source when decoded."""
        cases = (
            (b"heading\nTOKEN=" + CANARY + b"\n", (32835, 32835, 8, 47), CANARY),
            (b"heading\r\nTOKEN=" + CANARY + b"\r\n", (32835, 32835, 8, 47), CANARY),
            (
                b"ENCODED=" + base64.b64encode(b"TOKEN=" + CANARY) + b"\n",
                (32834, 32834, 10, 73),
                base64.b64encode(b"TOKEN=" + CANARY),
            ),
            (
                b"heading\nINERT_BEGIN\nnever issued marker\nINERT_END\n",
                (32835, 32837, 2, 10),
                b"INERT_BEGIN\nnever issued marker\nINERT_END",
            ),
        )
        for content, coordinates, expected in cases:
            with self.subTest(coordinates=coordinates), tempfile.TemporaryDirectory() as temporary:
                directory, paths = fixture(Path(temporary), content)
                result = spans.collect([finding(coordinates)], paths, directory)
                row = object_value(array_value(result["findings"])[0])
                self.assertEqual(row["status"], "resolved")
                self.assertEqual(row["spanBytes"], len(expected))
                self.assertEqual(row["spanSha256"], hashlib.sha256(expected).hexdigest())
                self.assertNotIn(CANARY.decode(), json.dumps(result))
                self.assertNotIn("Secret", json.dumps(result))
                self.assertEqual(
                    row["projectionSha256"], object_value(paths[NAME])["projectionSha256"]
                )

    def test_actual_long_line_coordinates_are_ambiguous_not_guessed(self) -> None:
        """Actual scanner columns reset at fragment boundaries on the same logical line."""
        with tempfile.TemporaryDirectory() as temporary:
            directory, paths = fixture(Path(temporary), b"z" * 180000 + b" TOKEN=" + CANARY + b"\n")
            result = spans.collect([finding((32834, 32834, 1870, 1909))], paths, directory)
            row = object_value(array_value(result["findings"])[0])
            self.assertEqual(row["status"], "ambiguous")
            self.assertNotIn("spanSha256", row)

    def test_unresolvable_or_incomplete_coordinates_do_not_claim_a_span(self) -> None:
        """A valid integer tuple is not proof of a region when the scanner cannot locate its end."""
        with tempfile.TemporaryDirectory() as temporary:
            directory, paths = fixture(Path(temporary), b"heading\nTOKEN=" + CANARY + b"\n")
            for coordinates in ((32835, 32835, 8, 0), (99999, 99999, 2, 4), (32835, 32834, 8, 47)):
                result = spans.collect([finding(coordinates)], paths, directory)
                row = object_value(array_value(result["findings"])[0])
                self.assertEqual(row["status"], "unresolved")
                self.assertNotIn("spanSha256", row)

    def test_whole_projection_hash_and_size_are_required(self) -> None:
        """Even a byte changed outside the candidate invalidates this diagnostic binding."""
        with tempfile.TemporaryDirectory() as temporary:
            directory, paths = fixture(Path(temporary), b"heading\nTOKEN=" + CANARY + b"\n")
            target = directory / NAME
            original = target.read_bytes()
            for content in (b"X" + original[1:], original + b"x"):
                _ = target.write_bytes(content)
                with self.assertRaisesRegex(ToolError, "secret_span_file"):
                    _ = spans.collect([finding((32835, 32835, 8, 47))], paths, directory)

    def test_symlinks_special_files_and_unsafe_names_are_refused(self) -> None:
        """Reading untrusted evidence cannot follow a host link or block on a FIFO."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory, paths = fixture(root, b"inert\n")
            target = directory / NAME
            target.unlink()
            target.symlink_to(root / "original")
            with self.assertRaises(OSError):
                _ = spans.collect([finding((1, 1, 1, 2))], paths, directory)
            target.unlink()
            os.mkfifo(target)
            with self.assertRaises(ToolError):
                _ = spans.collect([finding((1, 1, 1, 2))], paths, directory)
            for name in ("/layers/../outside", "/private/" + NAME, "/layers/content-000000/child"):
                with self.subTest(name=name), self.assertRaises(ToolError):
                    _ = spans.collect([{**finding((1, 1, 1, 2)), "File": name}], paths, directory)

    def test_invalid_coordinate_types_and_bounds_are_refused(self) -> None:
        """Booleans, strings and unbounded numbers cannot become public diagnostic fields."""
        for bad in (True, "private-content", -1, spans.MAX_FILE + 1):
            with tempfile.TemporaryDirectory() as temporary:
                directory, paths = fixture(Path(temporary), b"inert\n")
                with self.subTest(value=bad), self.assertRaises(ToolError):
                    _ = spans.collect(
                        [{**finding((1, 1, 1, 2)), "StartColumn": bad}], paths, directory
                    )

    def test_read_candidate_count_and_deadline_limits_fail_closed(self) -> None:
        """Every selected byte and candidate consumes an independent fixed budget."""
        for constant, limit in (
            ("MAX_TOTAL", 1),
            ("MAX_CANDIDATES", 0),
            ("MAX_FINDINGS", 0),
            ("SECONDS", -1),
        ):
            with tempfile.TemporaryDirectory() as temporary, patch.object(spans, constant, limit):
                directory, paths = fixture(Path(temporary), b"heading\nTOKEN=" + CANARY + b"\n")
                with self.subTest(limit=constant), self.assertRaises(ToolError):
                    _ = spans.collect([finding((32835, 32835, 8, 47))], paths, directory)

    def test_added_diagnostics_do_not_change_existing_fingerprint_or_disposition(self) -> None:
        """Preserve exact whole-file matching for both blocked and reviewed cases."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, paths = fixture(root, b"heading\nTOKEN=" + CANARY + b"\n")
            report: list[JsonValue] = [finding((32835, 32835, 8, 47))]
            secrets = root / "secrets"
            secrets.mkdir()
            image.write(root / "secret-paths.json", paths)
            image.write(secrets / "gitleaks.json", report)
            fingerprint = "github-pat:001/example:32835:" + str(object_value(paths[NAME])["sha256"])
            review = ExceptionRecord(
                scanner="gitleaks",
                fingerprint=fingerprint,
                scope="001/example",
                owner="fixture",
                rationale="Never-issued inert test value",
                reachability="Not a credential",
                expires=date(2026, 11, 29),
                review="https://example.invalid/review",
            )
            for reviews in ([], [review]):
                expected = policy.secret_verdict(report, reviews, paths)
                actual = image.secret_checks(secrets, root, reviews)
                _ = actual.pop("projectionSpans")
                self.assertEqual(actual, expected)


if __name__ == "__main__":
    _ = unittest.main()
