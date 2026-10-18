"""Validate and atomically retain only fingerprinted public browser assets.

Docker's fixed, bounded cp command supplies an uncompressed directory tar from
an identity-checked, never-started image container. Nothing is extracted using
tar paths. Only validated flat public files are written to this root-owned store.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import stat
import tarfile
import uuid
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path
from typing import TypedDict

from release_json import JsonValue, array_value, decode_json, object_value, string_value

MAX_FILE = 8 * 1024 * 1024
MAX_TOTAL = 32 * 1024 * 1024
MAX_TAR = MAX_TOTAL + 1024 * 1024
MAX_INDEX = 256 * 1024
MAX_FILES = 256
MAX_GENERATIONS = 4
MAX_CAPTURES = 2
MAX_VERSION = 128
MAX_AGE = 7 * 86400
CONTAINER_PATH = "/run/simplestchat-browser-assets"
NAME = re.compile(
    r"[A-Za-z0-9_-]+-[A-Za-z0-9_-]{6,64}\.(?:js|css|woff2?|svg|png|jpe?g|webp|avif|ico)"
)
REVISION = re.compile(r"[a-f0-9]{40}")
SHA256 = re.compile(r"[a-f0-9]{64}")


class AssetError(ValueError):
    """Fixed diagnostics never contain tar names or asset contents."""


class AssetRecord(TypedDict):
    """The immutable public content identity."""

    sha256: str
    size: int


class Generation(TypedDict):
    """A bounded release generation in the public index."""

    revision: str
    createdAt: int
    expiresAt: int
    assets: dict[str, AssetRecord]


def require(condition: object, code: str) -> None:
    """Fail closed with a fixed non-sensitive reason."""
    if not condition:
        raise AssetError(code)


def read_capture(archive: Path) -> dict[str, bytes]:
    """Read docker cp's directory tar without ever extracting its paths."""
    require(not archive.is_symlink() and 0 < archive.stat().st_size <= MAX_TAR, "asset_tar_size")
    assets: dict[str, bytes] = {}
    seen: set[str] = set()
    total = 0
    with tarfile.open(archive, "r:") as source:
        for member in source:
            require(len(seen) <= MAX_FILES, "asset_tar_count")
            require(member.name not in seen, "asset_tar_duplicate")
            seen.add(member.name)
            if member.name in ("assets", "assets/"):
                require(member.isdir(), "asset_tar_root")
                continue
            prefix, separator, name = member.name.partition("/")
            require(prefix == "assets" and separator and NAME.fullmatch(name), "asset_tar_path")
            require(member.isfile() and not member.linkname, "asset_tar_type")
            require(0 < member.size <= MAX_FILE, "asset_tar_file_size")
            total += member.size
            require(total <= MAX_TOTAL and len(assets) < MAX_FILES, "asset_tar_total")
            stream = source.extractfile(member)
            if stream is None:
                message = "asset_tar_content"
                raise AssetError(message)
            with stream:
                content = stream.read(MAX_FILE + 1)
            require(len(content) == member.size, "asset_tar_content_size")
            assets[name] = content
    require(assets, "asset_tar_empty")
    return assets


def protected(path: Path, owner: int, *, directory: bool = False) -> None:
    """Only the release owner may replace public store bytes or directory entries."""
    metadata = path.lstat()
    require(
        metadata.st_uid == owner
        and not stat.S_ISLNK(metadata.st_mode)
        and not metadata.st_mode & 0o022
        and (stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)),
        "asset_store_ownership",
    )


def record(content: bytes) -> AssetRecord:
    """Record a public file's digest and length."""
    return {"sha256": hashlib.sha256(content).hexdigest(), "size": len(content)}


