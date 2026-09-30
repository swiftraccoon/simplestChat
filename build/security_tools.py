"""Install and verify checksum-pinned security tools without global executables.

The lock identifies regular executables and required native libraries inside each
authenticated archive. Archives are inspected in memory; reviewed destination
mappings are the only paths written, and archive paths are never extracted.
Installation publishes a complete private directory atomically. Reuse verifies
the lock, receipt and executable bytes before returning a tool path. An existing
directory with a different tool selection must be replaced explicitly by its
owner; installation never overlays an unrelated directory.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import lzma
import os
import platform
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, cast
from urllib.parse import urlsplit
from urllib.request import urlopen

if TYPE_CHECKING:
    from collections.abc import Sequence
    from http.client import HTTPResponse

ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = ROOT / "build/security-tools.lock.json"
FAST_TOOLS = (
    "actionlint",
    "zizmor",
    "gitleaks",
    "cargo-deny",
    "cargo-audit",
    "squawk",
    "semgrep-core",
)
MAX_LOCK = 1024 * 1024
MAX_ARCHIVE = 128 * 1024**2
MAX_EXECUTABLE = 256 * 1024**2
MAX_MEMBERS = 10000
MAX_UNPACKED = 512 * 1024**2
MAX_STRING = 2048
MAX_NAME = 512
MAX_TOOLS = 32
CONTROL_END = 32
CHUNK = 65536
DOWNLOAD_SECONDS = 180
NAME = re.compile(r"[a-z][a-z0-9-]{0,63}")
DIGEST = re.compile(r"[a-f0-9]{64}")


class ToolError(Exception):
    """A fixed failure code; never include scanner output or downloaded contents."""


def require(condition: object, code: str) -> None:
    """Reject an unmet toolchain invariant using its stable code."""
    if not condition:
        raise ToolError(code)


def record(value: object, keys: set[str]) -> dict[str, object]:
    """Accept an exact JSON object shape without implicit or unknown fields."""
    require(isinstance(value, dict), "invalid_tool_record")
    result = cast("dict[str, object]", value)
    require(set(result) == keys, "invalid_tool_record_fields")
    return result


def string(value: object) -> str:
    """Require a bounded nonempty string before using configuration data."""
    require(isinstance(value, str) and 0 < len(value) <= MAX_STRING, "invalid_tool_string")
    return cast("str", value)


def safe_name(value: str) -> bool:
    """Allow only canonical relative POSIX archive paths, never alternate spellings."""
    path = PurePosixPath(value)
    return (
        bool(value)
        and value != "."
        and len(value) <= MAX_NAME
        and str(path) == value
        and not path.is_absolute()
        and ".." not in path.parts
        and "\\" not in value
        and not any(ord(character) < CONTROL_END for character in value)
    )


@dataclass(frozen=True)
class Asset:
    """Reviewed transport identity and explicit member-to-destination mapping."""

    url: str
    sha256: str
    format: str
    files: dict[str, str]

    @classmethod
    def parse(cls, value: object) -> Asset:
        """Require HTTPS and a full SHA-256 before any network access."""
        raw = record(value, {"url", "sha256", "format", "files"})
        require(isinstance(raw["files"], dict), "invalid_tool_files")
        files = cast("dict[str, object]", raw["files"])
        require(0 < len(files) <= MAX_MEMBERS, "invalid_tool_files")
        mapping = {string(key): string(item) for key, item in files.items()}
        require(
            all(safe_name(key) and safe_name(item) for key, item in mapping.items()),
            "invalid_tool_member",
        )
        require(len(set(mapping.values())) == len(mapping), "duplicate_tool_destination")
        result = cls(string(raw["url"]), string(raw["sha256"]), string(raw["format"]), mapping)
        url = urlsplit(result.url)
        require(
            url.scheme == "https"
            and (
                (url.hostname == "github.com" and "/releases/download/" in url.path)
                or (
                    url.hostname == "files.pythonhosted.org"
                    and url.path.startswith("/packages/")
                    and url.path.endswith(".whl")
                )
            )
            and url.username is None
            and url.password is None
            and url.port is None
            and not url.query
            and not url.fragment,
            "invalid_tool_url",
        )
        require(DIGEST.fullmatch(result.sha256), "invalid_tool_digest")
        require(result.format in {"tar.gz", "tar.xz", "binary", "zip"}, "invalid_tool_format")
        require(result.format != "binary" or len(mapping) == 1, "invalid_tool_binary_files")
        return result


@dataclass(frozen=True)
class Tool:
    """A versioned executable with explicitly supported platform assets."""

    version: str
    executable: str
    platforms: dict[str, Asset]

    @classmethod
    def parse(cls, value: object) -> Tool:
        """Validate the complete platform map, including unselected platforms."""
        raw = record(value, {"version", "executable", "platforms"})
        executable, version = string(raw["executable"]), string(raw["version"])
        require(safe_name(executable), "invalid_tool_executable")
        require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "invalid_tool_version")
        require(isinstance(raw["platforms"], dict), "invalid_tool_platforms")
        platforms = cast("dict[str, object]", raw["platforms"])
        require(
            bool(platforms)
            and set(platforms)
            <= {"linux-x86_64", "linux-aarch64", "darwin-arm64", "darwin-x86_64"},
            "invalid_tool_platforms",
        )
        assets = {key: Asset.parse(item) for key, item in platforms.items()}
        require(
            all(executable in asset.files.values() for asset in assets.values()),
            "missing_tool_executable",
        )
        return cls(version, executable, assets)


def bounded_file(path: Path, limit: int, *, private: bool = False) -> bytes:
    """Read a bounded regular file without following its final path component."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        require(stat.S_ISREG(metadata.st_mode), "nonregular_tool_file")
        require(0 < metadata.st_size <= limit, "tool_file_size")
        if private:
            require(
                metadata.st_uid == os.getuid() and not stat.S_IMODE(metadata.st_mode) & 0o022,
                "unprotected_tool_file",
            )
        content = source.read(limit + 1)
        require(len(content) == metadata.st_size, "tool_file_changed")
        return content


