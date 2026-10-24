"""Read real temporary publication files through only inert root-ID observations."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import BinaryIO, cast
from unittest.mock import patch

from test_support import ROOT

sys.path.insert(0, str(ROOT / "build"))
import security_vm_fetch_guest as guest

REVISION = "a" * 40
RUN_ID = "b" * 32


def fixture(root: Path) -> guest.Options:
    """Write a private marker, published pair and two finalized receiver receipts."""
    destination = root / "public/releases" / REVISION
    destination.mkdir(parents=True, mode=0o700)
    destination.chmod(0o700)
    marker = root / "marker"
    _ = marker.write_text(RUN_ID + "\n")
    marker.chmod(0o600)
    pins: dict[str, str] = {}
    for name, data in (("image.tar", b"inert image"), ("release.json", b"inert manifest")):
        path = destination / name
        _ = path.write_bytes(data)
        path.chmod(0o600)
        pins[name] = hashlib.sha256(data).hexdigest()
    options = guest.Options(
        run_id=RUN_ID,
        revision=REVISION,
        artifact_id=123,
        zip_sha256="c" * 64,
        zip_bytes=100,
        image_sha256=pins["image.tar"],
        manifest_sha256=pins["release.json"],
        attempts=2,
    )
    for number in (1, 2):
        attempt = destination / ("download." + str(number))
        attempt.mkdir(mode=0o700)
        path = attempt / "fetch-outcome.json"
        _ = path.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "passed": True,
                    "settled": True,
                    "finalized": True,
                    "status": "complete",
                    "phase": "complete",
                    "failureClass": None,
                    "revision": REVISION,
                    "artifactId": 123,
                    "zipSha256": "c" * 64,
                    "artifactZipBytes": 100,
                    "downloadedBytes": 100,
                    "archiveSha256": pins["image.tar"],
                    "manifestSha256": pins["release.json"],
                }
            )
        )
        path.chmod(0o600)
    return options


class FetchGuestProbeTests(unittest.TestCase):
    """The fixed read-only observer must refuse partial/foreign data without any write."""

    def observe(
        self, root: Path, options: guest.Options
    ) -> tuple[dict[str, object], list[BinaryIO]]:
        """Project only root ownership; retain actual opened files for close assertions."""
        streams: list[BinaryIO] = []
        original_lstat, original_fstat, original_fdopen = os.lstat, os.fstat, os.fdopen

        def project(value: os.stat_result) -> os.stat_result:
            values = list(value)
            values[4] = values[5] = 0
            return os.stat_result(values)

        def lstat(
            path: str | bytes | os.PathLike[str] | os.PathLike[bytes], *, dir_fd: int | None = None
        ) -> os.stat_result:
            return project(original_lstat(path, dir_fd=dir_fd))

        def fdopen(descriptor: int, mode: str) -> BinaryIO:
            stream = cast("BinaryIO", original_fdopen(descriptor, mode))
            streams.append(stream)
            return stream

        def fstat(descriptor: int) -> os.stat_result:
            return project(original_fstat(descriptor))

        with (
            patch.object(guest, "PUBLIC", root / "public"),
            patch.object(guest, "MARKER", root / "marker"),
            patch.object(os, "geteuid", return_value=0),
            patch.object(os, "lstat", lstat),
            patch.object(os, "fstat", fstat),
            patch.object(os, "fdopen", fdopen),
        ):
            value = guest.inspect(options)
        return value, streams

    def test_actual_files_hashes_counts_and_original_closes(self) -> None:
        """The observer reports real inode identities and changes no file bytes."""
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            options = fixture(root)
            before = {str(path): path.read_bytes() for path in root.rglob("*") if path.is_file()}
            value, streams = self.observe(root, options)
            after = {str(path): path.read_bytes() for path in root.rglob("*") if path.is_file()}
        self.assertTrue(value["passed"])
        self.assertEqual(value["attempts"], 2)
        self.assertEqual(before, after)
        self.assertTrue(all(stream.closed for stream in streams))
        self.assertNotIn("download.", json.dumps(value))

    def test_partial_or_changed_receipt_never_becomes_publication_proof(self) -> None:
        """Finalized must be literal true and each expected identity/hash must match."""
        for changed in (
            {"finalized": False},
            {"passed": 1},
            {"artifactId": True},
            {"zipSha256": "d" * 64},
            {"downloadedBytes": 99},
        ):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as name:
                root = Path(name).resolve()
                options = fixture(root)
                receipt = root / "public/releases" / REVISION / "download.2/fetch-outcome.json"
                value = guest.object_json(receipt.read_bytes())
                value.update(changed)
                _ = receipt.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    _ = self.observe(root, options)

    def test_wrong_marker_private_mode_and_symlink_are_refused(self) -> None:
        """Never follow a substitute published file or inspect another fixture run."""
        for fault in ("marker", "mode", "symlink"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as name:
                root = Path(name).resolve()
                options = fixture(root)
                image = root / "public/releases" / REVISION / "image.tar"
                if fault == "marker":
                    _ = (root / "marker").write_text("foreign\n")
                elif fault == "mode":
                    image.chmod(0o644)
                else:
                    image.unlink()
                    image.symlink_to(root / "marker")
                with self.assertRaises((ValueError, OSError)):
                    _ = self.observe(root, options)

    def test_ambiguous_or_nonfinite_json_is_refused(self) -> None:
        """The probe never projects arbitrary journal parsing into a success."""
        for data in (b'{"passed":true,"passed":false}', b'{"seconds":NaN}', b"[]"):
            with self.subTest(data=data), self.assertRaises(ValueError):
                _ = guest.object_json(data)


if __name__ == "__main__":
    _ = unittest.main()