def read_generation(root: Path, value: JsonValue, owner: int) -> Generation:
    """Validate the exact index schema and every referenced immutable byte."""
    item = object_value(value)
    require(
        set(item) == {"revision", "createdAt", "expiresAt", "assets"}, "asset_generation_fields"
    )
    revision = string_value(item["revision"])
    created, expires = item["createdAt"], item["expiresAt"]
    require(REVISION.fullmatch(revision), "asset_revision")
    require(
        type(created) is int
        and type(expires) is int
        and created >= 0
        and expires == created + MAX_AGE,
        "asset_lifetime",
    )
    if not isinstance(created, int) or not isinstance(expires, int):
        message = "asset_lifetime"
        raise AssetError(message)
    directory = root / revision
    protected(directory, owner, directory=True)
    entries = object_value(item["assets"])
    require(0 < len(entries) <= MAX_FILES, "asset_generation_count")
    require({path.name for path in directory.iterdir()} == set(entries), "asset_generation_files")
    assets: dict[str, AssetRecord] = {}
    for name, entry in entries.items():
        require(NAME.fullmatch(name), "asset_name")
        value = object_value(entry)
        require(set(value) == {"sha256", "size"}, "asset_record_fields")
        digest, size = string_value(value["sha256"]), value["size"]
        require(
            SHA256.fullmatch(digest) and type(size) is int and 0 < size <= MAX_FILE, "asset_record"
        )
        protected(directory / name, owner)
        require((directory / name).stat().st_size == size, "asset_store_file_size")
        content = (directory / name).read_bytes()
        require(hashlib.sha256(content).hexdigest() == digest, "asset_store_digest")
        assets[name] = {"sha256": digest, "size": len(content)}
    return {"revision": revision, "createdAt": created, "expiresAt": expires, "assets": assets}


def read_store(root: Path, owner: int) -> list[Generation]:
    """Reject foreign state instead of adopting or deleting it."""
    protected(root, owner, directory=True)
    index = root / "index.json"
    if not index.exists() and not index.is_symlink():
        require(not any(root.iterdir()), "asset_store_unindexed")
        return []
    protected(index, owner)
    require(index.stat().st_size <= MAX_INDEX, "asset_index_size")
    value = object_value(decode_json(index.read_bytes()))
    require(
        set(value) == {"schemaVersion", "generations"} and value["schemaVersion"] == 1,
        "asset_index_schema",
    )
    entries = array_value(value["generations"])
    require(len(entries) <= MAX_GENERATIONS, "asset_index_generations")
    generations = [read_generation(root, entry, owner) for entry in entries]
    revisions = {entry["revision"] for entry in generations}
    require(len(revisions) == len(generations), "asset_index_duplicate")
    index_bytes = index.read_bytes()
    union = "union-" + hashlib.sha256(index_bytes).hexdigest()
    require(
        {path.name for path in root.iterdir()} == revisions | {"index.json", "public", union},
        "asset_store_foreign",
    )
    validate_public(root, generations, union, owner)
    require(sum(size(entry) for entry in generations) <= MAX_TOTAL, "asset_index_total")
    return generations


def size(generation: Generation) -> int:
    """Count stored bytes conservatively, including copies shared across releases."""
    return sum(asset["size"] for asset in generation["assets"].values())


def write_file(path: Path, content: bytes) -> None:
    """Create one owned file and settle its bytes before any index can reference it."""
    with path.open("xb") as stream:
        _ = stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())
    path.chmod(0o644)


def sync_directory(path: Path) -> None:
    """Settle renames on the managed Linux filesystem."""
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def identities(generations: list[Generation]) -> dict[str, tuple[str, AssetRecord]]:
    """One URL must always denote the same public bytes in every retained edition."""
    result: dict[str, tuple[str, AssetRecord]] = {}
    for generation in generations:
        for name, identity in generation["assets"].items():
            require(name not in result or result[name][1] == identity, "asset_immutable_collision")
            result[name] = (generation["revision"], identity)
    return result


