"""Real inert files verify bounded printable-string coverage and original identity."""

from __future__ import annotations

import hashlib
import os
import tempfile
import unittest
from pathlib import Path

from test_support import ROOT

# isort: split
import security_secret_projection as projection

_ = ROOT
ASCII_FIRST, ASCII_LAST = 32, 126


class SecretProjectionTests(unittest.TestCase):
    """Binary headers cannot prevent contiguous ASCII material from reaching the scanner."""

    def test_binary_projection_preserves_strings_and_binds_original_bytes(self) -> None:
        """Non-text delimiters change; original hashes and stable printable runs remain exact."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / "binary", root / "text"
            content = (
                b"\x7fELF\x02\x01\0\xff" + b"TOKEN=inert-string-only\t\r\n" + bytes(range(256))
            )
            _ = source.write_bytes(content)
            result = projection.project(source, target, max_bytes=len(content))
            projected = target.read_bytes()
            self.assertIn(b"TOKEN=inert-string-only\t\r\n", projected)
            self.assertTrue(
                all(
                    value in (9, 10, 13) or ASCII_FIRST <= value <= ASCII_LAST
                    for value in projected
                )
            )
            self.assertEqual(result.source_sha256, hashlib.sha256(content).hexdigest())
            self.assertEqual(result.projection_sha256, hashlib.sha256(projected).hexdigest())
            self.assertNotEqual(result.source_sha256, result.projection_sha256)
            self.assertEqual(
                (result.source_bytes, result.projection_bytes),
                (len(content), len(content) + len(projection.PREFIX)),
            )
            self.assertEqual(target.stat().st_mode & 0o777, 0o600)
            self.assertEqual(source.read_bytes(), content)

    def test_ascii_text_and_lines_are_unchanged_across_chunk_boundaries(self) -> None:
        """A long encoded printable run is never split at the streaming chunk size."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            content = b"prefix\n" + b"aZ09+/=" * 20000 + b"\r\nnext\tline\n"
            _ = (root / "source").write_bytes(content)
            result = projection.project(root / "source", root / "text", max_bytes=len(content))
            self.assertEqual((root / "text").read_bytes(), projection.PREFIX + content)
            self.assertEqual(
                result.projection_sha256, hashlib.sha256(projection.PREFIX + content).hexdigest()
            )

    def test_non_ascii_encodings_have_an_explicit_limit(self) -> None:
        """No UTF-16 or compressed-format coverage is inferred from printable projection."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _ = (root / "source").write_bytes("benign".encode("utf-16-le"))
            _ = projection.project(root / "source", root / "text", max_bytes=12)
            self.assertEqual(
                (root / "text").read_bytes(), projection.PREFIX + b"b\ne\nn\ni\ng\nn\n"
            )
            self.assertIn("UTF-16", " ".join(projection.LIMITATIONS))

    def test_prefix_covers_pinned_binary_magic_offsets_without_splitting_content(self) -> None:
        """PDF, TAR, DICOM and ISO sniff offsets see only the fixed printable prefix."""
        self.assertGreater(len(projection.PREFIX), 32773)
        for offset, size in ((0, 4), (128, 4), (257, 5), (32769, 5)):
            with self.subTest(offset=offset):
                self.assertNotIn(
                    projection.PREFIX[offset : offset + size],
                    (b"%PDF", b"DICM", b"ustar", b"CD001"),
                )

    def test_size_bound_is_exact_and_refuses_before_output(self) -> None:
        """No decimal-MB scanner convention can silently bypass the caller's byte budget."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _ = (root / "source").write_bytes(b"12345")
            with self.assertRaisesRegex(projection.ProjectionError, "source_size"):
                _ = projection.project(root / "source", root / "text", max_bytes=4)
            self.assertFalse((root / "text").exists())
            _ = projection.project(root / "source", root / "text", max_bytes=5)
            self.assertEqual((root / "text").read_bytes(), projection.PREFIX + b"12345")

    def test_empty_input_is_a_complete_zero_byte_projection(self) -> None:
        """Empty regular files contain no printable credentials and still retain a hash."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _ = (root / "source").write_bytes(b"")
            result = projection.project(root / "source", root / "text", max_bytes=0)
            self.assertEqual(result.source_bytes, 0)
            self.assertEqual(result.source_sha256, hashlib.sha256(b"").hexdigest())

    def test_links_nonregular_sources_and_existing_outputs_are_refused(self) -> None:
        """Opening untrusted special input cannot block or overwrite unrelated evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _ = (root / "source").write_bytes(b"inert")
            (root / "link").symlink_to(root / "source")
            os.mkfifo(root / "fifo")
            for source in (root / "link", root / "fifo"):
                with self.subTest(source=source.name), self.assertRaises((OSError, ValueError)):
                    _ = projection.project(source, root / "text", max_bytes=100)
                self.assertFalse((root / "text").exists())
            _ = (root / "text").write_bytes(b"existing")
            with self.assertRaises(FileExistsError):
                _ = projection.project(root / "source", root / "text", max_bytes=100)
            self.assertEqual((root / "text").read_bytes(), b"existing")
