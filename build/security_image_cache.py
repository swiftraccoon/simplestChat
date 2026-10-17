"""Cache Grype database download bytes; freshness and scans always run again."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request, urlopen

from security_context import ROOT
from security_tools import bounded_file, private_directory, require

# isort: split
from release_json import JsonObject, decode_json, object_value

if TYPE_CHECKING:
    from http.client import HTTPResponse

# Match the scanner's four-GiB final database bound. Its separate online update
# workspace also accommodates the old database until its replacement is ready.
FILES = {"vulnerability.db": 4 * 1024**3 - 65536, "import.json": 65536}
LATEST = "https://grype.anchore.io/databases/v6/latest.json"
MAX_DESCRIPTOR = 65536


def pin() -> str:
    """Separate downloaded database formats when the pinned scanner changes."""
    lock = object_value(decode_json((ROOT / "build/security-tools.lock.json").read_bytes()))
    grype = object_value(object_value(lock["tools"])["grype"])
    return hashlib.sha256(json.dumps(grype, sort_keys=True).encode()).hexdigest()


def prefix() -> str:
    """Separate immutable database generations by scanner pin and trust boundary."""
    trust = (
        "main"
        if os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
        else "untrusted"
    )
    return f"grype-db-v2-{trust}-{pin()}-"


def generation(checksum: object) -> str:
    """Use the archive's content identity only as a cache hint, never a verdict."""
    require(
        isinstance(checksum, str) and re.fullmatch(r"sha256:[a-f0-9]{64}", checksum),
        "image_database_generation_checksum",
    )
    return prefix() + cast("str", checksum).removeprefix("sha256:")


def descriptor_key(value: JsonObject) -> str:
    """Accept the fixed v6 distribution descriptor without trusting its scan status."""
    schema = value.get("schemaVersion")
    require(
        value.get("status") == "active"
        and isinstance(schema, str)
        and re.fullmatch(r"v6\.\d+\.\d+", schema),
        "image_database_generation_descriptor",
    )
    return generation(value.get("checksum"))


def key() -> str:
    """Resolve the current archive generation, including updates within one UTC day."""
    request = Request(LATEST, headers={"User-Agent": "simplestchat-grype-database-cache"})
    with cast("HTTPResponse", urlopen(request, timeout=10)) as response:  # noqa: S310 -- Fixed public HTTPS endpoint.
        require(response.url == LATEST, "image_database_generation_redirect")
        content = response.read(MAX_DESCRIPTOR + 1)
    require(0 < len(content) <= MAX_DESCRIPTOR, "image_database_generation_size")
    return descriptor_key(object_value(decode_json(content)))


def key_from_status(status: JsonObject) -> str:
    """Publish under the actual online-validated archive after a possible rotation."""
    source = status.get("from")
    require(isinstance(source, str), "image_database_generation_source")
    location = urlsplit(cast("str", source))
    require(
        location.scheme == "https"
        and location.netloc == "grype.anchore.io"
        and location.path.startswith("/databases/v6/")
        and location.path.endswith(".tar.zst")
        and not location.fragment,
        "image_database_generation_source",
    )
    parameters = parse_qs(location.query, keep_blank_values=True, strict_parsing=True)
    require(
        set(parameters) == {"checksum"} and len(parameters["checksum"]) == 1,
        "image_database_generation_source",
    )
    return generation(parameters["checksum"][0])


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


@dataclass
class Options(argparse.Namespace):
    """Select a current descriptor, a restore prefix, or a checked update's identity."""

    prefix: bool = False
    status: Path | None = None


def main() -> int:
    """Print one bounded cache selector without changing database data."""
    parser = argparse.ArgumentParser(description=__doc__)
    selection = parser.add_mutually_exclusive_group()
    _ = selection.add_argument("--prefix", action="store_true")
    _ = selection.add_argument("--status", type=Path)
    arguments = parser.parse_args(namespace=Options())
    if arguments.prefix:
        selected = prefix()
    elif arguments.status is not None:
        selected = key_from_status(
            object_value(decode_json(bounded_file(arguments.status, MAX_DESCRIPTOR)))
        )
    else:
        selected = key()
    _ = sys.stdout.write(selected + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
