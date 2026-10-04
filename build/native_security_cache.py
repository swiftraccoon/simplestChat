#!/usr/bin/env python3
"""Reuse an exact prepared native image; every selected test mode still executes."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
import tempfile
from pathlib import Path

import native_security as native

# isort: split
import bounded_process
from release_json import JsonObject, decode_json, object_value, string_value

MAX_ARCHIVE = 8 * 1024**3


def cache_key(root: Path) -> str:
    """Bind actual image inputs and tool pins without unrelated workflow/document changes."""
    trusted = (
        os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
    )
    trust = "main" if trusted else "untrusted"
    identity = {
        "inputs": native.inputs_digest(root),
        "modes": {
            str(path.relative_to(root)): stat.S_IMODE(path.stat().st_mode)
            for path in native.source_files(root)
        },
        "architecture": native.native_architecture(),
        "trust": trust,
        "helper": hashlib.sha256(native.read_regular(Path(__file__), native.MIB)).hexdigest(),
    }
    digest = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    return f"native-prepared-v1-{trust}-{identity['architecture']}-{digest}"


def archive_identity(path: Path) -> tuple[int, str]:
    """Hash a bounded ordinary archive by streaming, without following a symlink."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        native.require(stat.S_ISREG(metadata.st_mode), "cache_archive_not_regular")
        native.require(0 < metadata.st_size <= MAX_ARCHIVE, "cache_archive_size")
        digest = hashlib.file_digest(source, "sha256").hexdigest()
        after = os.fstat(source.fileno())
        native.require(
            (after.st_size, after.st_mtime_ns) == (metadata.st_size, metadata.st_mtime_ns),
            "cache_archive_changed",
        )
    return metadata.st_size, digest


def checked_report(root: Path, report: JsonObject, key: str) -> str:
    """Reject stale preparation evidence before exporting or loading an engine image."""
    native.require(cache_key(root) == key, "cache_key_mismatch")
    native.require(
        set(report) == {"schemaVersion", "status", "inputsSha256", "architecture", "imageId"}
        and report.get("schemaVersion") == 1
        and report.get("status") == "passed"
        and report.get("inputsSha256") == native.inputs_digest(root)
        and report.get("architecture") == native.native_architecture(),
        "cache_preparation_mismatch",
    )
    image = string_value(report["imageId"])
    native.require(native.canonical_image_id(image) == image, "cache_image_id")
    return image


def save(engine: list[str], root: Path, preparation: Path, directory: Path, key: str) -> None:
    """Publish only a complete export of the checked prepared image, never a container snapshot."""
    native.require(not directory.exists() and not directory.is_symlink(), "cache_already_exists")
    report = object_value(decode_json(native.read_regular(preparation / "report.json", native.MIB)))
    image = checked_report(root, report, key)
    _ = native.checked_image(engine, image, string_value(report["inputsSha256"]))
    with tempfile.TemporaryDirectory(prefix="native-image-", dir=directory.parent) as temporary:
        stage = Path(temporary) / "cache"
        stage.mkdir(mode=0o700)
        archive = stage / "image.tar"
        _ = native.command([*engine, "image", "save", "--output", str(archive), image], timeout=600)
        archive.chmod(0o600)
        size, digest = archive_identity(archive)
        native.write_json(
            stage / "receipt.json",
            {"key": key, "preparation": report, "archiveBytes": size, "archiveSha256": digest},
        )
        _ = stage.rename(directory)


def load(engine: list[str], root: Path, directory: Path, output: Path, key: str) -> None:
    """Validate archive bytes first, then inspect the loaded immutable ID, input label and ISA."""
    receipt = object_value(decode_json(native.read_regular(directory / "receipt.json", native.MIB)))
    native.require(
        set(receipt) == {"key", "preparation", "archiveBytes", "archiveSha256"}
        and receipt["key"] == key,
        "cache_receipt_mismatch",
    )
    report = object_value(receipt["preparation"])
    image = checked_report(root, report, key)
    archive = directory / "image.tar"
    size, digest = archive_identity(archive)
    native.require(
        size == receipt["archiveBytes"] and digest == receipt["archiveSha256"],
        "cache_archive_mismatch",
    )
    output = native.fresh_output(output)
    _ = native.command([*engine, "image", "load", "--input", str(archive)], timeout=600)
    _ = native.checked_image(engine, image, string_value(report["inputsSha256"]))
    native.write_json(output / "report.json", report)


class Options(argparse.Namespace):
    """Fixed cache operations with explicit private directories."""

    operation: str = ""
    engine: str = "docker"
    directory: Path = Path()
    output: Path = Path()
    key: str = ""


def main() -> int:
    """Expose cache keys and checked image transport to the pinned cache workflow actions."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("operation", choices=("key", "save", "load"))
    _ = parser.add_argument("--engine", choices=("docker", "podman"), default="docker")
    _ = parser.add_argument("--directory", type=Path, default=Path())
    _ = parser.add_argument("--output", type=Path, default=Path())
    _ = parser.add_argument("--key", default="")
    args = parser.parse_args(namespace=Options())
    try:
        if args.operation == "key":
            _ = sys.stdout.write(cache_key(native.ROOT) + "\n")
        else:
            for path in (args.directory, args.output):
                native.require(path.is_absolute() and path.resolve() == path, "cache_path")
            engine = native.engine_prefix(args.engine)
            if args.operation == "save":
                save(engine, native.ROOT, args.output, args.directory, args.key)
            else:
                load(engine, native.ROOT, args.directory, args.output, args.key)
    except (OSError, ValueError, KeyError, native.SecurityError, bounded_process.ProcessError):
        _ = sys.stderr.write("Prepared native image cache validation failed\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
