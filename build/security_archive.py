"""Materialize bounded image evidence without executing or trusting archive paths.

Every regular file from every layer is retained for secret scanning, including
files later removed by whiteouts. The final filesystem is built separately.
Tar ownership, permissions and device entries never control host resources.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import posixpath
import re
import sys
import tarfile
import zlib
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from typing import IO

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import release_artifact
from release_json import JsonObject, array_value, object_value, string_value

MAX_ARCHIVE = 8 * 1024**3
MAX_LAYER = 2 * 1024**3
MAX_EXPANDED = 4 * 1024**3
MAX_FILE = 256 * 1024**2
MAX_MEMBERS = 200000
MAX_LAYERS = 128
MAX_PATH = 4096
MAX_METADATA = 16384
MAX_METADATA_TOTAL = 8 * 1024 * 1024
CHUNK = 65536
BLOCK = 512
CONTROL = 32
MAX_LINKS = 40
WHITEOUT = ".wh."
OPAQUE = ".wh..wh..opq"
ALLOWED_HEADERS = frozenset(
    {
        tarfile.REGTYPE,
        tarfile.AREGTYPE,
        tarfile.DIRTYPE,
        tarfile.SYMTYPE,
        tarfile.LNKTYPE,
        tarfile.XHDTYPE,
        tarfile.XGLTYPE,
        tarfile.GNUTYPE_LONGNAME,
        tarfile.GNUTYPE_LONGLINK,
    }
)
METADATA_HEADERS = frozenset(
    {tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK}
)


class ArchiveError(ValueError):
    """One fixed failure class for untrusted image metadata or resource bounds."""


def require(condition: object, reason: str) -> None:
    """Reject an incomplete or unsafe archive without echoing its contents."""
    if not condition:
        raise ArchiveError(reason)


def canonical(name: str, *, directory: bool = False) -> str:
    """Accept ordinary image tar paths, including a single conventional dot prefix."""
    value = name.removeprefix("./").rstrip("/") if directory else name.removeprefix("./")
    require(0 < len(value) <= MAX_PATH and "\\" not in value, "image_member_path")
    path = PurePosixPath(value)
    require(
        str(path) == value
        and value != "."
        and not path.is_absolute()
        and ".." not in path.parts
        and not any(ord(character) < CONTROL for character in value),
        "image_member_path",
    )
    return value


def physical_headers(source: IO[bytes], *, outer: bool = False) -> None:
    """Bound extended metadata before tarfile may allocate or interpret it."""
    total = source.seek(0, os.SEEK_END)
    _ = source.seek(0)
    count = 0
    metadata_bytes = 0
    while source.tell() < total:
        block = source.read(BLOCK)
        require(len(block) == BLOCK, "image_tar_truncated")
        if not any(block):
            require(source.read(BLOCK) == bytes(BLOCK), "image_tar_terminator")
            while chunk := source.read(CHUNK):
                require(not any(chunk), "image_tar_trailing_data")
            _ = source.seek(0)
            return
        header = tarfile.TarInfo.frombuf(block, "utf-8", "strict")
        count += 1
        require(count <= MAX_MEMBERS and header.type in ALLOWED_HEADERS, "image_tar_header")
        require(
            not outer or header.type not in (tarfile.SYMTYPE, tarfile.LNKTYPE), "image_outer_link"
        )
        limit = (
            MAX_METADATA
            if header.type in METADATA_HEADERS
            else (MAX_ARCHIVE if outer else MAX_FILE)
        )
        require(0 <= header.size <= limit, "image_tar_body_size")
        if header.type in METADATA_HEADERS:
            metadata_bytes += header.size
            require(metadata_bytes <= MAX_METADATA_TOTAL, "image_tar_metadata_total")
        end = source.tell() + ((header.size + BLOCK - 1) // BLOCK) * BLOCK
        require(end <= total, "image_tar_truncated")
        _ = source.seek(end)
    reason = "image_tar_terminator"
    raise ArchiveError(reason)


def new_file(path: Path, data: bytes) -> None:
    """Publish only a new private regular file at a caller-chosen path."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as destination:
        _ = destination.write(data)


