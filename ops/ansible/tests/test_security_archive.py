"""Real inert image archives exercise bounded layer handling and exact rootfs semantics."""

from __future__ import annotations

import gzip
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

import security_archive as archive

REVISION = "a" * 40
TAG = "simplestchat-release/production:" + REVISION


def member(
    name: str, data: bytes = b"", *, kind: bytes = tarfile.REGTYPE, link: str = ""
) -> tuple[tarfile.TarInfo, bytes]:
    """Create intentionally privileged archive attributes that extraction must ignore."""
    info = tarfile.TarInfo(name)
    info.type, info.linkname, info.size = kind, link, len(data)
    info.mode, info.uid, info.gid = 0o7777, 0, 0
    return info, data


def tar(members: list[tuple[tarfile.TarInfo, bytes]]) -> bytes:
    """Serialize inert members with real tar headers and padding."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as bundle:
        for info, data in members:
            bundle.addfile(info, io.BytesIO(data) if info.isfile() else None)
    return output.getvalue()


def saved_image(
    directory: Path, layers: list[bytes], *, compressed: bool = False
) -> tuple[Path, Path]:
    """Build the current canonical release manifest around exact Docker save bytes."""
    image = directory / "image.tar"
    manifest = directory / "release.json"
    names = [f"{index}/layer.tar" for index in range(len(layers))]
    config = {
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "User": "10001:10001",
            "Cmd": ["/app/simplestChat"],
            "Labels": {"org.opencontainers.image.revision": REVISION},
            "Env": ["INERT_FIXTURE=retained-for-secret-scan"],
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": ["sha256:" + hashlib.sha256(layer).hexdigest() for layer in layers],
        },
    }
    members = [
        member(
            "manifest.json",
            json.dumps([{"Config": "config.json", "RepoTags": [TAG], "Layers": names}]).encode(),
        ),
        member("config.json", json.dumps(config).encode()),
    ]
    members.extend(
        member(name, gzip.compress(layer, mtime=0) if compressed else layer)
        for name, layer in zip(names, layers, strict=True)
    )
    _ = image.write_bytes(tar(members))
    _ = manifest.write_text(
        json.dumps(
            {
                "schemaVersion": 1,
                "revision": REVISION,
                "platform": "linux/amd64",
                "archiveSha256": hashlib.sha256(image.read_bytes()).hexdigest(),
                "imageTag": TAG,
                "migrations": {"1": "a" * 96},
                "createdAt": "2026-09-30T00:00:00Z",
            }
        )
    )
    return image, manifest


class SecurityArchiveTests(unittest.TestCase):
    """Every selected byte must match a bounded layer, including removed content."""

    def test_whiteouts_preserve_secret_evidence_and_final_tree_matches_layers(self) -> None:
        """Removed lower-layer files remain scanned while final ELF inputs see the replacement."""
        lower = tar(
            [member("app/removed.env", b"fixture-secret-candidate"), member("app/current", b"old")]
        )
        upper = tar([member("app/current", b"new"), member("app/.wh.removed.env")])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [lower, upper])
            output = directory / "evidence"
            report = archive.extract(image, manifest, output)
            self.assertTrue(report["passed"])
            with tarfile.open(image, "r:") as bundle:
                source = bundle.extractfile("config.json")
                self.assertIsNotNone(source)
                assert source is not None  # noqa: S101 -- Fixture narrowing after assertion.
                with source:
                    config_bytes = source.read()
            expected = hashlib.sha256(config_bytes).hexdigest()
            self.assertEqual(report["configSha256"], expected)
            self.assertEqual(report["imageId"], "sha256:" + expected)
            self.assertEqual(archive.image_identity(image), report["imageId"])
            self.assertEqual((output / "layers/image-config.json").read_bytes(), config_bytes)
            self.assertEqual((output / "rootfs/app/current").read_bytes(), b"new")
            self.assertFalse((output / "rootfs/app/removed.env").exists())
            self.assertEqual(
                (output / "layers/000/app/removed.env").read_bytes(), b"fixture-secret-candidate"
            )
            self.assertIn("INERT_FIXTURE", (output / "layers/image-config.json").read_text())
            self.assertEqual((output / "rootfs/app/current").stat().st_mode & 0o7777, 0o600)

    def test_opaque_whiteout_applies_to_prior_layer_even_when_listed_last(self) -> None:
        """Whiteout processing cannot accidentally remove a new file from the same layer."""
        lower = tar([member("directory/old", b"old")])
        upper = tar([member("directory/new", b"new"), member("directory/.wh..wh..opq")])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [lower, upper])
            _ = archive.extract(image, manifest, directory / "evidence")
            self.assertEqual(
                [path.name for path in (directory / "evidence/rootfs/directory").iterdir()], ["new"]
            )

    def test_hardlink_keeps_original_bytes_when_later_layer_replaces_target(self) -> None:
        """Hardlinks bind to the layer's inode content rather than the final target name."""
        lower = tar(
            [
                member("lib/real", b"original"),
                member("lib/alias", kind=tarfile.LNKTYPE, link="lib/real"),
            ]
        )
        upper = tar([member("lib/real", b"replacement")])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [lower, upper])
            _ = archive.extract(image, manifest, directory / "evidence")
            self.assertEqual((directory / "evidence/rootfs/lib/real").read_bytes(), b"replacement")
            self.assertEqual((directory / "evidence/rootfs/lib/alias").read_bytes(), b"original")

    def test_absolute_symlink_is_rewritten_inside_private_rootfs(self) -> None:
        """A valid absolute image link never becomes a link to the controller host."""
        layer = tar(
            [
                member("usr/lib/real", b"library"),
                member("lib", kind=tarfile.SYMTYPE, link="/usr/lib"),
            ]
        )
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [layer], compressed=True)
            _ = archive.extract(image, manifest, directory / "evidence")
            root = directory / "evidence/rootfs"
            self.assertEqual((root / "lib/real").resolve(), (root / "usr/lib/real").resolve())
            self.assertFalse((root / "lib").readlink().is_absolute())

    def test_paths_devices_sparse_and_duplicate_entries_are_rejected(self) -> None:
        """No image metadata can create host devices or traverse outside its new evidence."""
        cases = [
            [member("../escape", b"x")],
            [member("/absolute", b"x")],
            [member("a//b", b"x")],
            [member("a", b"x"), member("a", b"y")],
            [member("device", kind=tarfile.CHRTYPE)],
            [member("pipe", kind=tarfile.FIFOTYPE)],
            [member("link", kind=tarfile.SYMTYPE, link="../outside")],
            [
                member("parent", kind=tarfile.SYMTYPE, link="/elsewhere"),
                member("parent/file", b"x"),
            ],
        ]
        for members in cases:
            with (
                self.subTest(names=[info.name for info, _ in members]),
                tempfile.TemporaryDirectory() as temporary,
            ):
                directory = Path(temporary)
                image, manifest = saved_image(directory, [tar(members)])
                with self.assertRaises((archive.ArchiveError, ValueError)):
                    _ = archive.extract(image, manifest, directory / "evidence")
                self.assertFalse((directory / "evidence/report.json").exists())

    def test_gzip_expansion_and_trailing_streams_are_bounded(self) -> None:
        """A valid checksum cannot authorize unbounded or concatenated compressed data."""
        data = tar([member("file", b"fixture")])
        digest = "sha256:" + hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with (
                patch.object(archive, "MAX_LAYER", 8),
                self.assertRaisesRegex(archive.ArchiveError, "expansion"),
            ):
                _ = archive.layer_file(
                    io.BytesIO(gzip.compress(data)), directory / "oversized", digest, "layer.tar"
                )
            with self.assertRaisesRegex(archive.ArchiveError, "compression"):
                _ = archive.layer_file(
                    io.BytesIO(gzip.compress(data) + gzip.compress(b"extra")),
                    directory / "concatenated",
                    digest,
                    "layer.tar",
                )

    def test_layer_and_archive_identities_are_independently_required(self) -> None:
        """Transferred archive integrity alone cannot substitute for image diff IDs."""
        data = tar([member("file", b"fixture")])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with self.assertRaisesRegex(archive.ArchiveError, "diff_id"):
                _ = archive.layer_file(
                    io.BytesIO(data), directory / "wrong", "sha256:" + "0" * 64, "layer.tar"
                )
            with self.assertRaisesRegex(archive.ArchiveError, "blob_digest"):
                _ = archive.layer_file(
                    io.BytesIO(data),
                    directory / "blob",
                    "sha256:" + hashlib.sha256(data).hexdigest(),
                    "blobs/sha256/" + "0" * 64,
                )

    def test_metadata_header_and_total_expansion_limits_precede_success(self) -> None:
        """Physical header bounds apply before tarfile allocates extended metadata."""
        info = tarfile.TarInfo("metadata")
        info.type, info.size = tarfile.XHDTYPE, archive.MAX_METADATA + 1
        with self.assertRaisesRegex(archive.ArchiveError, "body_size"):
            archive.physical_headers(io.BytesIO(info.tobuf()))
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [tar([member("file", b"fixture")])])
            with (
                patch.object(archive, "MAX_EXPANDED", 1),
                self.assertRaisesRegex(archive.ArchiveError, "total_expansion"),
            ):
                _ = archive.extract(image, manifest, directory / "evidence")

    def test_input_symlink_and_existing_evidence_are_refused(self) -> None:
        """Retries cannot overwrite earlier evidence or import unrelated host data."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image, manifest = saved_image(directory, [tar([member("file", b"fixture")])])
            output = directory / "existing"
            output.mkdir()
            with self.assertRaises(FileExistsError):
                _ = archive.extract(image, manifest, output)
            linked = directory / "linked"
            linked.symlink_to(ROOT / "Cargo.toml")
            with self.assertRaisesRegex(archive.ArchiveError, "archive_file"):
                _ = archive.extract(linked, manifest, directory / "never")

    def test_extended_metadata_has_an_independent_cumulative_budget(self) -> None:
        """Many individually small headers cannot exhaust the parser's metadata memory."""
        data = bytearray()
        for index in range(2):
            info = tarfile.TarInfo(f"metadata-{index}")
            info.type, info.size = tarfile.XHDTYPE, 8
            data.extend(info.tobuf())
            data.extend(bytes(archive.BLOCK))
        data.extend(bytes(archive.BLOCK * 2))
        with (
            patch.object(archive, "MAX_METADATA_TOTAL", 15),
            self.assertRaisesRegex(archive.ArchiveError, "metadata_total"),
        ):
            archive.physical_headers(io.BytesIO(data))


if __name__ == "__main__":
    _ = unittest.main()
