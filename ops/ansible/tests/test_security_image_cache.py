"""Exercise database byte reuse without substituting cached security verdicts."""

from __future__ import annotations

import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_image as image
import security_image_cache as cache
from release_json import decode_json, object_value
from security_tools import ToolError
from test_security_image_parallel import sandbox
from test_security_image_runner import Engine

if TYPE_CHECKING:
    from urllib.request import Request

    from release_json import JsonObject

_ = ROOT


def descriptor(checksum: str = "a" * 64) -> JsonObject:
    """Represent the fixed v6 upstream distribution format without network calls."""
    return {"status": "active", "schemaVersion": "v6.1.10", "checksum": "sha256:" + checksum}


def status(checksum: str = "a" * 64) -> JsonObject:
    """Describe the archive actually selected by the ordinary online updater."""
    return {
        "from": "https://grype.anchore.io/databases/v6/vulnerability-db_v6.1.10_example.tar.zst"
        + "?checksum=sha256%3A"
        + checksum
    }


class DescriptorResponse(io.BytesIO):
    """Bounded HTTPS response fixture for the descriptor cache hint."""

    url: str

    def __init__(self, body: bytes, url: str = cache.LATEST) -> None:
        """Expose only the response body and final URL used by the bounded reader."""
        super().__init__(body)
        self.url = url


def database(root: Path) -> Path:
    """Construct inert data only; no executable or actual database is required."""
    directory = root / "database/cache/6"
    directory.mkdir(parents=True)
    _ = (directory / "vulnerability.db").write_bytes(b"inert database bytes")
    _ = (directory / "import.json").write_text('{"checksum":"inert"}\n')
    return root / "database"