def layer_file(source: IO[bytes], destination: Path, expected: str, blob_name: str) -> int:
    """Authenticate the uncompressed diff ID with bounded optional gzip expansion."""
    prefix = source.read(2)
    decoder = zlib.decompressobj(31) if prefix == b"\x1f\x8b" else None
    raw_digest, digest = hashlib.sha256(), hashlib.sha256()
    total = 0
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        chunk = prefix
        while chunk:
            raw_digest.update(chunk)
            content = decoder.decompress(chunk, MAX_LAYER - total + 1) if decoder else chunk
            total += len(content)
            require(total <= MAX_LAYER, "image_layer_expansion")
            if decoder:
                require(
                    not decoder.unused_data and not decoder.unconsumed_tail,
                    "image_layer_compression",
                )
            digest.update(content)
            _ = output.write(content)
            chunk = source.read(CHUNK)
        require(decoder is None or decoder.eof, "image_layer_compression")
    require("sha256:" + digest.hexdigest() == expected, "image_layer_diff_id")
    if re.fullmatch(r"blobs/sha256/[a-f0-9]{64}", blob_name):
        require(raw_digest.hexdigest() == blob_name.rsplit("/", 1)[1], "image_layer_blob_digest")
    return total


@dataclass(frozen=True)
class Entry:
    """Image metadata plus a host-controlled regular-file evidence location."""

    kind: str
    content: Path | None = None
    link: str = ""