def load_lock(path: Path = LOCK_PATH) -> tuple[dict[str, Tool], str]:
    """Parse the maintained tool lock and return its content identity."""
    content = bounded_file(path, MAX_LOCK)
    raw = record(cast("object", json.loads(content)), {"schemaVersion", "tools"})
    require(type(raw["schemaVersion"]) is int and raw["schemaVersion"] == 1, "tool_lock_schema")
    require(isinstance(raw["tools"], dict), "invalid_tool_set")
    tools = cast("dict[str, object]", raw["tools"])
    require(bool(tools) and len(tools) <= MAX_TOOLS, "invalid_tool_set")
    for name in tools:
        require(NAME.fullmatch(name), "invalid_tool_name")
    return {name: Tool.parse(value) for name, value in tools.items()}, hashlib.sha256(
        content
    ).hexdigest()


def current_platform() -> str:
    """Return an explicit supported platform; never substitute another architecture."""
    system, machine = platform.system().lower(), platform.machine().lower()
    if system == "linux" and machine == "arm64":
        machine = "aarch64"
    if system == "darwin" and machine == "aarch64":
        machine = "arm64"
    value = system + "-" + machine
    require(
        value in {"linux-x86_64", "linux-aarch64", "darwin-arm64", "darwin-x86_64"},
        "unsupported_tool_platform",
    )
    return value


def default_directory(lock_digest: str, target_platform: str | None = None) -> Path:
    """Keep different reviewed toolchain versions in separate ignored directories."""
    return ROOT / "target/security-tools" / lock_digest / (target_platform or current_platform())


def download(asset: Asset) -> bytes:
    """Bound transport bytes/time and authenticate the archive before parsing it."""
    deadline = time.monotonic() + DOWNLOAD_SECONDS
    chunks: list[bytes] = []
    size = 0
    # Only lock-validated release URLs are accepted; redirects must retain TLS.
    with cast("HTTPResponse", urlopen(asset.url, timeout=30)) as response:  # noqa: S310 -- HTTPS validated by Asset.parse.
        require(urlsplit(response.url).scheme == "https", "insecure_tool_redirect")
        while True:
            require(time.monotonic() < deadline, "tool_download_timeout")
            chunk = response.read(min(CHUNK, MAX_ARCHIVE + 1 - size))
            if not chunk:
                break
            size += len(chunk)
            require(size <= MAX_ARCHIVE, "tool_download_size")
            chunks.append(chunk)
    result = b"".join(chunks)
    require(hashlib.sha256(result).hexdigest() == asset.sha256, "tool_archive_digest")
    return result


