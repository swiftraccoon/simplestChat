"""Reuse Rust extractor Cargo build output without reusing analysis or policy verdicts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import stat
import sys
from pathlib import Path

import ci_verified
from security_codeql_cache import command
from security_context import ROOT, executable
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
import bounded_process
from release_json import decode_json, object_value

MAX_BYTES = 1024**3
MAX_ENTRIES = 100000
DEPENDENCY_MTIME = 946684800


def tool_identity(name: str) -> str:
    """Allow the pinned rustup toolchain to initialize before reading its exact identity."""
    status, output, _ = bounded_process.run(
        [executable(name), "--version", "--verbose"],
        cwd=ROOT,
        limits=bounded_process.Limits(timeout=180, stdout=65536, stderr=65536),
    )
    require(status == 0, "codeql_cargo_toolchain")
    return output.decode()


def dependency_input(name: str) -> bool:
    """Preserve dependency/build-script inputs while Cargo rechecks changed application code."""
    return (
        name.rsplit("/", 1)[-1] in ci_verified.CODEQL_RUST_MANIFESTS
        or name.startswith(("vendor/", ".cargo/"))
        or name == "build.rs"
        or name.endswith("/build.rs")
        or name in ci_verified.CODEQL_NATIVE_FILES
    )


def cache_key(root: Path, source: Path) -> str:
    """Separate platforms, toolchains, build dependencies, fixed paths and trust domains."""
    require(source.is_absolute() and source.resolve() == source, "codeql_cargo_source_path")
    identity = {
        "runner": ci_verified.runner_identity(),
        "system": platform.system(),
        "architecture": platform.machine(),
        "trust": "main"
        if os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
        else "untrusted",
        "sourceRoot": str(source),
        "rustc": tool_identity("rustc"),
        "cargo": tool_identity("cargo"),
    }
    hashed = hashlib.sha256(json.dumps(identity, sort_keys=True).encode())
    listing = command([executable("git"), "-C", str(root), "ls-files", "--stage", "-z"])
    count = 0
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        metadata, raw_name = entry.split(b"\t", 1)
        mode, _, stage = metadata.decode().split()
        require(stage == "0" and mode in {"100644", "100755"}, "codeql_cargo_tracked_type")
        name = raw_name.decode()
        if not dependency_input(name):
            continue
        path = root / name
        require(path.resolve() == path and path.is_file(), "codeql_cargo_source_missing")
        hashed.update(raw_name + b"\0" + mode.encode() + b"\0")
        hashed.update(ci_verified.digest(path).encode() + b"\0")
        count += 1
    require(count > 0, "codeql_cargo_empty_inputs")
    return "codeql-rust-cargo-v1-" + ci_verified.cache_namespace() + "-" + hashed.hexdigest()


def private_directory(path: Path) -> None:
    """Require an owned, private restored directory without path aliases."""
    require(path.is_absolute() and path.resolve() == path, "codeql_cargo_directory_path")
    metadata = path.lstat()
    require(
        stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == os.getuid()
        and not stat.S_IMODE(metadata.st_mode) & 0o077,
        "codeql_cargo_directory_permissions",
    )


def validate(source: Path, directory: Path, key: str) -> Path:
    """Check an existing receipt without manufacturing an empty successful cache."""
    expected = {"schemaVersion": 1, "key": key, "sourceRoot": str(source)}
    private_directory(directory)
    require(
        {path.name for path in directory.iterdir()} == {"receipt.json", "target"},
        "codeql_cargo_inventory",
    )
    receipt = object_value(
        decode_json(bounded_file(directory / "receipt.json", 4096, private=True))
    )
    require(receipt == expected, "codeql_cargo_receipt")
    target = directory / "target"
    private_directory(target)
    return target


def payload_bytes(target: Path) -> int:
    """Bound stored bytes, counting Cargo hardlinks once as the cache tar archive does."""
    total = 0
    files: set[tuple[int, int]] = set()
    for count, path in enumerate(target.rglob("*"), start=1):
        require(count <= MAX_ENTRIES, "codeql_cargo_entries")
        metadata = path.lstat()
        require(
            (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode))
            and metadata.st_uid == os.getuid()
            and not stat.S_IMODE(metadata.st_mode) & 0o022,
            "codeql_cargo_entry_type",
        )
        identity = (metadata.st_dev, metadata.st_ino)
        if stat.S_ISREG(metadata.st_mode) and identity not in files:
            total += metadata.st_size
            files.add(identity)
    return total


def stabilize_dependencies(source: Path) -> None:
    """Preserve Cargo fingerprints only for vendored bytes covered by the dependency key."""
    require(source.is_dir() and source.resolve() == source, "codeql_cargo_source_path")
    vendor = source / "vendor"
    paths = [vendor, *vendor.rglob("*")] if vendor.exists() else []
    constraints = source / "build/pip-constraints.txt"
    if constraints.exists():
        paths.append(constraints)
    for path in paths:
        require(not path.is_symlink(), "codeql_cargo_dependency_type")
        os.utime(path, (DEPENDENCY_MTIME, DEPENDENCY_MTIME), follow_symlinks=False)


def prepare(root: Path, source: Path, directory: Path) -> Path:
    """Reuse only trusted build output; extraction, queries and policy remain independent.

    The receipt identifies the compiler/dependency namespace, never a successful
    security result. Cargo still validates its fingerprints against current source.
    """
    require(directory.is_absolute() and directory.resolve() == directory, "codeql_cargo_path")
    key = cache_key(root, source)
    if not directory.exists():
        directory.mkdir(mode=0o700, parents=True)
        (directory / "target").mkdir(mode=0o700)
        expected = {"schemaVersion": 1, "key": key, "sourceRoot": str(source)}
        write_private(directory / "receipt.json", (json.dumps(expected) + "\n").encode(), 0o600)
    target = validate(source, directory, key)
    require(payload_bytes(target) <= MAX_BYTES, "codeql_cargo_size")
    stabilize_dependencies(source)
    return target


class Options(argparse.Namespace):
    """Select only the fixed source path used by the extractor."""

    source_root: Path = Path()
    publication_check: Path | None = None


def main() -> int:
    """Print the dependency-build key for the pinned GitHub cache actions."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--source-root", type=Path, required=True)
    _ = parser.add_argument("--publication-check", type=Path)
    args = parser.parse_args(namespace=Options())
    try:
        key = cache_key(ROOT, args.source_root)
        require(
            re.fullmatch(
                r"codeql-rust-cargo-v1-(main|untrusted)-[a-z0-9_]+-[a-z0-9_]+-[a-f0-9]{64}", key
            ),
            "codeql_cargo_key",
        )
        if args.publication_check is None:
            _ = sys.stdout.write(key + "\n")
        else:
            target = validate(args.source_root, args.publication_check, key)
            size = payload_bytes(target)
            _ = sys.stdout.write(
                f"cacheable={str(size <= MAX_BYTES).lower()}\n"
                + f"bytes={size}\nlimit-bytes={MAX_BYTES}\n"
            )
    except (ToolError, OSError, ValueError, KeyError):
        _ = sys.stderr.write("Rust extraction Cargo cache validation failed.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