def copy_member(bundle: tarfile.TarFile, member: tarfile.TarInfo, path: Path) -> str:
    """Stream a regular member into safe directories, never archive-provided links."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    source = bundle.extractfile(member)
    require(source is not None, "image_member_missing")
    assert source is not None  # noqa: S101 -- Runtime guard above narrows the type.
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    digest = hashlib.sha256()
    with source, os.fdopen(descriptor, "wb") as output:
        remaining = member.size
        while remaining:
            chunk = source.read(min(remaining, CHUNK))
            require(bool(chunk), "image_member_truncated")
            _ = output.write(chunk)
            digest.update(chunk)
            remaining -= len(chunk)
    return digest.hexdigest()


def read_layer(path: Path, destination: Path) -> tuple[dict[str, Entry], list[str], list[str], int]:
    """Retain all regular layer content and validate metadata before overlay updates."""
    with path.open("rb") as source:
        physical_headers(source)
    entries: dict[str, Entry] = {}
    whiteouts: list[str] = []
    opaque: list[str] = []
    count = 0
    with tarfile.open(path, "r:") as bundle:
        for member in bundle:
            count += 1
            require(count <= MAX_MEMBERS, "image_member_count")
            if member.isdir() and member.name in (".", "./"):
                continue
            name = canonical(member.name, directory=member.isdir())
            require(name not in entries, "image_member_duplicate")
            require(
                member.sparse is None
                and not any(key.startswith("GNU.sparse") for key in member.pax_headers),
                "image_sparse_member",
            )
            filename = PurePosixPath(name).name
            parent = str(PurePosixPath(name).parent)
            if filename.startswith(WHITEOUT):
                require(member.isfile() and member.size == 0, "image_whiteout_shape")
                if filename == OPAQUE:
                    opaque.append(parent)
                else:
                    require(
                        len(filename) > len(WHITEOUT)
                        and filename[len(WHITEOUT) :] not in (".", ".."),
                        "image_whiteout_name",
                    )
                    whiteouts.append(str(PurePosixPath(parent) / filename[len(WHITEOUT) :]))
                entries[name] = Entry("whiteout")
            elif member.isfile():
                require(0 <= member.size <= MAX_FILE and not member.linkname, "image_regular_shape")
                content = destination / name
                _ = copy_member(bundle, member, content)
                entries[name] = Entry("file", content)
            elif member.isdir():
                require(member.size == 0 and not member.linkname, "image_directory_shape")
                entries[name] = Entry("directory")
            elif member.issym() or member.islnk():
                require(
                    member.size == 0 and 0 < len(member.linkname) <= MAX_PATH, "image_link_shape"
                )
                require(
                    not any(ord(character) < CONTROL for character in member.linkname),
                    "image_link_target",
                )
                entries[name] = Entry(
                    "symlink" if member.issym() else "hardlink", link=member.linkname
                )
            else:
                reason = "image_member_kind"
                raise ArchiveError(reason)
    return entries, whiteouts, opaque, count


def remove_tree(entries: dict[str, Entry], name: str, *, children_only: bool = False) -> None:
    """Apply whiteouts only to the prior image state, never newly added layer files."""
    for path in list(entries):
        if (not children_only and path == name) or name == "." or path.startswith(name + "/"):
            del entries[path]


def overlay(
    current: dict[str, Entry], incoming: dict[str, Entry], whiteouts: list[str], opaque: list[str]
) -> None:
    """Combine validated metadata without following any host filesystem link."""
    for name in whiteouts:
        remove_tree(current, name)
    for name in opaque:
        remove_tree(current, name, children_only=True)
    # Most image entries are new files. Index prior directory prefixes once so
    # adding N ordinary RPM files does not scan all prior files N times.
    parents = {str(parent) for name in current for parent in PurePosixPath(name).parents}
    for name, entry in sorted(
        incoming.items(), key=lambda item: (len(PurePosixPath(item[0]).parts), item[0])
    ):
        if entry.kind == "whiteout":
            continue
        if entry.kind != "directory" or (name in current and current[name].kind != "directory"):
            if name in parents:
                remove_tree(current, name)
            else:
                _ = current.pop(name, None)
        current[name] = entry
    for name in current:
        for parent in PurePosixPath(name).parents:
            ancestor = current.get(str(parent))
            require(ancestor is None or ancestor.kind == "directory", "image_parent_not_directory")
    resolve_hardlinks(current)


def resolve_hardlinks(current: dict[str, Entry]) -> None:
    """Capture this layer's hardlinked bytes before later target replacement."""
    for name, entry in list(current.items()):
        selected = entry
        links = 0
        while selected.kind == "hardlink":
            links += 1
            require(links <= MAX_LINKS, "image_hardlink_cycle")
            target = link_target(name, selected.link, hard=True)
            require(target in current, "image_hardlink_missing")
            selected = current[target]
        if entry.kind == "hardlink":
            require(selected.kind == "file", "image_hardlink_kind")
            current[name] = selected


def link_target(name: str, target: str, *, hard: bool = False) -> str:
    """Normalize links in the image namespace before any link is materialized."""
    relative = (
        target.lstrip("/")
        if target.startswith("/") or hard
        else str(PurePosixPath(name).parent / target)
    )
    normalized = posixpath.normpath(relative)
    require(
        normalized != ".." and not normalized.startswith("../") and "\\" not in normalized,
        "image_link_escape",
    )
    return normalized


def materialize(entries: dict[str, Entry], root: Path) -> None:
    """Create only host-owned paths; convert absolute image symlinks to safe relative ones."""
    root.mkdir(mode=0o700)
    for name, entry in entries.items():
        path = root / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if entry.kind == "directory":
            path.mkdir(mode=0o700, exist_ok=True)
        elif entry.kind == "file":
            require(entry.content is not None, "image_file_content")
            assert entry.content is not None  # noqa: S101 -- Explicit guard above.
            os.link(entry.content, path, follow_symlinks=False)
    for name, entry in entries.items():
        if entry.kind == "symlink":
            target = link_target(name, entry.link)
            path = root / name
            relative = posixpath.relpath(target, str(PurePosixPath(name).parent))
            path.symlink_to(relative)