def validate_public(root: Path, generations: list[Generation], union: str, owner: int) -> None:
    """Check the generated relative pointer and flat union used by the stable proxy."""
    public = root / "public"
    protected(public, owner, directory=True)
    names = {entry.name for entry in public.iterdir()}
    require(names in ({"assets"}, {"assets", "app-version.json"}), "asset_public_files")
    pointer = public / "assets"
    metadata = pointer.lstat()
    require(stat.S_ISLNK(metadata.st_mode) and metadata.st_uid == owner, "asset_public_pointer")
    require(str(pointer.readlink()) == "../" + union, "asset_public_target")
    directory = root / union
    protected(directory, owner, directory=True)
    expected = identities(generations)
    require({entry.name for entry in directory.iterdir()} == set(expected), "asset_union_files")
    for name, (_revision, identity) in expected.items():
        path = directory / name
        protected(path, owner)
        require(path.stat().st_size == identity["size"], "asset_union_size")
        require(
            hashlib.sha256(path.read_bytes()).hexdigest() == identity["sha256"],
            "asset_union_digest",
        )
    if "app-version.json" in names:
        protected(public / "app-version.json", owner)
        require((public / "app-version.json").stat().st_size <= MAX_VERSION, "asset_version_size")
        value = object_value(decode_json((public / "app-version.json").read_bytes()))
        require(
            set(value) == {"revision"} and REVISION.fullmatch(string_value(value["revision"])),
            "asset_version_schema",
        )


def new_generations(
    root: Path,
    pending: Path,
    captures: dict[str, dict[str, bytes]],
    existing: set[str],
    added: list[Path],
) -> None:
    """Publish immutable directories before making them reachable in an index."""
    for revision, files in captures.items():
        if revision in existing:
            continue
        directory = pending / revision
        directory.mkdir(mode=0o755)
        directory.chmod(0o755)
        for name, content in files.items():
            write_file(directory / name, content)
        sync_directory(directory)
        _ = directory.rename(root / revision)
        added.append(root / revision)


def commit(  # noqa: C901, PLR0912, PLR0915 - one index/pointer transaction owns cleanup until publication settles.
    root: Path,
    selected: list[Generation],
    captures: dict[str, dict[str, bytes]],
    prior: list[Generation],
) -> None:
    """Switch the proxy union after settling its manifest; refuse ambiguous crash residue."""
    expected = identities(selected)
    index = (
        json.dumps({"schemaVersion": 1, "generations": selected}, separators=(",", ":")).encode()
        + b"\n"
    )
    require(len(index) <= MAX_INDEX, "asset_index_size")
    previous = (root / "index.json").read_bytes() if (root / "index.json").exists() else None
    union = "union-" + hashlib.sha256(index).hexdigest()
    if previous == index:
        return
    pending = root / (".pending-" + uuid.uuid4().hex)
    pending.mkdir(mode=0o755)
    pending.chmod(0o755)
    added: list[Path] = []
    switched = False
    public_created = False
    failure: BaseException | None = None
    public = root / "public"
    try:
        new_generations(root, pending, captures, {entry["revision"] for entry in prior}, added)
        directory = pending / union
        directory.mkdir(mode=0o755)
        directory.chmod(0o755)
        for name, (revision, _identity) in expected.items():
            os.link(root / revision / name, directory / name)
        sync_directory(directory)
        _ = directory.rename(root / union)
        added.append(root / union)
        if not public.exists():
            public.mkdir(mode=0o755)
            public.chmod(0o755)
            public_created = True
        (pending / "assets").symlink_to("../" + union)
        write_file(pending / "index.json", index)
        _ = (pending / "index.json").replace(root / "index.json")
        _ = (pending / "assets").replace(public / "assets")
        switched = True
        sync_directory(public)
        sync_directory(root)
    except BaseException as error:
        failure = error
        # No application replacement can start while the release lock is held.
        # Restore the previous index only if it actually changed. Preserve the
        # primary failure if storage fails again during owned cleanup.
        try:
            if not switched:
                if previous is None:
                    (root / "index.json").unlink(missing_ok=True)
                elif (root / "index.json").read_bytes() != previous:
                    write_file(pending / "restore.json", previous)
                    _ = (pending / "restore.json").replace(root / "index.json")
                for directory in added:
                    shutil.rmtree(directory)
                if public_created and not any(public.iterdir()):
                    public.rmdir()
        except BaseException:  # noqa: BLE001, S110 - never hide the publication error; unfinished journal requires recovery.
            pass
        raise
    finally:
        try:
            shutil.rmtree(pending)
        except BaseException:
            if failure is None:
                raise
    retained = {entry["revision"] for entry in selected}
    for entry in prior:
        if entry["revision"] not in retained:
            shutil.rmtree(root / entry["revision"])
    if previous is not None:
        old_union = root / ("union-" + hashlib.sha256(previous).hexdigest())
        shutil.rmtree(old_union)
    sync_directory(root)


