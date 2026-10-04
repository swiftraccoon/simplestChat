"""Bind reusable CI successes and backend executables to their exact checked inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from typing import cast

from security_tools import ToolError, bounded_file, record, require, string

ROOT = Path(__file__).resolve().parents[1]
SCOPES = (
    "automation",
    "rust-lint",
    "rust-test",
    "backend",
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-native",
    "codeql-rust",
)
BINARIES = ("simplestChat", "load_test")
NATIVE_PREFIXES = ("vendor/", "security/", "build/", ".github/", "ops/ansible/files/")
CODEQL_NATIVE_PREFIXES = ("vendor/", "security/codeql/")
CODEQL_NATIVE_FILES = frozenset(
    {
        ".github/workflows/codeql.yml",
        "build/ci_verified.py",
        "build/codeql-native-build.sh",
        "build/install-openssl.sh",
        "build/pip-constraints.txt",
        "build/security_codeql.py",
        "build/security_codeql_cache.py",
        "build/security_codeql_local.py",
        "build/security_codeql_resources.py",
        "build/security_codeql_sources.py",
        "build/security_codeql_tools.py",
        "build/security_codeql_triage.py",
        "build/security_context.py",
        "build/security_findings.py",
        "build/security_policy.py",
        "build/security_tools.py",
        "build/security_vendor.py",
        "ops/ansible/files/bounded_process.py",
        "ops/ansible/files/release_json.py",
        "security/codeql-toolchain.json",
        "security/codeql-review-2026-09-30.json",
        "security/codeql-native-review-2026-09-30.json",
        "security/exceptions.json",
    }
)


def native_codeql_input(name: str) -> bool:
    """Bind the complete worker and its analysis helpers without unrelated CI tooling."""
    return name.startswith(CODEQL_NATIVE_PREFIXES) or name in CODEQL_NATIVE_FILES


class Options(argparse.Namespace):
    """Validated fixed operations and explicit paths from the workflow."""

    operation: str = ""
    scope: str = ""
    key: str = ""
    directory: Path = Path()


def digest(path: Path) -> str:
    """Hash only regular files, including empty tracked files, without loading executables."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        require(stat.S_ISREG(metadata.st_mode), "ci_cache_regular_file")
        require(metadata.st_size <= 1024**3, "ci_cache_file_size")
        return hashlib.file_digest(source, "sha256").hexdigest()


def runner_identity() -> str:
    """Keep immutable local images and hosted image generations in separate namespaces."""
    if os.environ.get("ACT") == "true":
        image = os.environ.get("LOCAL_CI_RUNNER_IMAGE", "")
        require(re.fullmatch(r"[a-z0-9./_-]+@sha256:[a-f0-9]{64}", image), "ci_cache_local_image")
        return "local:" + image
    # These mixed-case names are supplied by the GitHub-hosted runner image.
    image_os = os.environ.get("ImageOS", "")  # noqa: SIM112
    version = os.environ.get("ImageVersion", "")  # noqa: SIM112
    require(
        re.fullmatch(r"[a-zA-Z0-9._-]{1,100}", image_os)
        and re.fullmatch(r"[a-zA-Z0-9._-]{1,100}", version),
        "ci_cache_hosted_image",
    )
    return f"hosted:{image_os}:{version}"


def cache_key(root: Path, scope: str) -> str:
    """Bind each check's source, helpers, policy, workflow, tool and runner inputs."""
    require(scope in SCOPES, "ci_cache_scope")
    trusted = (
        os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
    )
    identity = {
        "scope": scope,
        "trust": "main" if trusted else "untrusted",
        "system": platform.system(),
        "architecture": platform.machine(),
        "runner": runner_identity(),
    }
    hashed = hashlib.sha256(json.dumps(identity, sort_keys=True).encode())
    tracked = subprocess.run(
        ["git", "ls-files", "--stage", "-z"],  # noqa: S607 -- Use the workflow's Git installation.
        cwd=root,
        check=True,
        capture_output=True,
        timeout=30,
    ).stdout
    count = 0
    for entry in tracked.split(b"\0"):
        if not entry:
            continue
        metadata, raw_name = entry.split(b"\t", 1)
        mode, _, stage = metadata.decode().split()
        name = raw_name.decode()
        require(stage == "0" and mode in {"100644", "100755"}, "ci_cache_tracked_type")
        # Sanitizers retain their full input roots. CodeQL's standalone worker
        # uses a smaller explicit closure; its complete local imports are tested.
        if scope.startswith("native-") and not name.startswith(NATIVE_PREFIXES):
            continue
        if scope == "codeql-native" and not native_codeql_input(name):
            continue
        # Rust tests embed shared JSON fixtures from web/tests. Include all web
        # JSON configuration and data for Rust scopes; JS helper tests stay fresh.
        # Automation inspects frontend helpers, so it retains every tracked file.
        rust_data = scope in {"rust-lint", "rust-test", "backend", "codeql-rust"} and name.endswith(
            ".json"
        )
        if scope != "automation" and name.startswith("web/") and not rust_data:
            continue
        path = root / name
        require(not path.is_symlink() and path.is_file(), "ci_cache_source_missing")
        hashed.update(raw_name + b"\0" + mode.encode() + b"\0" + digest(path).encode())
        count += 1
    require(count > 0, "ci_cache_empty_inputs")
    return f"ci-verified-v1-{scope}-{identity['trust']}-{hashed.hexdigest()}"


