"""Read only the exact marked disposable guest's release-fetch publication.

This fixed probe cannot choose a root, host, command or service. It does not
repair an uncertain outcome. Only closed hashes, inode identities and counts
leave the guest; private receiver paths and arbitrary diagnostics do not.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Sequence

PUBLIC = Path("/srv/simplestchat-public")
MARKER = Path("/etc/simplestchat-vm-fixture")
MAX_IMAGE = 2 * 1024**3
MAX_RECORD = 65536
MAX_MANIFEST = 1024**2
SAFE = 9007199254740991
PRIVATE_MODE = 0o600
DIRECTORY_MODE = 0o700


def require(value: object) -> None:
    """Refuse without disclosing a path, receiver record or exception text."""
    if not value:
        message = "fetch_vm_probe_unproven"
        raise ValueError(message)


def stamp(value: os.stat_result) -> tuple[int, ...]:
    """Retain ownership and mutation facts, excluding read-only atime changes."""
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_uid,
        value.st_gid,
        value.st_nlink,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def file_pin(path: Path, limit: int) -> tuple[dict[str, object], bytes]:
    """Hash an original nofollow file and explicitly close it on every refusal."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        first = os.fstat(source.fileno())
        require(
            stat.S_ISREG(first.st_mode)
            and first.st_uid == first.st_gid == 0
            and stat.S_IMODE(first.st_mode) == PRIVATE_MODE
            and 0 < first.st_size <= limit
            and stamp(path.lstat()) == stamp(first)
        )
        digest, total, record = hashlib.sha256(), 0, bytearray()
        while chunk := source.read(65536):
            total += len(chunk)
            require(total <= limit)
            digest.update(chunk)
            if limit <= MAX_RECORD:
                record.extend(chunk)
        require(
            total == first.st_size
            and stamp(os.fstat(source.fileno())) == stamp(first)
            and stamp(path.lstat()) == stamp(first)
        )
        result: dict[str, object] = {
            "sha256": digest.hexdigest(),
            "bytes": total,
            "device": first.st_dev,
            "inode": first.st_ino,
        }
    return result, bytes(record)


def object_json(data: bytes) -> dict[str, object]:
    """Decode one bounded finite, duplicate-free passive record."""
    require(0 < len(data) <= MAX_RECORD)

    def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for name, item in items:
            require(name not in value)
            value[name] = item
        return value

    def invalid(_: str) -> object:
        require(None)
        return None

    value = cast("object", json.loads(data, object_pairs_hook=pairs, parse_constant=invalid))
    require(isinstance(value, dict))
    return cast("dict[str, object]", value)


@dataclass
class Options(argparse.Namespace):
    """Only the run marker and selected immutable public identities are inputs."""

    run_id: str = ""
    revision: str = ""
    artifact_id: int = 0
    zip_sha256: str = ""
    zip_bytes: int = 0
    image_sha256: str = ""
    manifest_sha256: str = ""
    attempts: int = 0


def inspect(options: Options) -> dict[str, object]:
    """Read finalized attempt receipts and both current public files without modifying them."""
    require(
        os.geteuid() == 0
        and re.fullmatch(r"[a-f0-9]{32}", options.run_id)
        and re.fullmatch(r"[a-f0-9]{40}", options.revision)
        and 0 < options.artifact_id <= SAFE
        and 0 < options.zip_bytes <= MAX_IMAGE
        and options.attempts in (1, 2)
        and all(
            re.fullmatch(r"[a-f0-9]{64}", item)
            for item in (options.zip_sha256, options.image_sha256, options.manifest_sha256)
        )
    )
    _, marker = file_pin(MARKER, MAX_RECORD)
    require(marker == (options.run_id + "\n").encode())
    destination = PUBLIC / "releases" / options.revision
    require(
        destination.resolve() == destination
        and destination.is_dir()
        and destination.lstat().st_uid == 0
        and stat.S_IMODE(destination.lstat().st_mode) == DIRECTORY_MODE
    )
    published: dict[str, object] = {}
    for name, expected, limit in (
        ("image.tar", options.image_sha256, MAX_IMAGE),
        ("release.json", options.manifest_sha256, MAX_MANIFEST),
    ):
        pinned, _ = file_pin(destination / name, limit)
        require(pinned["sha256"] == expected)
        published[name] = pinned
    attempts = sorted(destination.glob("download.*"))
    require(len(attempts) == options.attempts)
    for attempt in attempts:
        metadata = attempt.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == metadata.st_gid == 0
            and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
        )
        _, data = file_pin(attempt / "fetch-outcome.json", MAX_RECORD)
        receipt = object_json(data)
        require(
            receipt.get("schemaVersion") == 1
            and type(receipt["schemaVersion"]) is int
            and receipt.get("passed") is True
            and receipt.get("settled") is True
            and receipt.get("finalized") is True
            and receipt.get("status") == "complete"
            and receipt.get("phase") == "complete"
            and receipt.get("failureClass") is None
            and receipt.get("revision") == options.revision
            and type(receipt.get("artifactId")) is int
            and receipt["artifactId"] == options.artifact_id
            and receipt.get("zipSha256") == options.zip_sha256
            and type(receipt.get("artifactZipBytes")) is int
            and receipt["artifactZipBytes"] == options.zip_bytes
            and type(receipt.get("downloadedBytes")) is int
            and receipt["downloadedBytes"] == options.zip_bytes
            and receipt.get("archiveSha256") == options.image_sha256
            and receipt.get("manifestSha256") == options.manifest_sha256
        )
    return {
        "schemaVersion": 1,
        "scope": "ReleaseFetchGuestProbe",
        "runId": options.run_id,
        "revision": options.revision,
        "attempts": len(attempts),
        "published": published,
        "passed": True,
    }


def main(arguments: Sequence[str] | None = None) -> int:
    """Return only a closed success object or a fixed negative message."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    for name in ("run-id", "revision", "zip-sha256", "image-sha256", "manifest-sha256"):
        _ = parser.add_argument("--" + name, required=True)
    for name in ("artifact-id", "zip-bytes", "attempts"):
        _ = parser.add_argument("--" + name, type=int, required=True)
    try:
        options = parser.parse_args(arguments, namespace=Options())
        value = inspect(options)
        data = json.dumps(value, separators=(",", ":")).encode() + b"\n"
        require(len(data) <= MAX_RECORD)
        _ = sys.stdout.buffer.write(data)
        _ = sys.stdout.buffer.flush()
    except (OSError, ValueError, KeyError, TypeError, RecursionError):
        _ = sys.stderr.write("Release fetch guest observation is unproven.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
