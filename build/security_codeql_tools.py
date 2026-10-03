"""Require the same pinned analyzer and bundled query suites locally and in CI."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

from security_context import ROOT, Context, executable
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
from release_json import JsonObject, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

PIN = ROOT / "security/codeql-toolchain.json"
LANGUAGES = ("actions", "javascript-typescript", "python", "rust", "c-cpp")
MAX_MEMBERS = 250000


def pin() -> JsonObject:
    """Read the maintained shared pin and reject a mismatched workflow action."""
    result = object_value(decode_json(bounded_file(PIN, 16384)))
    require(result["schemaVersion"] == 1, "codeql_toolchain_schema")
    require(set(object_value(result["languages"])) == set(LANGUAGES), "codeql_toolchain_languages")
    workflow = (ROOT / ".github/workflows/codeql.yml").read_text()
    revisions = set(re.findall(r"github/codeql-action/[a-z-]+@([a-f0-9]{40})", workflow))
    require(revisions == {string_value(result["actionRevision"])}, "codeql_action_pin_mismatch")
    return result


def language_pin(language: str) -> JsonObject:
    """Select one explicit extractor and query-pack version."""
    require(language in LANGUAGES, "codeql_language")
    return object_value(object_value(pin()["languages"])[language])


def scalar(text: str, name: str, expected: str) -> None:
    """Require exactly one simple generated YAML field without permissive parsing."""
    matches = re.findall(r"^\s*" + re.escape(name) + r":\s*([^\n]+)$", text, re.MULTILINE)
    require(matches == [expected], "codeql_metadata_" + name)


def suite(codeql: Path, language: str, category: str) -> Path:
    """Resolve only the pinned bundled suite; never install mutable query packs."""
    selected = language_pin(language)
    extractor = string_value(selected["extractor"])
    version = string_value(selected["queriesVersion"])
    pack = codeql.parent / "qlpacks/codeql" / (extractor + "-queries") / version
    metadata = bounded_file(pack / "qlpack.yml", 65536).decode()
    scalar(metadata, "name", "codeql/" + extractor + "-queries")
    scalar(metadata, "version", version)
    scalar(metadata, "cliVersion", string_value(pin()["cliVersion"]))
    require(category in {"security", "quality-advisory"}, "codeql_suite")
    queries = "security-extended" if category == "security" else "security-and-quality"
    result = pack / "codeql-suites" / (extractor + "-" + queries + ".qls")
    require(result.is_file(), "codeql_suite_missing")
    return result


def verify(context: Context, codeql: Path, languages: Sequence[str]) -> None:
    """Reject CLI/query drift before either hosted or local analysis."""
    configure_environment(context)
    require(codeql.is_absolute() and codeql.is_file(), "codeql_binary_required")
    _, output = context.run("codeql-version", [str(codeql), "version", "--format=json"])
    result = object_value(decode_json(output))
    require(result["version"] == pin()["cliVersion"], "codeql_cli_version")
    for language in languages:
        for category in ("security", "quality-advisory"):
            _ = suite(codeql, language, category)


def configure_environment(context: Context) -> None:
    """Keep analyzer configuration, caches and temporary files in this owned output."""
    for name in ("tmp", "config", "cache"):
        (context.output / name).mkdir(mode=0o700, exist_ok=True)
    context.env.update(
        {
            "JAVA_TOOL_OPTIONS": "-Duser.home="
            + str(context.output / "home")
            + " -Djava.io.tmpdir="
            + str(context.output / "tmp"),
            "TMPDIR": str(context.output / "tmp"),
            "XDG_CONFIG_HOME": str(context.output / "config"),
            "XDG_CACHE_HOME": str(context.output / "cache"),
        }
    )


def archive_digest(path: Path, expected_size: int) -> str:
    """Hash a bounded regular bundle archive without loading it into memory."""
    require(path.is_file() and not path.is_symlink(), "codeql_archive_file")
    require(path.stat().st_size == expected_size, "codeql_archive_size")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024**2):
            digest.update(chunk)
    return digest.hexdigest()


def install(context: Context) -> Path:
    """Acquire the authenticated platform bundle in a private repository cache."""
    target = "darwin" if sys.platform == "darwin" else sys.platform + "-" + platform.machine()
    bundles = object_value(pin()["bundles"])
    require(target in bundles, "codeql_platform_unsupported")
    asset = object_value(bundles[target])
    expected = string_value(asset["sha256"])
    size = asset["bytes"]
    require(type(size) is int and size > 0, "codeql_archive_bound")
    size = int(str(size))
    # Cargo caches and cleans target recursively; shared analyzer bytes live outside it.
    directory = ROOT / ".cache/codeql-tools" / expected
    receipt = directory / "receipt.json"
    if directory.exists():
        require(
            object_value(decode_json(bounded_file(receipt, 4096)))
            == {"archiveSha256": expected, "archiveBytes": size},
            "codeql_installation_receipt",
        )
        return directory / "codeql/codeql"
    archive = context.output / "codeql-bundle.tar.zst"
    _ = context.run(
        "codeql-download",
        [
            executable("curl"),
            "--fail",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "20",
            "--max-time",
            "600",
            "--max-filesize",
            str(size),
            "--output",
            str(archive),
            string_value(asset["url"]),
        ],
        timeout=610,
    )
    require(archive_digest(archive, size) == expected, "codeql_archive_digest")
    # Authenticate before inspecting or extracting the upstream archive. GNU tar
    # and macOS bsdtar both auto-detect the platform bundle's Zstandard stream.
    _, listing = context.run(
        "codeql-archive-list", [executable("tar"), "-tf", str(archive)], timeout=180
    )
    members = listing.decode().splitlines()
    require(0 < len(members) <= MAX_MEMBERS, "codeql_archive_members")
    for member in members:
        path = PurePosixPath(member)
        require(
            bool(path.parts)
            and not path.is_absolute()
            and path.parts[0] == "codeql"
            and ".." not in path.parts,
            "codeql_archive_path",
        )
    directory.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".codeql-install-", dir=directory.parent))
    try:
        _ = context.run(
            "codeql-extract",
            [executable("tar"), "-xf", str(archive), "-C", str(staging)],
            timeout=300,
        )
        write_private(
            staging / "receipt.json",
            (json.dumps({"archiveSha256": expected, "archiveBytes": size}) + "\n").encode(),
            0o600,
        )
        require(not directory.exists() and not directory.is_symlink(), "codeql_cache_appeared")
        _ = staging.rename(directory)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
    return directory / "codeql/codeql"


class Options(argparse.Namespace):
    """An explicit installed bundle and a fresh private output directory."""

    codeql: Path | None = None
    language: str = ""
    output: Path = Path()


def main() -> int:
    """Validate a hosted action's actual bundle using the same local contract."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--codeql", type=Path)
    _ = parser.add_argument("--language", choices=LANGUAGES)
    _ = parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)
    try:
        require(args.output.is_absolute(), "codeql_output_absolute")
        args.output.mkdir(mode=0o700)
        context = Context(ROOT, args.output)
        codeql = args.codeql or install(context)
        verify(context, codeql, [args.language] if args.language else LANGUAGES)
        _ = sys.stdout.write(str(codeql) + "\n")
    except (ToolError, OSError, ValueError, KeyError):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