def extract(archive: Path, manifest_path: Path, output: Path) -> dict[str, object]:
    """Require one authenticated image and retain both complete layer and final-tree evidence."""
    require(
        not archive.is_symlink() and archive.is_file() and archive.stat().st_size <= MAX_ARCHIVE,
        "image_archive_file",
    )
    with archive.open("rb") as source:
        physical_headers(source, outer=True)
    manifest = release_artifact.validate_manifest(manifest_path)
    config = release_artifact.verify_archive(archive, manifest)
    report = materialize_layers(archive, config, output)
    report.update(
        {
            "archiveSha256": manifest["archiveSha256"],
            "revision": manifest["revision"],
            "platform": manifest["platform"],
        }
    )
    new_file(output / "report.json", (json.dumps(report, indent=2) + "\n").encode())
    return report


def materialize_layers(archive: Path, config: JsonObject, output: Path) -> dict[str, object]:
    """Materialize authenticated Docker-save layers; caller binds config/image identity.

    This parser core also permits native-architecture development fixtures. The
    public extraction command still enforces the canonical amd64 release manifest.
    """
    diff_ids = array_value(object_value(config["rootfs"])["diff_ids"])
    require(0 < len(diff_ids) <= MAX_LAYERS, "image_layer_count")
    output.mkdir(mode=0o700)
    layers = output / "layers"
    layers.mkdir(mode=0o700)
    entries: dict[str, Entry] = {}
    expanded = 0
    members = 0
    records: list[dict[str, object]] = []
    new_file(layers / "image-config.json", (json.dumps(config, indent=2) + "\n").encode())
    with tarfile.open(archive, "r:") as bundle:
        reader = release_artifact.ArchiveReader.indexed(bundle)
        image = object_value(array_value(reader.read_json("manifest.json"))[0])
        names = array_value(image["Layers"])
        require(len(names) == len(diff_ids), "image_layer_count_mismatch")
        for index, (name_value, expected_value) in enumerate(zip(names, diff_ids, strict=True)):
            name, expected = string_value(name_value), string_value(expected_value)
            require(re.fullmatch(r"sha256:[a-f0-9]{64}", expected), "image_diff_id_shape")
            source = bundle.extractfile(reader.members[name])
            require(source is not None, "image_layer_missing")
            assert source is not None  # noqa: S101 -- Explicit guard above.
            raw = output / f"layer-{index:03d}.tar"
            with source:
                size = layer_file(source, raw, expected, name)
            expanded += size
            require(expanded <= MAX_EXPANDED, "image_total_expansion")
            destination = layers / f"{index:03d}"
            destination.mkdir(mode=0o700)
            incoming, whiteouts, opaque, count = read_layer(raw, destination)
            members += count
            require(members <= MAX_MEMBERS, "image_total_members")
            overlay(entries, incoming, whiteouts, opaque)
            records.append(
                {"index": index, "diffId": expected, "expandedBytes": size, "members": count}
            )
            raw.unlink()
    materialize(entries, output / "rootfs")
    return {
        "passed": True,
        "expandedBytes": expanded,
        "members": members,
        "layers": records,
        "regularLayerFilesPreserved": True,
        "configIncluded": True,
    }


@dataclass
class Options(argparse.Namespace):
    """Typed arguments for the bounded subprocess entry point."""

    archive: Path = Path()
    manifest: Path = Path()
    output: Path = Path()


def main() -> int:
    """Expose extraction as a separately deadline-bound controller subprocess."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("archive", "manifest", "output"):
        _ = parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args(namespace=Options())
    try:
        _ = extract(args.archive, args.manifest, args.output)
    except (
        ArchiveError,
        release_artifact.ArtifactError,
        OSError,
        ValueError,
        tarfile.TarError,
        zlib.error,
    ) as error:
        reason = str(error) if isinstance(error, ArchiveError) else type(error).__name__
        print("Image archive check failed: " + reason, file=sys.stderr)  # noqa: T201 -- CLI diagnostic.
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
