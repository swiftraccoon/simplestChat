"""Validate the portable, registry-free simplestChat production artifact.

Both the off-host builder and host importer use these checks. Validation reads
the Docker archive without extracting or loading it. Its digest and single tag
identify the transfer; Docker's target-local image ID is recorded by the importer.
"""

# Error text is the stable artifact-validation interface, not a logged exception.
# ruff: noqa: TRY003, EM101

import hashlib
import re
import tarfile
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import Literal, TypedDict, TypeGuard

from release_json import DuplicateJsonError, JsonObject, JsonValue, decode_json

MAX_JSON_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 8 * 1024**3
MAX_MIGRATIONS = 1000
MAX_ARCHIVE_MEMBERS = 100_000
MAX_INDEX_DEPTH = 8
ASCII_SPACE = 32
OCI_SCHEMA_VERSION = 2
MANIFEST_KEYS = {
    "schemaVersion",
    "revision",
    "platform",
    "archiveSha256",
    "imageTag",
    "migrations",
    "createdAt",
}


class Manifest(TypedDict):
    """Validated release manifest; preserve the portable JSON field spellings."""

    schemaVersion: Literal[1]
    revision: str
    platform: Literal["linux/amd64"]
    archiveSha256: str
    imageTag: str
    migrations: dict[str, str]
    createdAt: str


class ArtifactError(ValueError):
    """The artifact is incomplete or outside the supported release contract."""


def _json(data: bytes) -> JsonValue:
    try:
        return decode_json(data)
    except DuplicateJsonError as error:
        raise ArtifactError(str(error)) from error
    except (UnicodeError, ValueError) as error:
        raise ArtifactError("Invalid artifact JSON") from error


def _matches(value: JsonValue, pattern: str) -> TypeGuard[str]:
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def _migrations(value: JsonValue) -> dict[str, str]:
    if not isinstance(value, dict) or not 0 < len(value) <= MAX_MIGRATIONS:
        raise ArtifactError("Release migrations must be a nonempty bounded map")
    result: dict[str, str] = {}
    for version, checksum in value.items():
        if not _matches(version, r"[1-9][0-9]{0,18}") or int(version) > 2**63 - 1:
            raise ArtifactError("Migration versions must be canonical positive decimal integers")
        if not _matches(checksum, r"[a-f0-9]{96}"):
            raise ArtifactError("Invalid migration SHA384")
        result[version] = checksum
    return result