def initialize(root: Path, *, owner: int = 0) -> None:
    """Create an empty verified store before the readonly proxy mount is started."""
    if not root.exists() and not root.is_symlink():
        root.mkdir(mode=0o755)
        root.chmod(0o755)
    generations = read_store(root, owner)
    if not (root / "index.json").exists():
        commit(root, [], {}, generations)


def publish(
    root: Path, captures: dict[str, dict[str, bytes]], timestamp: int, *, owner: int = 0
) -> dict[str, int]:
    """Preserve selected+candidate assets; bound additional generations by count/age/bytes."""
    require(0 < len(captures) <= MAX_CAPTURES and timestamp >= 0, "asset_capture_count")
    initialize(root, owner=owner)
    generations = read_store(root, owner)
    by_revision = {entry["revision"]: entry for entry in generations}
    selected: list[Generation] = []
    for revision, files in captures.items():
        require(
            REVISION.fullmatch(revision) and 0 < len(files) <= MAX_FILES, "asset_capture_revision"
        )
        require(
            all(
                NAME.fullmatch(name) and 0 < len(content) <= MAX_FILE
                for name, content in files.items()
            ),
            "asset_capture_file",
        )
        records = {name: record(content) for name, content in files.items()}
        prior = by_revision.get(revision)
        require(prior is None or prior["assets"] == records, "asset_revision_changed")
        selected.append(
            {
                "revision": revision,
                "createdAt": timestamp,
                "expiresAt": timestamp + MAX_AGE,
                "assets": records,
            }
        )
    require(sum(size(entry) for entry in selected) <= MAX_TOTAL, "asset_required_capacity")
    for entry in sorted(generations, key=lambda item: item["createdAt"], reverse=True):
        if (
            entry["revision"] not in captures
            and entry["expiresAt"] > timestamp
            and len(selected) < MAX_GENERATIONS
            and sum(size(item) for item in selected) + size(entry) <= MAX_TOTAL
        ):
            selected.append(entry)
    commit(root, selected, captures, generations)
    return {
        "generations": len(selected),
        "files": sum(len(entry["assets"]) for entry in selected),
        "bytes": sum(size(entry) for entry in selected),
        "maximumAgeSeconds": MAX_AGE,
    }


def prune(root: Path, timestamp: int, *, owner: int = 0) -> None:
    """Expire old public files even when the owner makes no subsequent deployment."""
    generations = read_store(root, owner)
    commit(
        root, [entry for entry in generations if entry["expiresAt"] > timestamp], {}, generations
    )


def select_version(root: Path, revision: str, *, owner: int = 0) -> None:
    """Publish selected revision metadata at the same boundary as image selection."""
    require(REVISION.fullmatch(revision), "asset_selected_revision")
    _ = read_store(root, owner)
    public = root / "public"
    temporary = public / (".version-" + uuid.uuid4().hex)
    try:
        write_file(
            temporary, json.dumps({"revision": revision}, separators=(",", ":")).encode() + b"\n"
        )
        _ = temporary.replace(public / "app-version.json")
        sync_directory(public)
    finally:
        temporary.unlink(missing_ok=True)
