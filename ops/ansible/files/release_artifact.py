"""Validate the portable, registry-free simplestChat production artifact.

Both the off-host builder and host importer use these checks. Validation reads
the Docker archive without extracting or loading it. Its digest and single tag
identify the transfer; Docker's target-local image ID is recorded by the importer.
"""

from datetime import datetime
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tarfile

MAX_JSON_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 8 * 1024**3
MANIFEST_KEYS = {
    "schemaVersion", "revision", "platform", "archiveSha256", "imageTag",
    "migrations", "createdAt",
}


class ArtifactError(ValueError):
    """The artifact is incomplete or outside the supported release contract."""


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ArtifactError(f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def _json(data):
    try:
        return json.loads(data, object_pairs_hook=_object)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ArtifactError("Invalid artifact JSON") from error


def _matches(pattern, value):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def validate_manifest(path: Path) -> dict:
    """Read a bounded manifest and reject unknown or ambiguous schema fields."""
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= MAX_JSON_BYTES:
        raise ArtifactError("Release manifest must be a bounded regular file")
    manifest = _json(path.read_bytes())
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise ArtifactError("Unsupported release manifest fields")
    if type(manifest["schemaVersion"]) is not int or manifest["schemaVersion"] != 1:
        raise ArtifactError("Unsupported release schema")
    if not _matches(r"[a-f0-9]{40}", manifest["revision"]):
        raise ArtifactError("Release revision must be a full Git commit ID")
    if manifest["platform"] != "linux/amd64":
        raise ArtifactError("Only linux/amd64 releases are supported")
    if not _matches(r"[a-f0-9]{64}", manifest["archiveSha256"]):
        raise ArtifactError("Invalid archive SHA256")
    if manifest["imageTag"] != f"simplestchat-release/production:{manifest['revision']}":
        raise ArtifactError("Release tag does not match its revision")
    migrations = manifest["migrations"]
    if not isinstance(migrations, dict) or not 0 < len(migrations) <= 1000:
        raise ArtifactError("Release migrations must be a nonempty bounded map")
    for version, checksum in migrations.items():
        if not _matches(r"[1-9][0-9]{0,18}", version) or int(version) > 2**63 - 1:
            raise ArtifactError("Migration versions must be canonical positive decimal integers")
        if not _matches(r"[a-f0-9]{96}", checksum):
            raise ArtifactError("Invalid migration SHA384")
    timestamp = manifest["createdAt"]
    if not _matches(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", timestamp):
        raise ArtifactError("Release timestamp must be UTC with second precision")
    try:
        datetime.strptime(timestamp, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError as error:
        raise ArtifactError("Invalid release timestamp") from error
    return manifest


def sha256_file(path: Path) -> str:
    """Hash large archives in bounded chunks."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _member_name(name):
    if not isinstance(name, str) or not name or "\\" in name or any(ord(char) < 32 for char in name):
        raise ArtifactError("Invalid Docker archive member path")
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or str(path) != name.rstrip("/") or str(path) == ".":
        raise ArtifactError("Noncanonical Docker archive member path")
    return str(path)


def verify_archive(archive: Path, manifest: dict) -> dict:
    """Verify transfer identity and one least-privileged production image.

    ``manifest`` must be the result of ``validate_manifest``. Image layers are
    never unpacked or executed; only bounded JSON metadata is read from the tar.
    """
    if archive.is_symlink() or not archive.is_file() or not 0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES:
        raise ArtifactError("Image archive must be a bounded regular file")
    if sha256_file(archive) != manifest["archiveSha256"]:
        raise ArtifactError("Image archive SHA256 does not match release manifest")
    try:
        with tarfile.open(archive, "r:") as contents:
            members = {}
            for member in contents:
                name = _member_name(member.name)
                if name in members or not (member.isfile() or member.isdir()):
                    raise ArtifactError("Duplicate or nonregular Docker archive member")
                members[name] = member
                if len(members) > 100000:
                    raise ArtifactError("Too many Docker archive members")

            def read_json(name):
                member = members.get(_member_name(name))
                if member is None or not member.isfile() or not 0 < member.size <= MAX_JSON_BYTES:
                    raise ArtifactError("Missing or oversized Docker image metadata")
                stream = contents.extractfile(member)
                if stream is None:
                    raise ArtifactError("Unreadable Docker image metadata")
                with stream:
                    return _json(stream.read(MAX_JSON_BYTES + 1))

            def descriptor_path(descriptor):
                if not isinstance(descriptor, dict) or not _matches(r"sha256:[a-f0-9]{64}", descriptor.get("digest")):
                    raise ArtifactError("Invalid OCI descriptor digest")
                name = "blobs/sha256/" + descriptor["digest"].split(":", 1)[1]
                member = members.get(name)
                if member is None or not member.isfile() or type(descriptor.get("size")) is not int or descriptor["size"] != member.size:
                    raise ArtifactError("OCI descriptor size or member mismatch")
                if descriptor.get("urls"):
                    raise ArtifactError("External OCI descriptor URLs are unsupported")
                return name

            def read_descriptor(descriptor):
                name = descriptor_path(descriptor)
                value = read_json(name)
                with contents.extractfile(members[name]) as stream:
                    digest = hashlib.sha256(stream.read(MAX_JSON_BYTES + 1)).hexdigest()
                if descriptor["digest"] != "sha256:" + digest:
                    raise ArtifactError("OCI metadata digest mismatch")
                return value

            images = read_json("manifest.json")
            if not isinstance(images, list) or len(images) != 1 or not isinstance(images[0], dict):
                raise ArtifactError("Archive must contain exactly one Docker image")
            image = images[0]
            if image.get("RepoTags") != [manifest["imageTag"]]:
                raise ArtifactError("Archive must contain exactly the declared release tag")
            layers = image.get("Layers")
            if not isinstance(layers, list) or not layers:
                raise ArtifactError("Docker image has no layers")
            for layer in layers:
                member = members.get(_member_name(layer))
                if member is None or not member.isfile():
                    raise ArtifactError("Docker image layer is missing or nonregular")
            config = read_json(image.get("Config"))
            # Docker Engine's containerd store exports both layouts. Validate the
            # secondary index too: checking manifest.json alone could admit an
            # archive that imports a different image or additional tags.
            if "index.json" in members or "oci-layout" in members:
                if read_json("oci-layout") != {"imageLayoutVersion": "1.0.0"}:
                    raise ArtifactError("Unsupported OCI image layout")
                index = read_json("index.json")
                for depth in range(8):
                    if not isinstance(index, dict) or index.get("schemaVersion") != 2:
                        raise ArtifactError("Invalid OCI index")
                    descriptors = index.get("manifests")
                    if not isinstance(descriptors, list) or len(descriptors) != 1 or not isinstance(descriptors[0], dict):
                        raise ArtifactError("OCI index must reference exactly one image")
                    descriptor = descriptors[0]
                    annotations = descriptor.get("annotations", {})
                    if not isinstance(annotations, dict):
                        raise ArtifactError("Invalid OCI annotations")
                    image_name = annotations.get("io.containerd.image.name")
                    reference_name = annotations.get("org.opencontainers.image.ref.name")
                    valid_names = (manifest["imageTag"], "docker.io/" + manifest["imageTag"])
                    if (image_name is not None and image_name not in valid_names) or (reference_name is not None and reference_name not in (*valid_names, manifest["revision"])):
                        raise ArtifactError("OCI index tag does not match release")
                    if depth == 0 and image_name is None and reference_name is None:
                        raise ArtifactError("OCI index has no declared release tag")
                    resolved = read_descriptor(descriptor)
                    if descriptor.get("mediaType") in (
                        "application/vnd.oci.image.index.v1+json",
                        "application/vnd.docker.distribution.manifest.list.v2+json",
                    ):
                        index = resolved
                        continue
                    if descriptor.get("mediaType") not in (
                        "application/vnd.oci.image.manifest.v1+json",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    ) or not isinstance(resolved, dict) or resolved.get("schemaVersion") != 2:
                        raise ArtifactError("Unsupported OCI image manifest")
                    if descriptor_path(resolved.get("config")) != image["Config"] or read_descriptor(resolved["config"]) != config:
                        raise ArtifactError("Docker and OCI image configurations disagree")
                    oci_layers = resolved.get("layers")
                    if not isinstance(oci_layers, list) or [descriptor_path(layer) for layer in oci_layers] != layers:
                        raise ArtifactError("Docker and OCI image layers disagree")
                    break
                else:
                    raise ArtifactError("OCI image index nesting is too deep")
            if "repositories" in members:
                repositories = read_json("repositories")
                repository, tag = manifest["imageTag"].rsplit(":", 1)
                if not isinstance(repositories, dict) or set(repositories) != {repository} or not isinstance(repositories[repository], dict) or set(repositories[repository]) != {tag}:
                    raise ArtifactError("Legacy Docker repository tags do not match release")
            if not isinstance(config, dict) or config.get("os") != "linux" or config.get("architecture") != "amd64":
                raise ArtifactError("Docker image platform does not match linux/amd64")
            runtime = config.get("config")
            if not isinstance(runtime, dict) or runtime.get("User") != "10001:10001":
                raise ArtifactError("Docker image must run as 10001:10001")
            labels = runtime.get("Labels")
            if not isinstance(labels, dict) or labels.get("org.opencontainers.image.revision") != manifest["revision"]:
                raise ArtifactError("Docker image revision label does not match")
            if runtime.get("Cmd") != ["/app/simplestChat"] or runtime.get("Entrypoint") not in (None, []):
                raise ArtifactError("Docker image must start only the production server")
            return config
    except (tarfile.TarError, OSError) as error:
        raise ArtifactError("Cannot read Docker image archive") from error
