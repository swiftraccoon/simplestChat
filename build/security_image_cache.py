"""Cache Grype database download bytes; freshness and scans always run again."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import sys
import tempfile
from datetime import UTC, datetime
from pathlib import Path

from security_context import ROOT
from security_tools import private_directory, require

# isort: split
from release_json import JsonObject, decode_json, object_value

# Match the existing scanner's four-GiB database-output bound. The current
# schema-six database is already about three GiB before compression.
FILES = {"vulnerability.db": 4 * 1024**3 - 65536, "import.json": 65536}


def pin() -> str:
    """Separate downloaded database formats when the pinned scanner changes."""
    lock = object_value(decode_json((ROOT / "build/security-tools.lock.json").read_bytes()))
    grype = object_value(object_value(lock["tools"])["grype"])
    return hashlib.sha256(json.dumps(grype, sort_keys=True).encode()).hexdigest()


def key() -> str:
    """Refresh the immutable cache daily, with a separate trusted namespace."""
    trust = (
        "main"
        if os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
        else "untrusted"
    )
    return f"grype-db-v1-{trust}-{pin()}-{datetime.now(UTC):%Y-%m-%d}"


def copy_checked(source: Path, destination: Path, limit: int) -> JsonObject:
    """Copy only bounded regular data, hashing exactly the bytes copied."""
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as incoming, destination.open("xb") as outgoing:
        metadata = os.fstat(incoming.fileno())
        require(stat.S_ISREG(metadata.st_mode), "image_database_cache_file")
        require(0 < metadata.st_size <= limit, "image_database_cache_size")
        digest = hashlib.sha256()
        size = 0
        while chunk := incoming.read(1024 * 1024):
            size += len(chunk)
            require(size <= limit, "image_database_cache_size")
            digest.update(chunk)
            _ = outgoing.write(chunk)
        require(size == metadata.st_size, "image_database_cache_changed")
    destination.chmod(0o600)
    return {"bytes": size, "sha256": digest.hexdigest()}


def restore(directory: Path, output: Path, uid: int, gid: int) -> bool:
    """Seed an owned online updater; this never supplies a scan verdict."""
    require(directory.is_absolute(), "image_database_cache_path")
    if not directory.exists() and not directory.is_symlink():
        return False
    private_directory(directory)
    receipt_path = directory / "receipt.json"
    with tempfile.TemporaryDirectory(prefix="database-receipt-", dir=output) as temporary:
        receipt_copy = Path(temporary) / "receipt.json"
        _ = copy_checked(receipt_path, receipt_copy, 65536)
        receipt = object_value(decode_json(receipt_copy.read_bytes()))
    require(
        set(receipt) == {"schemaVersion", "scannerPin", "files"}
        and receipt["schemaVersion"] == 1
        and receipt["scannerPin"] == pin(),
        "image_database_cache_receipt",
    )
    identities = object_value(receipt["files"])
    require(set(identities) == set(FILES), "image_database_cache_inventory")
    cache = output / "cache"
    cache.mkdir(mode=0o700)
    schema = cache / "6"
    schema.mkdir(mode=0o700)
    for name, limit in FILES.items():
        target = schema / name
        require(
            copy_checked(directory / name, target, limit) == identities[name],
            "image_database_cache_digest",
        )
        if os.getuid() == 0:
            os.chown(target, uid, gid)
    if os.getuid() == 0:
        os.chown(cache, uid, gid)
        os.chown(schema, uid, gid)
    return True


def save(directory: Path, database: Path) -> None:
    """Publish only the freshly updated and validated database, not policy results."""
    require(directory.is_absolute(), "image_database_cache_path")
    directory.mkdir(mode=0o700, exist_ok=True)
    private_directory(directory)
    with tempfile.TemporaryDirectory(prefix="database-publish-", dir=directory) as temporary:
        stage = Path(temporary)
        identities: JsonObject = {}
        for name, limit in FILES.items():
            identities[name] = copy_checked(database / "cache/6" / name, stage / name, limit)
        receipt: JsonObject = {"schemaVersion": 1, "scannerPin": pin(), "files": identities}
        _ = (stage / "receipt.json").write_text(json.dumps(receipt, sort_keys=True) + "\n")
        (stage / "receipt.json").chmod(0o600)
        for name in (*FILES, "receipt.json"):
            _ = (stage / name).replace(directory / name)


if __name__ == "__main__":
    _ = sys.stdout.write(key() + "\n")