def selected_zip_files(asset: Asset, archive: bytes) -> dict[str, bytes]:
    """Inspect wheel metadata and read only explicitly selected regular members."""
    found: dict[str, bytes] = {}
    names: set[str] = set()
    total = 0
    with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
        require(len(bundle.infolist()) <= MAX_MEMBERS, "tool_archive_entry_count")
        for entry in bundle.infolist():
            name = entry.filename.rstrip("/") if entry.is_dir() else entry.filename
            require(entry.orig_filename == entry.filename and safe_name(name), "tool_archive_path")
            require(name not in names, "tool_archive_duplicate")
            names.add(name)
            kind = stat.S_IFMT(entry.external_attr >> 16)
            require(
                kind in {0, stat.S_IFREG, stat.S_IFDIR}
                and (kind != stat.S_IFDIR or entry.is_dir()),
                "tool_archive_entry_type",
            )
            require(
                not entry.flag_bits & 1
                and entry.compress_type in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED},
                "tool_archive_compression",
            )
            require(0 <= entry.file_size <= MAX_EXECUTABLE, "tool_archive_entry_size")
            total += entry.file_size
            require(total <= MAX_UNPACKED, "tool_archive_expansion")
            if entry.filename in asset.files:
                require(not entry.is_dir() and entry.file_size > 0, "tool_executable_type")
                with bundle.open(entry) as source:
                    content = source.read(MAX_EXECUTABLE + 1)
                require(len(content) == entry.file_size, "tool_executable_truncated")
                found[asset.files[entry.filename]] = content
    require(set(found) == set(asset.files.values()), "tool_executable_missing")
    return found


def selected_files(asset: Asset, archive: bytes) -> dict[str, bytes]:
    """Inspect authenticated entries; reject ambiguous or linked executable members."""
    require(hashlib.sha256(archive).hexdigest() == asset.sha256, "tool_archive_digest")
    if asset.format == "binary":
        require(0 < len(archive) <= MAX_EXECUTABLE, "tool_executable_size")
        return {next(iter(asset.files.values())): archive}
    if asset.format == "zip":
        return selected_zip_files(asset, archive)
    if asset.format == "tar.gz":
        with gzip.GzipFile(fileobj=io.BytesIO(archive)) as compressed:
            unpacked = compressed.read(MAX_UNPACKED + 1)
    else:
        decoder = lzma.LZMADecompressor(memlimit=MAX_ARCHIVE)
        unpacked = decoder.decompress(archive, max_length=MAX_UNPACKED + 1)
        require(decoder.eof and not decoder.unused_data, "tool_archive_compression")
    require(len(unpacked) <= MAX_UNPACKED, "tool_archive_expansion")
    found: dict[str, bytes] = {}
    names: set[str] = set()
    with tarfile.open(fileobj=io.BytesIO(unpacked), mode="r|") as bundle:
        for entry in bundle:
            name = entry.name.rstrip("/") if entry.isdir() else entry.name
            require(len(names) < MAX_MEMBERS and safe_name(name), "tool_archive_path")
            require(name not in names, "tool_archive_duplicate")
            names.add(name)
            require(entry.isfile() or entry.isdir(), "tool_archive_entry_type")
            require(0 <= entry.size <= MAX_EXECUTABLE, "tool_archive_entry_size")
            if name in asset.files:
                require(entry.isfile() and entry.size > 0, "tool_executable_type")
                source = bundle.extractfile(entry)
                require(source is not None, "tool_executable_missing")
                if source is None:
                    message = "tool_executable_missing"
                    raise ToolError(message)
                with source:
                    content = source.read(MAX_EXECUTABLE + 1)
                require(len(content) == entry.size, "tool_executable_truncated")
                found[asset.files[name]] = content
    require(set(found) == set(asset.files.values()), "tool_executable_missing")
    return found


def private_directory(path: Path) -> None:
    """Reject symlinked or writable installed tool directories."""
    metadata = path.lstat()
    require(
        stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == os.getuid()
        and not stat.S_IMODE(metadata.st_mode) & 0o022,
        "unprotected_tool_directory",
    )


