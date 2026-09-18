"""Offline recursive JSON and archive-reader boundary regression checks."""

from __future__ import annotations

import hashlib
import io
import json
import tarfile
import unittest
from dataclasses import dataclass
from typing import TYPE_CHECKING
from unittest.mock import patch

import test_support

# Bootstrap flat checkout imports before loading release helpers.
# isort: split
import release_artifact
from release_json import (
    DuplicateJsonError,
    JsonObject,
    JsonValue,
    array_value,
    boolean_value,
    decode_json,
    integer_value,
    json_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Callable

REVISION = "a" * 40
TAG = f"simplestchat-release/production:{REVISION}"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
OCI_INDEX = "application/vnd.oci.image.index.v1+json"


def memory_archive(files: dict[str, bytes]) -> io.BytesIO:
    """Create inert tar members entirely in memory without extracting anything."""
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w") as archive:
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    _ = stream.seek(0)
    return stream


@dataclass
class OciFixture:
    """Equivalent Docker and OCI metadata with inert, local-only layer bytes."""

    files: dict[str, bytes]
    image: JsonObject
    config: JsonObject
    manifest: release_artifact.Manifest

    def verify(self) -> None:
        """Check both metadata views through the actual indexed archive reader."""
        with (
            memory_archive(self.files) as stream,
            tarfile.open(fileobj=stream, mode="r:") as archive,
        ):
            reader = release_artifact.ArchiveReader.indexed(archive)
            reader.validate_oci(self.image, self.config, self.manifest)


def oci_fixture(index_depth: int = 0) -> OciFixture:
    """Build a single-image OCI chain with a selectable number of nested indexes."""
    files: dict[str, bytes] = {}

    def descriptor(data: bytes) -> JsonObject:
        digest = hashlib.sha256(data).hexdigest()
        files["blobs/sha256/" + digest] = data
        return {"digest": "sha256:" + digest, "size": len(data)}

    def metadata(value: JsonObject) -> JsonObject:
        return descriptor(json.dumps(value).encode())

    config: JsonObject = {"architecture": "amd64", "os": "linux"}
    config_descriptor = metadata(config)
    layer_descriptors = [descriptor(b"first inert layer"), descriptor(b"second inert layer")]
    layers: list[JsonValue] = list(layer_descriptors)
    current = metadata({"schemaVersion": 2, "config": config_descriptor, "layers": layers})
    current["mediaType"] = OCI_MANIFEST
    for _ in range(index_depth):
        current = metadata({"schemaVersion": 2, "manifests": [current]})
        current["mediaType"] = OCI_INDEX
    current["annotations"] = {"io.containerd.image.name": TAG}
    files["index.json"] = json.dumps({"schemaVersion": 2, "manifests": [current]}).encode()
    files["oci-layout"] = b'{"imageLayoutVersion":"1.0.0"}'
    image: JsonObject = {
        "Config": "blobs/sha256/" + string_value(config_descriptor["digest"]).split(":")[1],
        "Layers": [
            "blobs/sha256/" + string_value(item["digest"]).split(":")[1]
            for item in layer_descriptors
        ],
    }
    manifest: release_artifact.Manifest = {
        "schemaVersion": 1,
        "revision": REVISION,
        "platform": "linux/amd64",
        "archiveSha256": "b" * 64,
        "imageTag": TAG,
        "migrations": {"1": "c" * 96},
        "createdAt": "2026-09-13T12:00:00Z",
    }
    return OciFixture(files, image, config, manifest)


class JsonBoundaryTests(unittest.TestCase):
    """Require exact recursive types and independent copies at JSON boundaries."""

    def test_bootstrap_points_to_the_actual_checkout(self) -> None:
        """Keep this independently runnable suite bound to the checkout helpers."""
        self.assertTrue((test_support.ROOT / "ops/ansible/files/release_json.py").is_file())

    def test_decode_preserves_nested_values_from_text_and_utf8_bytes(self) -> None:
        """Decode all JSON scalar and container types without string coercion."""
        source = '{"nested":[null,true,false,0,-12,1.25,"snowman ☃",{"key":[]}]}'
        expected: JsonObject = {
            "nested": [None, True, False, 0, -12, 1.25, "snowman ☃", {"key": []}],
        }
        self.assertEqual(decode_json(source), expected)
        self.assertEqual(decode_json(source.encode()), expected)

    def test_duplicate_keys_are_rejected_at_every_object_depth(self) -> None:
        """Reject repeated and escape-equivalent field names, even in arrays."""
        for source in (
            '{"key":1,"key":2}',
            '{"outer":{"key":1,"key":2}}',
            '[{"key":1,"key":2}]',
            '{"key":1,"\\u006bey":2}',
        ):
            with self.subTest(source=source), self.assertRaises(DuplicateJsonError) as raised:
                _ = decode_json(source)
            self.assertEqual(raised.exception.key, "key")
            self.assertEqual(str(raised.exception), "Duplicate JSON key: key")

    def test_same_field_name_in_separate_objects_is_unambiguous(self) -> None:
        """Allow independent objects to use the same schema field names."""
        self.assertEqual(decode_json('[{"key":1},{"key":2}]'), [{"key": 1}, {"key": 2}])

    def test_non_finite_json_constants_and_exponent_overflow_are_rejected(self) -> None:
        """Reject non-finite values at the top level and inside nested containers."""
        for token in ("NaN", "Infinity", "-Infinity", "1e999", "-1e999"):
            for source in (token, "[" + token + "]", '{"nested":[' + token + "]}"):
                with self.subTest(source=source), self.assertRaises(ValueError):
                    _ = decode_json(source)

    def test_malformed_text_and_invalid_encoding_are_not_coerced(self) -> None:
        """Propagate malformed JSON and encoding failures instead of guessing values."""
        for source in ("", "null trailing", '{"key":}', "[1,]", "{unquoted:1}"):
            with self.subTest(source=source), self.assertRaises(json.JSONDecodeError):
                _ = decode_json(source)
        with self.assertRaises(UnicodeDecodeError):
            _ = decode_json(b"\xff")

    def test_json_value_rejects_non_json_objects_and_non_string_keys(self) -> None:
        """Check every supplied key and value, including nested decoded YAML shapes."""
        cases: list[object] = [
            b"bytes",
            (1, 2),
            {1, 2},
            object(),
            {1: "number"},
            {True: "boolean"},
            {None: "null"},
            {("tuple",): "tuple"},
            {"outer": [{"key": object()}]},
            {"outer": [{7: "nested numeric key"}]},
            float("nan"),
            float("inf"),
            {"outer": [float("-inf")]},
        ]
        for index, value in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(ValueError):
                _ = json_value(value)

    def test_json_value_copies_every_container_in_both_directions(self) -> None:
        """Ensure later input mutation cannot rewrite previously validated JSON."""
        original_leaf: JsonObject = {"name": "original"}
        original_items: list[JsonValue] = [original_leaf]
        original: JsonObject = {"items": original_items}
        copied = object_value(json_value(original))
        copied_items = array_value(copied["items"])
        copied_leaf = object_value(copied_items[0])
        self.assertIsNot(copied, original)
        self.assertIsNot(copied_items, original_items)
        self.assertIsNot(copied_leaf, original_leaf)
        original_leaf["name"] = "changed input"
        original_items.append(None)
        original["new"] = True
        self.assertEqual(copied, {"items": [{"name": "original"}]})
        copied_leaf["name"] = "changed output"
        self.assertEqual(original_leaf["name"], "changed input")

    def test_object_and_array_accessors_preserve_the_validated_container(self) -> None:
        """Narrow validated containers without silently making another copy."""
        object_fixture: JsonObject = {"key": []}
        array_fixture: list[JsonValue] = [object_fixture]
        self.assertIs(object_value(object_fixture), object_fixture)
        self.assertIs(array_value(array_fixture), array_fixture)

    def test_accessors_reject_other_json_types_without_coercion(self) -> None:
        """Keep object, array, text, integer and boolean schema fields distinct."""
        cases: list[tuple[Callable[[JsonValue], object], list[JsonValue]]] = [
            (object_value, [None, [], "{}", 0, True]),
            (array_value, [None, {}, "[]", 0, False]),
            (string_value, [None, {}, [], 0, 1.5, True]),
            (integer_value, [None, {}, [], "1", 1.0, True, False]),
            (boolean_value, [None, {}, [], "true", "false", 0, 1, 0.0, 1.0]),
        ]
        for accessor, values in cases:
            for value in values:
                with (
                    self.subTest(accessor=accessor.__name__, value=value),
                    self.assertRaises(ValueError),
                ):
                    _ = accessor(value)

    def test_scalar_accessors_accept_exact_values_including_zero_and_false(self) -> None:
        """Accept legitimate falsey values without confusing integer and boolean types."""
        self.assertEqual(string_value(""), "")
        self.assertEqual(integer_value(0), 0)
        self.assertEqual(integer_value(-42), -42)
        self.assertIs(boolean_value(value=False), expr2=False)
        self.assertIs(boolean_value(value=True), expr2=True)


class ArtifactReaderBoundaryTests(unittest.TestCase):
    """Cover refactored archive-reader limits and OCI metadata equivalence."""

    def test_metadata_decode_preserves_duplicate_errors_and_rejects_nonfinite(self) -> None:
        """Keep shared JSON rejection behind the artifact-specific error interface."""
        for data, error in (
            (b'{"key":1,"key":2}', "Duplicate JSON key: key"),
            (b'{"key":NaN}', "Invalid artifact JSON"),
            (b"\xff", "Invalid artifact JSON"),
        ):
            with (
                self.subTest(data=data),
                memory_archive({"metadata.json": data}) as stream,
                tarfile.open(fileobj=stream, mode="r:") as archive,
            ):
                reader = release_artifact.ArchiveReader.indexed(archive)
                with self.assertRaisesRegex(release_artifact.ArtifactError, error):
                    _ = reader.read_json("metadata.json")

    def test_metadata_budget_accepts_exact_limit_and_refuses_larger_members(self) -> None:
        """Enforce the metadata byte budget before reading oversized member contents."""
        with (
            memory_archive({"valid.json": b"null", "large.json": b"null "}) as stream,
            tarfile.open(fileobj=stream, mode="r:") as archive,
            patch.object(release_artifact, "MAX_JSON_BYTES", 4),
        ):
            reader = release_artifact.ArchiveReader.indexed(archive)
            self.assertIsNone(reader.read_json("valid.json"))
            with self.assertRaisesRegex(release_artifact.ArtifactError, "oversized"):
                _ = reader.read_json("large.json")

    def test_archive_member_count_remains_bounded(self) -> None:
        """Apply the member limit while indexing, before metadata processing."""
        with (
            memory_archive({"first": b"1", "second": b"2"}) as stream,
            tarfile.open(fileobj=stream, mode="r:") as archive,
            patch.object(release_artifact, "MAX_ARCHIVE_MEMBERS", 1),
            self.assertRaisesRegex(release_artifact.ArtifactError, "Too many"),
        ):
            _ = release_artifact.ArchiveReader.indexed(archive)

    def test_descriptor_requires_exact_size_local_location_and_content_digest(self) -> None:
        """Reject ambiguous sizes, remote URLs and mismatched OCI metadata bytes."""
        data = b"{}"
        digest = "a" * 64
        name = "blobs/sha256/" + digest
        cases: list[JsonObject] = [
            {"digest": "sha256:" + digest, "size": True},
            {"digest": "sha256:" + digest, "size": 2.0},
            {"digest": "sha256:" + digest, "size": 3},
            {"digest": "sha256:" + digest, "size": 2, "urls": ["https://example.invalid"]},
            {"digest": "sha256:" + digest, "size": 2},
        ]
        with (
            memory_archive({name: data}) as stream,
            tarfile.open(fileobj=stream, mode="r:") as archive,
        ):
            reader = release_artifact.ArchiveReader.indexed(archive)
            for index, descriptor in enumerate(cases):
                with self.subTest(index=index), self.assertRaises(release_artifact.ArtifactError):
                    _ = reader.read_descriptor(descriptor)

    def test_oci_index_depth_accepts_last_supported_level_and_rejects_next(self) -> None:
        """Preserve the eight-descriptor traversal limit after reader extraction."""
        oci_fixture(release_artifact.MAX_INDEX_DEPTH - 1).verify()
        with self.assertRaisesRegex(release_artifact.ArtifactError, "nesting is too deep"):
            oci_fixture(release_artifact.MAX_INDEX_DEPTH).verify()

    def test_oci_config_and_ordered_layers_must_match_the_docker_view(self) -> None:
        """Bind both layouts to the same configuration and ordered local layers."""
        fixture = oci_fixture()
        fixture.verify()
        fixture.config["architecture"] = "arm64"
        with self.assertRaisesRegex(release_artifact.ArtifactError, "configurations disagree"):
            fixture.verify()
        fixture = oci_fixture()
        array_value(fixture.image["Layers"]).reverse()
        with self.assertRaisesRegex(release_artifact.ArtifactError, "layers disagree"):
            fixture.verify()


if __name__ == "__main__":
    _ = unittest.main()
