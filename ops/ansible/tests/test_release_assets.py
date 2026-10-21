"""Pure public-asset archive and atomic-retention checks; never invoke Docker."""

from __future__ import annotations

import hashlib
import io
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import patch

import release_assets as assets
from release_json import array_value, decode_json, object_value

NOW = 1800000000
NAME = "media-previous123.js"
CONTENT = b"export const previous = true;"


class AssetsTests(unittest.TestCase):
    """Untrusted captures cannot create private files or unbounded retention."""

    temporary: tempfile.TemporaryDirectory[str] | None = None
    directory: Path = Path()
    root: Path = Path()
    owner: int = 0

    @override
    def setUp(self) -> None:
        """Own all fixture bytes in one disposable directory."""
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name).resolve()
        self.root = self.directory / "browser-assets"
        self.owner = self.directory.stat().st_uid

    @override
    def tearDown(self) -> None:
        """Remove only this fixture's owned state."""
        if self.temporary is not None:
            self.temporary.cleanup()

    def capture(self, entries: list[tuple[str, bytes, bytes]]) -> Path:
        """Build the exact uncompressed directory-tar shape returned by docker cp."""
        archive = self.directory / "capture.tar"
        with tarfile.open(archive, "w") as output:
            for name, data, kind in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(data) if kind == tarfile.REGTYPE else 0
                if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    member.linkname = "/private"
                output.addfile(member, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
        return archive

    def publish(self, revision: str, content: bytes = CONTENT, timestamp: int = NOW) -> None:
        """Publish one validated generation with the real transaction."""
        _ = assets.publish(self.root, {revision: {NAME: content}}, timestamp, owner=self.owner)

    def test_directory_capture_is_flat_public_content_only(self) -> None:
        """Allow only actual hashed static bytes, not metadata or extracted tar paths."""
        archive = self.capture(
            [
                ("assets", b"", tarfile.DIRTYPE),
                ("assets/" + NAME, CONTENT, tarfile.REGTYPE),
                ("assets/index-current123.css", b"body{}", tarfile.REGTYPE),
            ]
        )
        self.assertEqual(assets.read_capture(archive)[NAME], CONTENT)

    def test_private_nested_absolute_and_source_map_paths_fail(self) -> None:
        """No private/API/HTML/source-map payload can enter the fallback store."""
        for name in [
            "../private",
            "/assets/" + NAME,
            "assets/../" + NAME,
            "assets/nested/" + NAME,
            "assets/index.html",
            "assets/private-previous123.json",
            "assets/media-previous123.js.map",
            "assets/plain.js",
            "assets/media-previous123.js\\escape",
        ]:
            with self.subTest(name=name), self.assertRaises(assets.AssetError):
                _ = assets.read_capture(self.capture([(name, CONTENT, tarfile.REGTYPE)]))

    def test_symlink_hardlink_and_duplicate_members_fail(self) -> None:
        """Never follow links or adopt ambiguous tar replacement semantics."""
        for kind in [tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE, tarfile.FIFOTYPE]:
            with self.subTest(kind=kind), self.assertRaises(assets.AssetError):
                _ = assets.read_capture(self.capture([("assets/" + NAME, b"", kind)]))
        with self.assertRaises(assets.AssetError):
            _ = assets.read_capture(
                self.capture([("assets/" + NAME, CONTENT, tarfile.REGTYPE)] * 2)
            )

    def test_file_count_and_total_bounds_fail_before_store_write(self) -> None:
        """Fixed parser bounds do not depend on compressed or declared output sizes."""
        archive = self.capture([("assets/" + NAME, CONTENT, tarfile.REGTYPE)])
        with patch.object(assets, "MAX_FILE", 1), self.assertRaises(assets.AssetError):
            _ = assets.read_capture(archive)
        with patch.object(assets, "MAX_TOTAL", 1), self.assertRaises(assets.AssetError):
            _ = assets.read_capture(archive)
        with patch.object(assets, "MAX_FILES", 1), self.assertRaises(assets.AssetError):
            _ = assets.read_capture(
                self.capture(
                    [
                        ("assets/" + NAME, CONTENT, tarfile.REGTYPE),
                        ("assets/index-current123.css", b"body{}", tarfile.REGTYPE),
                    ]
                )
            )
        self.assertFalse(self.root.exists())

    def test_selected_and_candidate_are_preserved_across_rollover_and_rollback(self) -> None:
        """Both browser editions remain available regardless of the selected app image."""
        old, new = "a" * 40, "b" * 40
        _ = assets.publish(
            self.root,
            {old: {NAME: CONTENT}, new: {"media-current123.js": b"current"}},
            NOW,
            owner=self.owner,
        )
        entries = assets.read_store(self.root, self.owner)
        self.assertEqual({entry["revision"] for entry in entries}, {old, new})
        self.assertEqual((self.root / old / NAME).read_bytes(), CONTENT)
        _ = assets.publish(
            self.root,
            {new: {"media-current123.js": b"current"}, old: {NAME: CONTENT}},
            NOW + 1,
            owner=self.owner,
        )
        self.assertEqual((self.root / old / NAME).read_bytes(), CONTENT)
        self.assertEqual((self.root / new / "media-current123.js").read_bytes(), b"current")

    def test_generation_count_and_ttl_prune_only_unprotected_releases(self) -> None:
        """Count and age bounds may evict old editions before seven days."""
        for number in range(6):
            self.publish(f"{number:040x}", timestamp=NOW + number)
        retained = assets.read_store(self.root, self.owner)
        self.assertEqual(len(retained), assets.MAX_GENERATIONS)
        self.assertFalse((self.root / ("0" * 40)).exists())
        self.publish("f" * 40, timestamp=NOW + assets.MAX_AGE + 10)
        self.assertEqual(len(assets.read_store(self.root, self.owner)), 1)

    def test_required_capacity_failure_leaves_prior_index_and_bytes(self) -> None:
        """Deployment must fail before eviction of either selected or rollback assets."""
        self.publish("a" * 40)
        before = (self.root / "index.json").read_bytes()
        with (
            patch.object(assets, "MAX_TOTAL", len(CONTENT) + 1),
            self.assertRaises(assets.AssetError),
        ):
            _ = assets.publish(
                self.root,
                {"a" * 40: {NAME: CONTENT}, "b" * 40: {"media-current123.js": CONTENT}},
                NOW + 1,
                owner=self.owner,
            )
        self.assertEqual((self.root / "index.json").read_bytes(), before)
        self.assertFalse((self.root / ("b" * 40)).exists())

    def test_immutable_url_and_revision_collisions_fail(self) -> None:
        """Opaque bundler fingerprints never excuse serving different bytes at one URL."""
        self.publish("a" * 40)
        before = (self.root / "index.json").read_bytes()
        for revision in ["a" * 40, "b" * 40]:
            with self.subTest(revision=revision), self.assertRaises(assets.AssetError):
                self.publish(revision, b"changed")
        self.assertEqual((self.root / "index.json").read_bytes(), before)

    def test_ownership_symlinks_foreign_state_and_hash_changes_fail(self) -> None:
        """Store validation cannot adopt an operator's unrelated files or corrupted bytes."""
        self.publish("a" * 40)
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner + 1)
        target = self.root / ("a" * 40) / NAME
        _ = target.write_bytes(b"changed")
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner)
        _ = target.write_bytes(CONTENT)
        target.chmod(0o666)
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner)
        target.chmod(0o644)
        target.unlink()
        target.symlink_to(self.directory / "private")
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner)

    def test_failed_index_publication_cleans_new_generation_and_preserves_old(self) -> None:
        """A failed atomic replacement cannot erase the last known good publication."""
        self.publish("a" * 40)
        before = (self.root / "index.json").read_bytes()
        with (
            patch.object(Path, "replace", side_effect=OSError("fixture publication failed")),
            self.assertRaises(OSError),
        ):
            _ = assets.publish(
                self.root,
                {"b" * 40: {"media-current123.js": b"current"}},
                NOW + 1,
                owner=self.owner,
            )
        self.assertEqual((self.root / "index.json").read_bytes(), before)
        self.assertFalse((self.root / ("b" * 40)).exists())
        _ = assets.read_store(self.root, self.owner)

    def test_failed_first_initialize_removes_only_its_new_empty_public_directory(self) -> None:
        """An ordinary initial publication failure can be retried without adopting residue."""
        with (
            patch.object(Path, "replace", side_effect=OSError("fixture first publication failed")),
            self.assertRaises(OSError),
        ):
            _ = assets.initialize(self.root, owner=self.owner)
        self.assertEqual(list(self.root.iterdir()), [])
        self.assertTrue(assets.initialize(self.root, owner=self.owner))
        self.assertFalse(assets.initialize(self.root, owner=self.owner))
        self.assertEqual(assets.read_store(self.root, self.owner), [])

    def test_index_records_exact_sha_size_and_public_modes(self) -> None:
        """The proxy receives only auditable immutable public bytes."""
        self.publish("a" * 40)
        value = object_value(decode_json((self.root / "index.json").read_bytes()))
        generation = object_value(array_value(value["generations"])[0])
        self.assertEqual(
            object_value(generation["assets"])[NAME],
            {"sha256": hashlib.sha256(CONTENT).hexdigest(), "size": len(CONTENT)},
        )
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o755)
        self.assertEqual((self.root / "index.json").stat().st_mode & 0o777, 0o644)

    def test_aggregate_metadata_budget_rejects_before_any_asset_payload_read(self) -> None:
        """A corrupt owned index cannot make validation hash gigabytes before refusal."""
        _ = assets.publish(
            self.root,
            {"a" * 40: {NAME: CONTENT}, "b" * 40: {"media-current123.js": CONTENT}},
            NOW,
            owner=self.owner,
        )
        original = Path.read_bytes
        reads: list[Path] = []

        def guarded_read(path: Path) -> bytes:
            reads.append(path)
            if path == self.root / "index.json":
                return original(path)
            message = "Asset payload opened before aggregate admission"
            raise AssertionError(message)

        with (
            patch.object(assets, "MAX_TOTAL", len(CONTENT) * 2 - 1),
            patch.object(Path, "read_bytes", guarded_read),
            self.assertRaisesRegex(assets.AssetError, "asset_index_total"),
        ):
            _ = assets.read_store(self.root, self.owner)
        self.assertEqual(reads, [self.root / "index.json"])

    def test_proxy_union_is_relative_verified_and_selected_version_is_separate(self) -> None:
        """Staging cannot announce a candidate; selecting and rolling back can."""
        self.publish("a" * 40)
        _ = assets.select_version(self.root, "a" * 40, owner=self.owner)
        version = self.root / "public" / "app-version.json"
        old = version.read_bytes()
        _ = assets.publish(
            self.root, {"b" * 40: {"media-current123.js": b"current"}}, NOW + 1, owner=self.owner
        )
        self.assertEqual(version.read_bytes(), old)
        pointer = self.root / "public" / "assets"
        self.assertTrue(str(pointer.readlink()).startswith("../union-"))
        self.assertEqual((pointer / NAME).read_bytes(), CONTENT)
        self.assertEqual((pointer / "media-current123.js").read_bytes(), b"current")
        _ = assets.select_version(self.root, "b" * 40, owner=self.owner)
        self.assertIn(b"b" * 40, version.read_bytes())
        _ = assets.select_version(self.root, "a" * 40, owner=self.owner)
        self.assertEqual(version.read_bytes(), old)
        _ = assets.read_store(self.root, self.owner)

    def test_prune_expires_proxy_bytes_without_another_release(self) -> None:
        """The timer removes expired flat files, while preserving selected metadata."""
        self.publish("a" * 40)
        _ = assets.select_version(self.root, "a" * 40, owner=self.owner)
        assets.prune(self.root, NOW + assets.MAX_AGE, owner=self.owner)
        self.assertEqual(assets.read_store(self.root, self.owner), [])
        self.assertFalse((self.root / "public" / "assets" / NAME).exists())
        self.assertTrue((self.root / "public" / "app-version.json").is_file())

    def test_foreign_union_pointer_and_extra_public_files_are_not_adopted(self) -> None:
        """A relative generated pointer cannot be replaced by an arbitrary target."""
        self.publish("a" * 40)
        pointer = self.root / "public" / "assets"
        target = pointer.readlink()
        pointer.unlink()
        pointer.symlink_to(self.directory)
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner)
        pointer.unlink()
        pointer.symlink_to(target)
        _ = (self.root / "public" / "private.json").write_bytes(b"private")
        with self.assertRaises(assets.AssetError):
            _ = assets.read_store(self.root, self.owner)


if __name__ == "__main__":
    _ = unittest.main()