def tool_path(
    name: str,
    directory: Path | None = None,
    *,
    lock_path: Path = LOCK_PATH,
    target_platform: str | None = None,
) -> Path:
    """Verify installed receipt and executable bytes before a scanner is invoked."""
    tools, digest = load_lock(lock_path)
    require(name in tools, "unknown_tool")
    host = target_platform or current_platform()
    require(host in tools[name].platforms, "unsupported_tool_platform")
    root = directory if directory is not None else default_directory(digest, host)
    private_directory(root)
    private_directory(root / "bin")
    receipt = record(
        cast("object", json.loads(bounded_file(root / "receipt.json", MAX_LOCK, private=True))),
        {"schemaVersion", "lockSha256", "platform", "tools"},
    )
    require(receipt["schemaVersion"] == 1 and receipt["lockSha256"] == digest, "tool_receipt_lock")
    require(receipt["platform"] == host, "tool_receipt_platform")
    require(isinstance(receipt["tools"], dict), "tool_receipt_tools")
    installed = cast("dict[str, object]", receipt["tools"])
    require(name in installed, "tool_not_installed")
    item = record(installed[name], {"version", "archiveSha256", "files"})
    tool = tools[name]
    require(host in tool.platforms, "unsupported_tool_platform")
    asset = tool.platforms[host]
    require(
        item["version"] == tool.version and item["archiveSha256"] == asset.sha256,
        "tool_receipt_identity",
    )
    hashes = record(item["files"], set(asset.files.values()))
    for destination, expected in hashes.items():
        path = root / "bin" / destination
        for parent in path.relative_to(root / "bin").parents:
            private_directory(root / "bin" / parent)
        content = bounded_file(path, MAX_EXECUTABLE, private=True)
        require(hashlib.sha256(content).hexdigest() == expected, "tool_executable_digest")
    path = root / "bin" / tool.executable
    require(os.access(path, os.X_OK), "tool_not_executable")
    return path


def write_private(path: Path, content: bytes, mode: int) -> None:
    """Create an owned file exclusively and synchronize bytes before publication."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    with os.fdopen(descriptor, "wb") as destination:
        _ = destination.write(content)
        destination.flush()
        os.fsync(destination.fileno())


def install(
    names: Sequence[str],
    directory: Path | None = None,
    *,
    lock_path: Path = LOCK_PATH,
    target_platform: str | None = None,
) -> Path:
    """Atomically install a selected complete tool set or verify its existing copy."""
    tools, digest = load_lock(lock_path)
    selected = sorted(set(names) if names else FAST_TOOLS)
    require(bool(selected) and set(selected) <= set(tools), "unknown_tool")
    host = target_platform or current_platform()
    require(all(host in tools[name].platforms for name in selected), "unsupported_tool_platform")
    root = directory if directory is not None else default_directory(digest, host)
    if root.exists() or root.is_symlink():
        for name in selected:
            _ = tool_path(name, root, lock_path=lock_path, target_platform=host)
        return root
    root.parent.mkdir(parents=True, exist_ok=True)
    private_directory(root.parent)
    staging = Path(tempfile.mkdtemp(prefix=".security-tools-", dir=root.parent))
    try:
        (staging / "bin").mkdir(mode=0o700)
        installed: dict[str, dict[str, object]] = {}
        for name in selected:
            tool = tools[name]
            asset = tool.platforms[host]
            contents = selected_files(asset, download(asset))
            hashes: dict[str, str] = {}
            for destination, content in contents.items():
                path = staging / "bin" / destination
                path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                write_private(path, content, 0o700)
                hashes[destination] = hashlib.sha256(content).hexdigest()
            installed[name] = {
                "version": tool.version,
                "archiveSha256": asset.sha256,
                "files": hashes,
            }
        receipt = {"schemaVersion": 1, "lockSha256": digest, "platform": host, "tools": installed}
        write_private(
            staging / "receipt.json", (json.dumps(receipt, indent=2) + "\n").encode(), 0o600
        )
        require(not root.exists() and not root.is_symlink(), "tool_directory_appeared")
        _ = staging.rename(root)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
    return root


@dataclass
class Options(argparse.Namespace):
    """Small, explicit install/path CLI with no arbitrary executable arguments."""

    action: str = ""
    tools: list[str] | None = None
    directory: Path | None = None
    name: str | None = None
    platform: str | None = None


def main(argv: Sequence[str] | None = None) -> int:
    """Install from the committed lock or print a fully verified scanner path."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    installer = commands.add_parser("install", argument_default=argparse.SUPPRESS)
    _ = installer.add_argument("--tools", nargs="+")
    _ = installer.add_argument("--directory", type=Path)
    _ = installer.add_argument("--platform")
    resolver = commands.add_parser("path", argument_default=argparse.SUPPRESS)
    _ = resolver.add_argument("name")
    _ = resolver.add_argument("--directory", type=Path)
    _ = resolver.add_argument("--platform")
    args = parser.parse_args(argv, namespace=Options())
    try:
        path = (
            install(args.tools or [], args.directory, target_platform=args.platform)
            if args.action == "install"
            else tool_path(args.name or "", args.directory, target_platform=args.platform)
        )
    except (ToolError, OSError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
        code = str(error) if isinstance(error, ToolError) else type(error).__name__
        _ = sys.stderr.write(f"Security tool setup failed: {code}\n")
        return 1
    _ = sys.stdout.write(str(path) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