class DatabaseCacheTests(unittest.TestCase):
    """Malformed or changed cached bytes fail; a miss still allows fresh downloads."""

    def test_roundtrip_and_updated_database_replace_prior_bytes(self) -> None:
        """A repeated publication stores the current validated database, not the first one."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = database(root)
            saved = root / "saved"
            for index in range(2):
                payload = f"database generation {index}".encode()
                _ = (source / "cache/6/vulnerability.db").write_bytes(payload)
                cache.save(saved, source)
                target = root / f"restored-{index}"
                target.mkdir()
                self.assertTrue(cache.restore(saved, target, os.getuid(), os.getgid()))
                self.assertEqual((target / "cache/6/vulnerability.db").read_bytes(), payload)
                receipt = object_value(decode_json((saved / "receipt.json").read_bytes()))
                self.assertEqual(set(receipt), {"schemaVersion", "scannerPin", "files"})
                self.assertEqual(set(object_value(receipt["files"])), set(cache.FILES))

    def test_missing_cache_is_a_miss_but_corruption_and_links_fail(self) -> None:
        """No malformed restored cache silently replaces a failed validation with success."""
        for mutation in ("digest", "link", "directory-link", "pin", "oversize"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = database(root)
                saved, output = root / "saved", root / "output"
                output.mkdir()
                self.assertFalse(cache.restore(saved, output, os.getuid(), os.getgid()))
                cache.save(saved, source)
                target = saved / "vulnerability.db"
                if mutation == "digest":
                    _ = target.write_bytes(b"corrupt")
                elif mutation == "link":
                    target.unlink()
                    target.symlink_to(source / "cache/6/vulnerability.db")
                elif mutation == "directory-link":
                    link = root / "link"
                    link.symlink_to(saved, target_is_directory=True)
                    saved = link
                elif mutation == "pin":
                    receipt = object_value(decode_json((saved / "receipt.json").read_bytes()))
                    receipt["scannerPin"] = "wrong"
                    _ = (saved / "receipt.json").write_text(json.dumps(receipt))
                elif mutation == "oversize":
                    with target.open("wb") as stream:
                        _ = stream.truncate(cache.FILES["vulnerability.db"] + 1)
                with self.assertRaises((ToolError, OSError)):
                    _ = cache.restore(saved, output, os.getuid(), os.getgid())

    def test_cache_hit_still_executes_networked_update_in_owned_sandbox(self) -> None:
        """Restored data seeds the updater, which retains its network and artifact boundaries."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = database(root)
            saved = root / "saved"
            cache.save(saved, source)
            selected = sandbox(root)
            selected.database_cache = saved
            engine = Engine()
            with patch.object(selected, "command", side_effect=engine.command):
                status, _ = selected.run(
                    "grype", ["db", "update"], destination=root / "update", online=True
                )
            self.assertEqual(status, 0)
            self.assertEqual(
                (root / "update/cache/6/vulnerability.db").read_bytes(), b"inert database bytes"
            )
            command = engine.calls[0]
            self.assertEqual(command[-2:], ["db", "update"])
            self.assertIn("GRYPE_DB_AUTO_UPDATE=true", command)
            self.assertIn("GRYPE_DB_VALIDATE_BY_HASH_ON_START=true", command)
            self.assertIn("GRYPE_DB_VALIDATE_AGE=true", command)
            self.assertFalse(any("dst=/input" in argument for argument in command))

    def test_database_status_failure_cannot_publish_cache(self) -> None:
        """A successful download alone cannot make an invalid database reusable."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _ = database(root)
            selected = sandbox(root)
            selected.database_cache = root / "saved"
            with (
                patch.object(selected, "run", side_effect=[(0, ""), (1, "")]) as run,
                patch.object(cache, "save") as save,
                self.assertRaisesRegex(ToolError, "image_database_status_failed"),
            ):
                _ = image.prepare_database(selected)
            self.assertEqual(run.call_count, 2)
            save.assert_not_called()

    def test_completed_database_still_requires_the_four_gib_bound(self) -> None:
        """Extra update workspace cannot admit a larger final database or publish it."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = database(root)
            with (source / "cache/6/vulnerability.db").open("wb") as stream:
                _ = stream.truncate(image.MAX_DB + 1)
            selected = sandbox(root)
            selected.database_cache = root / "saved"
            with (
                patch.object(selected, "run", return_value=(0, "")) as run,
                patch.object(cache, "save") as save,
                self.assertRaisesRegex(ToolError, "image_database_output_limit"),
            ):
                _ = image.prepare_database(selected)
            self.assertEqual(run.call_count, 1)
            save.assert_not_called()

    def test_main_and_untrusted_download_cache_keys_never_overlap(self) -> None:
        """Executable scanner pins and the trust boundary remain part of every cache key."""
        with patch.dict(os.environ, GITHUB_REF="refs/heads/main", GITHUB_EVENT_NAME="push"):
            trusted = cache.descriptor_key(descriptor())
        with patch.dict(os.environ, GITHUB_REF="refs/heads/main", GITHUB_EVENT_NAME="pull_request"):
            untrusted = cache.descriptor_key(descriptor())
        self.assertNotEqual(trusted, untrusted)
        self.assertRegex(trusted, r"^grype-db-v2-main-[a-f0-9]{64}-[a-f0-9]{64}$")
        with patch.object(cache, "pin", return_value="b" * 64):
            self.assertNotEqual(untrusted, cache.descriptor_key(descriptor()))

    def test_same_day_rotation_and_post_update_identity_select_new_generations(self) -> None:
        """An upstream rotation uses immutable content keys, including during preparation."""
        first = cache.descriptor_key(descriptor())
        second = cache.descriptor_key(descriptor("b" * 64))
        self.assertNotEqual(first, second)
        self.assertEqual(first, cache.key_from_status(status()))
        self.assertEqual(second, cache.key_from_status(status("b" * 64)))
        self.assertTrue(second.startswith(cache.prefix()))

    def test_descriptor_and_updater_sources_reject_ambiguous_or_unbounded_identities(self) -> None:
        """Only the reviewed schema and one SHA-256 checksum can select a generation."""
        for field, value in (
            ("status", "inactive"),
            ("schemaVersion", "v7.0.0"),
            ("schemaVersion", 6),
            ("checksum", None),
            ("checksum", "sha256:" + "g" * 64),
            ("checksum", "sha256:" + "a" * 64 + "\n"),
            ("checksum", "../../elsewhere"),
        ):
            changed = descriptor()
            changed[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ToolError):
                _ = cache.descriptor_key(changed)
        source = str(status()["from"])
        for changed_source in (
            source.replace("https:", "http:"),
            source.replace("grype.anchore.io", "example.com"),
            source.replace("/v6/", "/v7/"),
            source.replace(".tar.zst", ".zip"),
            source + "#fragment",
            source + "&checksum=",
            source + "&unexpected=value",
            source.split("?", maxsplit=1)[0],
            source.replace("sha256%3A", "md5%3A"),
        ):
            with self.subTest(source=changed_source), self.assertRaises((ToolError, ValueError)):
                _ = cache.key_from_status({"from": changed_source})

    def test_public_descriptor_is_fixed_origin_bounded_and_only_a_cache_hint(self) -> None:
        """No redirect, oversized response, or malformed document becomes a cache key."""
        payload = json.dumps(descriptor()).encode()
        with patch.object(cache, "urlopen", return_value=DescriptorResponse(payload)) as request:
            self.assertEqual(cache.key(), cache.descriptor_key(descriptor()))
            actual_request = cast("Request", request.call_args.args[0])
            self.assertEqual(actual_request.full_url, cache.LATEST)
            self.assertEqual(request.call_args.kwargs, {"timeout": 10})
        for body, url in (
            (payload, "https://example.com/latest.json"),
            (b" " * (cache.MAX_DESCRIPTOR + 1), cache.LATEST),
            (b"", cache.LATEST),
            (b"[]", cache.LATEST),
            (b"not-json", cache.LATEST),
        ):
            with (
                self.subTest(url=url, size=len(body)),
                patch.object(cache, "urlopen", return_value=DescriptorResponse(body, url)),
                self.assertRaises((ToolError, ValueError)),
            ):
                _ = cache.key()