def validate_manifest(path: Path) -> Manifest:
    """Read a bounded manifest and reject unknown or ambiguous schema fields."""
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= MAX_JSON_BYTES:
        raise ArtifactError("Release manifest must be a bounded regular file")
    manifest = _json(path.read_bytes())
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise ArtifactError("Unsupported release manifest fields")
    if type(manifest["schemaVersion"]) is not int or manifest["schemaVersion"] != 1:
        raise ArtifactError("Unsupported release schema")
    revision, checksum = manifest["revision"], manifest["archiveSha256"]
    if not _matches(revision, r"[a-f0-9]{40}"):
        raise ArtifactError("Release revision must be a full Git commit ID")
    if manifest["platform"] != "linux/amd64":
        raise ArtifactError("Only linux/amd64 releases are supported")
    if not _matches(checksum, r"[a-f0-9]{64}"):
        raise ArtifactError("Invalid archive SHA256")
    tag = f"simplestchat-release/production:{revision}"
    if manifest["imageTag"] != tag:
        raise ArtifactError("Release tag does not match its revision")
    migrations = _migrations(manifest["migrations"])
    timestamp = manifest["createdAt"]
    if not _matches(timestamp, r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z"):
        raise ArtifactError("Release timestamp must be UTC with second precision")
    try:
        _ = datetime.strptime(timestamp, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError as error:
        raise ArtifactError("Invalid release timestamp") from error
    return Manifest(
        schemaVersion=1,
        revision=revision,
        platform="linux/amd64",
        archiveSha256=checksum,
        imageTag=tag,
        migrations=migrations,
        createdAt=timestamp,
    )


def sha256_file(path: Path) -> str:
    """Hash large archives in bounded chunks."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _member_name(name: JsonValue) -> str:
    if (
        not isinstance(name, str)
        or not name
        or "\\" in name
        or any(ord(char) < ASCII_SPACE for char in name)
    ):
        raise ArtifactError("Invalid Docker archive member path")
    path = PurePosixPath(name)
    if (
        path.is_absolute()
        or ".." in path.parts
        or str(path) != name.rstrip("/")
        or str(path) == "."
    ):
        raise ArtifactError("Noncanonical Docker archive member path")
    return str(path)


@dataclass(frozen=True, slots=True, kw_only=True)
class ArchiveReader:
    """Bounded metadata reader tied to one open archive and its validated index."""

    contents: tarfile.TarFile
    members: dict[str, tarfile.TarInfo]

    @classmethod
    def indexed(cls, contents: tarfile.TarFile) -> "ArchiveReader":
        """Index regular members without extracting or running their contents."""
        members: dict[str, tarfile.TarInfo] = {}
        for member in contents:
            name = _member_name(member.name)
            if name in members or not (member.isfile() or member.isdir()):
                raise ArtifactError("Duplicate or nonregular Docker archive member")
            members[name] = member
            if len(members) > MAX_ARCHIVE_MEMBERS:
                raise ArtifactError("Too many Docker archive members")
        return cls(contents=contents, members=members)

    def read_bytes(self, name: JsonValue) -> bytes:
        """Read one regular metadata file within the JSON size budget."""
        member = self.members.get(_member_name(name))
        if member is None or not member.isfile() or not 0 < member.size <= MAX_JSON_BYTES:
            raise ArtifactError("Missing or oversized Docker image metadata")
        stream = self.contents.extractfile(member)
        if stream is None:
            raise ArtifactError("Unreadable Docker image metadata")
        with stream:
            return stream.read(MAX_JSON_BYTES + 1)

    def read_json(self, name: JsonValue) -> JsonValue:
        """Decode bounded metadata, rejecting duplicate fields."""
        return _json(self.read_bytes(name))

    def descriptor_path(self, value: JsonValue) -> str:
        """Validate an OCI descriptor before following its local member path."""
        if not isinstance(value, dict):
            raise ArtifactError("Invalid OCI descriptor digest")
        digest = value.get("digest")
        if not _matches(digest, r"sha256:[a-f0-9]{64}"):
            raise ArtifactError("Invalid OCI descriptor digest")
        name = "blobs/sha256/" + digest.split(":", 1)[1]
        member = self.members.get(name)
        if (
            member is None
            or not member.isfile()
            or type(value.get("size")) is not int
            or value["size"] != member.size
        ):
            raise ArtifactError("OCI descriptor size or member mismatch")
        if value.get("urls"):
            raise ArtifactError("External OCI descriptor URLs are unsupported")
        return name

    def read_descriptor(self, descriptor: JsonObject) -> JsonValue:
        """Read and authenticate OCI metadata against its declared digest."""
        data = self.read_bytes(self.descriptor_path(descriptor))
        value = _json(data)
        if descriptor["digest"] != "sha256:" + hashlib.sha256(data).hexdigest():
            raise ArtifactError("OCI metadata digest mismatch")
        return value

    def validate_layers(self, layers: JsonValue) -> list[JsonValue]:
        """Ensure every Docker layer resolves to a regular archive member."""
        if not isinstance(layers, list) or not layers:
            raise ArtifactError("Docker image has no layers")
        for layer in layers:
            member = self.members.get(_member_name(layer))
            if member is None or not member.isfile():
                raise ArtifactError("Docker image layer is missing or nonregular")
        return layers

    def validate_oci(self, image: JsonObject, config: JsonValue, manifest: Manifest) -> None:
        """Require Docker and OCI indexes to select the same image and layers."""
        if "index.json" not in self.members and "oci-layout" not in self.members:
            return
        if self.read_json("oci-layout") != {"imageLayoutVersion": "1.0.0"}:
            raise ArtifactError("Unsupported OCI image layout")
        index = self.read_json("index.json")
        for depth in range(MAX_INDEX_DEPTH):
            descriptor = _index_descriptor(index)
            _validate_annotations(descriptor.get("annotations", {}), manifest, outermost=depth == 0)
            resolved = self.read_descriptor(descriptor)
            if descriptor.get("mediaType") in (
                "application/vnd.oci.image.index.v1+json",
                "application/vnd.docker.distribution.manifest.list.v2+json",
            ):
                index = resolved
                continue
            if (
                descriptor.get("mediaType")
                not in (
                    "application/vnd.oci.image.manifest.v1+json",
                    "application/vnd.docker.distribution.manifest.v2+json",
                )
                or not isinstance(resolved, dict)
                or resolved.get("schemaVersion") != OCI_SCHEMA_VERSION
            ):
                raise ArtifactError("Unsupported OCI image manifest")
            oci_config = resolved.get("config")
            if (
                not isinstance(oci_config, dict)
                or self.descriptor_path(oci_config) != image["Config"]
                or self.read_descriptor(oci_config) != config
            ):
                raise ArtifactError("Docker and OCI image configurations disagree")
            layers = resolved.get("layers")
            if (
                not isinstance(layers, list)
                or [self.descriptor_path(layer) for layer in layers] != image["Layers"]
            ):
                raise ArtifactError("Docker and OCI image layers disagree")
            return
        raise ArtifactError("OCI image index nesting is too deep")


def _index_descriptor(index: JsonValue) -> JsonObject:
    if not isinstance(index, dict) or index.get("schemaVersion") != OCI_SCHEMA_VERSION:
        raise ArtifactError("Invalid OCI index")
    descriptors = index.get("manifests")
    if (
        not isinstance(descriptors, list)
        or len(descriptors) != 1
        or not isinstance(descriptors[0], dict)
    ):
        raise ArtifactError("OCI index must reference exactly one image")
    return descriptors[0]


def _validate_annotations(value: JsonValue, manifest: Manifest, *, outermost: bool) -> None:
    if not isinstance(value, dict):
        raise ArtifactError("Invalid OCI annotations")
    image_name = value.get("io.containerd.image.name")
    reference_name = value.get("org.opencontainers.image.ref.name")
    valid_names = (manifest["imageTag"], "docker.io/" + manifest["imageTag"])
    if (image_name is not None and image_name not in valid_names) or (
        reference_name is not None and reference_name not in (*valid_names, manifest["revision"])
    ):
        raise ArtifactError("OCI index tag does not match release")
    if outermost and image_name is None and reference_name is None:
        raise ArtifactError("OCI index has no declared release tag")


def _validate_config(config: JsonValue, manifest: Manifest) -> JsonObject:
    if (
        not isinstance(config, dict)
        or config.get("os") != "linux"
        or config.get("architecture") != "amd64"
    ):
        raise ArtifactError("Docker image platform does not match linux/amd64")
    runtime = config.get("config")
    if not isinstance(runtime, dict) or runtime.get("User") != "10001:10001":
        raise ArtifactError("Docker image must run as 10001:10001")
    labels = runtime.get("Labels")
    if (
        not isinstance(labels, dict)
        or labels.get("org.opencontainers.image.revision") != manifest["revision"]
    ):
        raise ArtifactError("Docker image revision label does not match")
    if runtime.get("Cmd") != ["/app/simplestChat"] or runtime.get("Entrypoint") not in (None, []):
        raise ArtifactError("Docker image must start only the production server")
    return config


def verify_archive(archive: Path, manifest: Manifest) -> JsonObject:
    """Verify transfer identity and one least-privileged production image.

    ``manifest`` must be the result of ``validate_manifest``. Image layers are
    never unpacked or executed; only bounded JSON metadata is read from the tar.
    """
    if (
        archive.is_symlink()
        or not archive.is_file()
        or not 0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES
    ):
        raise ArtifactError("Image archive must be a bounded regular file")
    if sha256_file(archive) != manifest["archiveSha256"]:
        raise ArtifactError("Image archive SHA256 does not match release manifest")
    try:
        with tarfile.open(archive, "r:") as contents:
            reader = ArchiveReader.indexed(contents)
            images = reader.read_json("manifest.json")
            if not isinstance(images, list) or len(images) != 1 or not isinstance(images[0], dict):
                raise ArtifactError("Archive must contain exactly one Docker image")
            image = images[0]
            if image.get("RepoTags") != [manifest["imageTag"]]:
                raise ArtifactError("Archive must contain exactly the declared release tag")
            _ = reader.validate_layers(image.get("Layers"))
            config = reader.read_json(image.get("Config"))
            reader.validate_oci(image, config, manifest)
            if "repositories" in reader.members:
                repositories = reader.read_json("repositories")
                repository, tag = manifest["imageTag"].rsplit(":", 1)
                tags = repositories.get(repository) if isinstance(repositories, dict) else None
                if (
                    not isinstance(repositories, dict)
                    or set(repositories) != {repository}
                    or not isinstance(tags, dict)
                    or set(tags) != {tag}
                ):
                    raise ArtifactError("Legacy Docker repository tags do not match release")
            return _validate_config(config, manifest)
    except (tarfile.TarError, OSError) as error:
        raise ArtifactError("Cannot read Docker image archive") from error