def save(root: Path, scope: str, key: str, directory: Path) -> None:
    """Save only after the workflow's original check completed successfully."""
    require(not scope.startswith("codeql-"), "ci_cache_codeql_requires_database")
    require(cache_key(root, scope) == key, "ci_cache_inputs_changed")
    revision = os.environ.get("GITHUB_SHA", "")
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "ci_cache_revision")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    artifacts: dict[str, str] = {}
    if scope == "backend":
        for name in BINARIES:
            source = root / "target/debug" / name
            artifacts[name] = digest(source)
            require(os.access(source, os.X_OK), "ci_cache_binary_not_executable")
            destination = directory / name
            require(
                not destination.exists() and not destination.is_symlink(),
                "ci_cache_existing_binary",
            )
            _ = shutil.copyfile(source, destination)
            destination.chmod(0o700)
    with (directory / "receipt.json").open("x", encoding="utf-8") as target:
        _ = target.write(
            json.dumps({"schema": 1, "key": key, "revision": revision, "artifacts": artifacts})
            + "\n"
        )
    (directory / "receipt.json").chmod(0o600)


def verify(root: Path, scope: str, key: str, directory: Path) -> str:
    """Validate an exact restored success and every executable before installing it."""
    require(not scope.startswith("codeql-"), "ci_cache_codeql_requires_database")
    require(cache_key(root, scope) == key, "ci_cache_inputs_changed")
    receipt = record(
        cast("object", json.loads(bounded_file(directory / "receipt.json", 65536))),
        {"schema", "key", "revision", "artifacts"},
    )
    require(receipt["schema"] == 1 and receipt["key"] == key, "ci_cache_receipt_identity")
    revision = string(receipt["revision"])
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "ci_cache_revision")
    names: set[str] = set(BINARIES) if scope == "backend" else set()
    artifacts = record(receipt["artifacts"], names)
    for name in names:
        require(digest(directory / name) == artifacts[name], "ci_cache_binary_changed")
    for name in names:
        destination = root / "target/debug" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        require(not destination.is_symlink(), "ci_cache_binary_destination")
        _ = shutil.copyfile(directory / name, destination)
        destination.chmod(0o700)
    return revision


def main() -> int:
    """Expose a fixed scope and explicit receipt directory to the pinned cache action."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("operation", choices=("key", "save", "verify"))
    _ = parser.add_argument("--scope", choices=SCOPES, required=True)
    _ = parser.add_argument("--key", default="")
    _ = parser.add_argument("--directory", type=Path, default=Path())
    args = parser.parse_args(namespace=Options())
    try:
        scope, operation = string(args.scope), string(args.operation)
        if operation == "key":
            _ = sys.stdout.write(cache_key(ROOT, scope) + "\n")
        else:
            directory = Path(str(args.directory))
            require(directory.is_absolute() and not directory.is_symlink(), "ci_cache_directory")
            key = string(args.key)
            if operation == "save":
                save(ROOT, scope, key, directory)
            else:
                revision = verify(ROOT, scope, key, directory)
                _ = sys.stdout.write(
                    f"Reused {scope} success for identical inputs; original revision: {revision}\n"
                )
    except (ToolError, OSError, ValueError, subprocess.SubprocessError) as error:
        _ = sys.stderr.write(f"CI cache rejected: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
