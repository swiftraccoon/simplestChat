"""Defensive archive fixtures: untrusted metadata never controls a host destination."""

from __future__ import annotations

import hashlib
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split

import bounded_process
import capacity
import capacity_artifacts as artifacts


def archive(path: Path, members: list[tarfile.TarInfo]) -> None:
    """Build a small inert archive; only regular entries receive fixture bytes."""
    with tarfile.open(path, "w") as bundle:
        for member in members:
            bundle.addfile(member, io.BytesIO(b"{}") if member.isfile() else None)


def regular(name: str) -> tarfile.TarInfo:
    """Create one expected-size regular JSON member."""
    member = tarfile.TarInfo(name)
    member.size = 2
    return member


class CapacityArtifactTests(unittest.TestCase):
    """Transfer success, safe file kinds, exact names and bounds are all required."""

    def test_regular_files_have_host_owned_names_permissions_and_digests(self) -> None:
        """The current manifest records both required JSON files with verified hashes."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / "input.tar"
            archive(source, [regular("./" + name) for name in sorted(artifacts.REQUIRED)])
            result = artifacts.extract(source, directory / "generator")
            self.assertTrue(result["complete"])
            for path in (directory / "generator").iterdir():
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(artifacts.read_regular(path), b"{}")
            self.assertIn(hashlib.sha256(b"{}").hexdigest(), json.dumps(result))

    def test_links_special_files_duplicates_and_traversal_are_rejected_before_writes(self) -> None:
        """Metadata validation finishes before any generator file is materialized."""
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            member = tarfile.TarInfo("load_test_summary.json")
            member.type = kind
            member.linkname = "fixture-only"
            with self.subTest(kind=kind):
                self.rejected([member])
        for name in (
            "../fixture.json",
            "/fixture.json",
            "generator.log",
            "step.json",
            "generator/nested.json",
            "././load_test_summary.json",
        ):
            with self.subTest(name=name):
                self.rejected([regular(name)])
        self.rejected([regular("load_test_summary.json"), regular("./load_test_summary.json")])

    def rejected(self, additions: list[tarfile.TarInfo]) -> None:
        """Reject the whole archive, including valid files that precede an invalid member."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / "input.tar"
            archive(source, [regular("load_test_results.json"), *additions])
            with self.assertRaises(artifacts.ArtifactError):
                _ = artifacts.extract(source, directory / "generator")
            self.assertEqual(list((directory / "generator").iterdir()), [])

    def test_missing_files_timeout_marker_and_size_budget_cannot_certify_collection(self) -> None:
        """A generator watchdog report and partial evidence remain failed measurements."""
        self.rejected([])
        self.rejected([regular("load_test_summary.json"), regular("load_test_timeout.json")])
        with patch.dict(artifacts.EXPECTED, {"load_test_summary.json": 1}):
            self.rejected([regular("load_test_summary.json")])

    def test_failed_transfer_does_not_extract_even_complete_files(self) -> None:
        """Copy exit status is required independently of otherwise usable archive content."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with (
                patch.object(bounded_process, "run", return_value=(1, b"", b"")),
                self.assertRaisesRegex(artifacts.ArtifactError, "result_transfer_failed"),
            ):
                artifacts.collect("unused", "fixture", directory)
            self.assertFalse((directory / "generator").exists())
            self.assertFalse((directory / "collection.json").exists())

    def test_physical_metadata_limits_precede_general_tar_parsing(self) -> None:
        """Reject excessive extended metadata from its header without reading its body."""
        metadata = tarfile.TarInfo("metadata")
        metadata.type = tarfile.XHDTYPE
        metadata.size = 9
        with (
            patch.object(artifacts, "MAX_TAR_METADATA", 8),
            self.assertRaisesRegex(artifacts.ArtifactError, "oversized_tar_body"),
        ):
            artifacts.check_headers(io.BytesIO(metadata.tobuf()), 8, 1024)
        with self.assertRaisesRegex(artifacts.ArtifactError, "truncated_tar_body"):
            artifacts.check_headers(io.BytesIO(regular("result").tobuf()), 8, 1024)
        with self.assertRaisesRegex(artifacts.ArtifactError, "trailing_tar_data"):
            artifacts.check_headers(io.BytesIO(bytes(1024) + b"extra"), 8, 1024)

    def test_safe_read_refuses_links_and_nonregular_files(self) -> None:
        """Read helpers defend independently, even outside the normal extractor flow."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            link = directory / "linked"
            link.symlink_to(ROOT / "README.md")
            with self.assertRaises(OSError):
                _ = artifacts.read_regular(link)
            with self.assertRaises(artifacts.ArtifactError):
                _ = artifacts.read_regular(directory)

    def test_collection_failure_invalidates_step_and_cleanup_still_runs(self) -> None:
        """The run-level regression covers the former ignored-copy-status boundary."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            result = capacity.StepResult("meetings", 30, 30, 1, "fixture", generator_passed=True)
            with (
                patch.object(
                    capacity,
                    "collect",
                    side_effect=artifacts.ArtifactError("result_transfer_failed"),
                ),
                patch.object(capacity.Engine, "remove") as remove,
            ):
                capacity.finalize_step(
                    capacity.Engine("unused"),
                    ("owned-server", "owned-generator"),
                    directory,
                    result,
                )
            self.assertIn("evidence collection failed", result.error)
            self.assertEqual(
                [call.args for call in remove.call_args_list],
                [("owned-generator",), ("owned-server",)],
            )


if __name__ == "__main__":
    _ = unittest.main()
