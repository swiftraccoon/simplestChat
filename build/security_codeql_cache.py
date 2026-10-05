"""Reuse exact analyzed databases; regenerate SARIF and enforce current policy every time."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import stat
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

import ci_verified
import security_codeql
import security_codeql_tools as tools
from security_context import ROOT, executable
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
import bounded_process
from release_json import JsonObject, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

    from security_context import Context

MAX_BUNDLE = 4 * 1024**3
MAX_MEMBERS = 100000
CONTROL_END = 32


class Options(argparse.Namespace):
    """Explicit language, suite, source and optional native library locations."""

    source_root: Path = Path()
    openssl_prefix: Path | None = None
    language: str = ""
    suite: str = "security"


def command(argv: Sequence[str]) -> bytes:
    """Read bounded installed package identity without logging its contents."""
    status, output, _ = bounded_process.run(
        argv,
        cwd=ROOT,
        limits=bounded_process.Limits(timeout=30, stdout=4 * 1024**2, stderr=65536),
    )
    require(status == 0, "codeql_cache_environment")
    return output


def openssl_identity(prefix: Path) -> str:
    """Bind the actual headers, static archives and installation settings used by the build."""
    require(prefix.is_absolute() and prefix.resolve() == prefix, "codeql_cache_openssl_path")
    hashed = hashlib.sha256()
    for directory in ("include", "lib", "share/simplestchat"):
        root = prefix / directory
        require(root.is_dir() and not root.is_symlink(), "codeql_cache_openssl_missing")
        for path in sorted(root.rglob("*")):
            require(not path.is_symlink(), "codeql_cache_openssl_symlink")
            if path.is_dir():
                continue
            hashed.update(str(path.relative_to(prefix)).encode() + b"\0")
            hashed.update(ci_verified.digest(path).encode() + b"\0")
    return hashed.hexdigest()


def cache_key(root: Path, source: Path, openssl: Path | None, language: str, suite: str) -> str:
    """Bind paths, trusted runner generation, source, tools and installed native dependencies."""
    require(source.is_absolute(), "codeql_cache_source_path")
    require(language in {"c-cpp", "rust"} and suite in {"security", "all"}, "codeql_cache_scope")
    identity: JsonObject = {
        "inputs": ci_verified.cache_key(
            root, "codeql-native" if language == "c-cpp" else "codeql-rust"
        ),
        "language": language,
        "suite": suite,
        "sourceRoot": str(source),
        "toolchain": tools.pin(),
    }
    if language == "c-cpp":
        require(openssl is not None, "codeql_cache_openssl_missing")
        if openssl is None:
            raise ToolError("codeql_cache_openssl_missing")  # noqa: EM101 -- Fixed safe code.
        packages = command([executable("dpkg-query"), "-W", "-f=${Package}\t${Version}\n"])
        compilers: JsonObject = {
            name: ci_verified.digest((Path("/usr/bin") / name).resolve()) for name in ("gcc", "g++")
        }
        identity.update(
            {
                "opensslRoot": str(openssl),
                "opensslSha256": openssl_identity(openssl),
                "packagesSha256": hashlib.sha256(packages).hexdigest(),
                "compilerSha256": compilers,
            }
        )
    digest = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    return "codeql-analyzed-v2-" + language + "-" + ci_verified.cache_namespace() + "-" + digest


def archive_members(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    """Validate bounded ordinary ZIP members before the CLI can unpack anything."""
    members = archive.infolist()
    require(0 < len(members) <= MAX_MEMBERS, "codeql_cache_archive_members")
    names: set[str] = set()
    total = 0
    for member in members:
        name = member.filename.rstrip("/")
        path = PurePosixPath(name)
        mode = member.external_attr >> 16
        require(
            bool(name)
            and str(path) == name
            and not path.is_absolute()
            and ".." not in path.parts
            and "\\" not in name
            and not any(ord(character) < CONTROL_END for character in name)
            and name not in names
            and not member.flag_bits & 1
            and (stat.S_IFMT(mode) in {0, stat.S_IFREG, stat.S_IFDIR}),
            "codeql_cache_archive_path",
        )
        names.add(name)
        total += member.file_size
        require(total <= MAX_BUNDLE, "codeql_cache_archive_size")
    return members


def source_integrity(database: Path, source: Path, manifest: JsonObject, language: str) -> None:
    """Verify stored source bytes and mapping, including every required protocol implementation."""
    tools.scalar(
        bounded_file(database / "codeql-database.yml", 65536).decode(),
        "sourceLocationPrefix",
        str(source),
    )
    source_archive = database / "src.zip"
    require(
        source_archive.is_file()
        and not source_archive.is_symlink()
        and source_archive.stat().st_size <= MAX_BUNDLE,
        "codeql_cache_source_archive",
    )
    prefix = str(source).lstrip("/") + "/"
    observed: set[str] = set()
    with zipfile.ZipFile(source_archive) as archive:
        for member in archive_members(archive):
            if member.is_dir() or not member.filename.startswith(prefix):
                continue
            name = member.filename.removeprefix(prefix)
            require(name not in ci_verified.CODEQL_POLICY_DATA, "codeql_cache_unkeyed_source")
            # Reject omitted frontend bytes even when absent from the tracked manifest.
            require(
                not name.startswith("web/")
                or (language == "rust" and ci_verified.rust_codeql_input(name)),
                "codeql_cache_unkeyed_source",
            )
            if name in manifest:
                require(
                    ci_verified.native_codeql_input(name)
                    if language == "c-cpp"
                    else ci_verified.rust_codeql_input(name),
                    "codeql_cache_unkeyed_source",
                )
                require(member.file_size <= 16 * 1024**2, "codeql_cache_source_size")
                actual = hashlib.sha256(archive.read(member)).hexdigest()
                require(actual == manifest[name], "codeql_cache_source_changed")
                observed.add(name)
    required = set(security_codeql.REQUIRED) if language == "c-cpp" else {"src/main.rs"}
    require(required.issubset(observed), "codeql_cache_source_coverage")


def bundle_integrity(bundle: Path, language: str) -> None:
    """Require genuine evaluated query results, rejecting unrelated paths or replayed SARIF."""
    results = 0
    with zipfile.ZipFile(bundle) as archive:
        for member in archive_members(archive):
            path = PurePosixPath(member.filename)
            require(
                path.parts[0] == language and not member.filename.endswith(".sarif"),
                "codeql_cache_bundle_content",
            )
            results += int("results" in path.parts and member.filename.endswith(".bqrs"))
    require(results > 0, "codeql_cache_query_results_missing")


def restore(
    context: Context, codeql: Path, directory: Path, key: str, database: Path
) -> JsonObject | None:
    """Restore one genuine matching bundle, refusing partial or altered cache entries."""
    require(directory.is_absolute() and not directory.is_symlink(), "codeql_cache_directory")
    if not directory.exists():
        return None
    require(
        {path.name for path in directory.iterdir()} == {"database.zip", "receipt.json"},
        "codeql_cache_inventory",
    )
    receipt = object_value(decode_json(bounded_file(directory / "receipt.json", 4096)))
    require(
        set(receipt) == {"schemaVersion", "key", "revision", "bundleSha256", "bundleBytes"}
        and receipt["schemaVersion"] == 1
        and receipt["key"] == key
        and re.fullmatch(r"[a-f0-9]{40}", string_value(receipt["revision"])),
        "codeql_cache_receipt",
    )
    size = receipt["bundleBytes"]
    require(type(size) is int and 0 < size <= MAX_BUNDLE, "codeql_cache_bundle_size")
    bundle = directory / "database.zip"
    require(
        tools.archive_digest(bundle, int(str(size))) == receipt["bundleSha256"],
        "codeql_cache_bundle_digest",
    )
    bundle_integrity(bundle, database.name)
    _ = context.run(
        "codeql-cache-unbundle",
        [
            str(codeql),
            "database",
            "unbundle",
            "--name=" + database.name,
            "--target=" + str(database.parent),
            str(bundle),
        ],
        timeout=180,
    )
    return {**receipt, "reused": True}


def save(  # noqa: PLR0913 -- Preserve distinct runtime, artifact and source identity arguments.
    context: Context, codeql: Path, directory: Path, key: str, revision: str, *, language: str
) -> JsonObject:
    """Publish complete evaluated queries; current policy verdicts are never cached."""
    require(not directory.exists() and not directory.is_symlink(), "codeql_cache_exists")
    directory.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".codeql-analyzed-", dir=directory.parent))
    try:
        bundle = staging / "database.zip"
        _ = context.run(
            "codeql-cache-bundle",
            [
                str(codeql),
                "database",
                "bundle",
                "--include-diagnostics",
                "--include-results",
                "--no-include-logs",
                *(["--cache-cleanup=clear"] if language == "c-cpp" else []),
                "--output=" + str(bundle),
                str(context.output / "databases" / language),
            ],
            timeout=180,
        )
        size = bundle.stat().st_size
        require(0 < size <= MAX_BUNDLE, "codeql_cache_bundle_size")
        bundle_integrity(bundle, language)
        receipt: JsonObject = {
            "schemaVersion": 1,
            "key": key,
            "revision": revision,
            "bundleSha256": tools.archive_digest(bundle, size),
            "bundleBytes": size,
        }
        write_private(staging / "receipt.json", (json.dumps(receipt) + "\n").encode(), 0o600)
        _ = staging.rename(directory)
        return {**receipt, "reused": False}
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def main() -> int:
    """Print only the exact restore key for the workflow's pinned cache action."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--source-root", type=Path, required=True)
    _ = parser.add_argument("--openssl-prefix", type=Path)
    _ = parser.add_argument("--language", choices=("c-cpp", "rust"), required=True)
    _ = parser.add_argument("--suite", choices=("security", "all"), default="security")
    args = parser.parse_args(namespace=Options())
    try:
        _ = sys.stdout.write(
            cache_key(ROOT, args.source_root, args.openssl_prefix, args.language, args.suite) + "\n"
        )
    except (ToolError, OSError, ValueError, KeyError):
        _ = sys.stderr.write("CodeQL analyzed database identity failed.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
