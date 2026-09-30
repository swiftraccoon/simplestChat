"""Authenticate upstream bytes and account for every maintained vendor deviation.

Archives are inspected in memory after SHA-256 verification, never extracted.
The manifest is reviewed input, not a generated allowance for the current tree.
Network reads run in a bounded child; offline verification has identical checks.
"""

# Validation errors are intentionally precise CLI diagnostics.
# ruff: noqa: EM101, EM102, TRY003
from __future__ import annotations

import argparse
import base64
import configparser
import difflib
import gzip
import hashlib
import io
import json
import os
import re
import stat
import sys
import tarfile
import tempfile
import urllib.parse
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, cast, override

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence
    from http.client import HTTPMessage, HTTPResponse
    from typing import IO

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import bounded_process
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    object_value,
    string_value,
)

MAX_DOWNLOAD = 128 * 1024 * 1024
MAX_MEMBER = 32 * 1024 * 1024
MAX_CONTENT = 256 * 1024 * 1024
MAX_FILES = 20000
MAX_MANIFEST = 2 * 1024 * 1024
MAX_DIFF = 64 * 1024 * 1024
MAX_DECLARATIONS = 64
MAX_PATH = 1024
FIRST_PRINTABLE = 32
LAST_PRINTABLE = 126
HTTP_OK = 200
CHUNK = 1024 * 1024
SOURCE_FORMATS = frozenset({"tar.gz", "zip", "file"})


class IntegrityError(ValueError):
    """Reviewed provenance and the supplied source bytes do not agree."""


@dataclass(frozen=True)
class Source:
    """One exact downloadable input, shared with native component evidence."""

    identifier: str
    url: str
    sha256: str
    format: str


@dataclass(frozen=True)
class Change:
    """A precise changed, added or deleted member; null means absence."""

    path: str
    upstream_sha256: str | None
    vendored_sha256: str | None


@dataclass(frozen=True)
class Tree:
    """An exhaustive upstream archive subtree and its reviewed deviations."""

    path: str
    source: str
    prefix: str
    changes: tuple[Change, ...]


@dataclass(frozen=True)
class Wrap:
    """A Meson source pin and its optional authenticated or local overlay."""

    path: str
    source: str
    patch_source: str | None
    patch_directory: str | None
    fallback_urls: tuple[str, ...]


@dataclass(frozen=True)
class Manifest:
    """Strictly parsed current integrity contract."""

    sources: Mapping[str, Source]
    trees: tuple[Tree, ...]
    files: Mapping[str, str]
    maintained_files: tuple[str, ...]
    wraps: tuple[Wrap, ...]


def sha256(data: bytes) -> str:
    """Identify exact source or evidence bytes."""
    return hashlib.sha256(data).hexdigest()


def relative_path(value: str) -> str:
    """Require a canonical, printable relative POSIX path without ambiguous parts."""
    path = PurePosixPath(value)
    if (
        not value
        or len(value) > MAX_PATH
        or path.is_absolute()
        or str(path) != value
        or any(part in {".", ".."} for part in value.split("/"))
        or any(
            ord(char) < FIRST_PRINTABLE or ord(char) > LAST_PRINTABLE or char in "\\:"
            for char in value
        )
    ):
        raise IntegrityError(f"Unsafe or noncanonical path: {value!r}")
    return value


def digest(value: JsonValue) -> str:
    """Require a complete lowercase SHA-256, never a version-only pin."""
    result = string_value(value)
    if re.fullmatch(r"[a-f0-9]{64}", result) is None:
        raise IntegrityError("Invalid SHA-256")
    return result


def https_url(value: str) -> str:
    """Disallow credentials, fragments and protocol downgrades in source URLs."""
    parsed = urllib.parse.urlsplit(value)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
        or parsed.port not in {None, 443}
        or any(ord(char) <= FIRST_PRINTABLE or ord(char) > LAST_PRINTABLE for char in value)
    ):
        raise IntegrityError("Source URLs must be credential-free HTTPS URLs")
    return value


def fields(value: JsonValue, expected: set[str]) -> JsonObject:
    """Reject missing and unknown fields instead of silently weakening a contract."""
    result = object_value(value)
    if set(result) != expected:
        raise IntegrityError(f"Unexpected manifest fields: {sorted(set(result) ^ expected)}")
    return result


def text_field(value: JsonObject, key: str) -> str:
    """Read one required string field."""
    return string_value(value[key])


def parse_source(value: JsonValue) -> Source:
    """Parse a reusable immutable upstream source record."""
    obj = fields(value, {"id", "url", "sha256", "format"})
    identifier = text_field(obj, "id")
    if re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,127}", identifier) is None:
        raise IntegrityError("Invalid source identifier")
    source = Source(
        identifier,
        https_url(text_field(obj, "url")),
        digest(obj["sha256"]),
        text_field(obj, "format"),
    )
    if source.format not in SOURCE_FORMATS:
        raise IntegrityError("Unsupported source format")
    return source


def parse_change(value: JsonValue) -> Change:
    """Require both old and new identities, with exactly represented absence."""
    obj = fields(value, {"path", "upstream_sha256", "vendored_sha256"})
    old = None if obj["upstream_sha256"] is None else digest(obj["upstream_sha256"])
    new = None if obj["vendored_sha256"] is None else digest(obj["vendored_sha256"])
    if old == new:
        raise IntegrityError("Stale allowance: identical upstream and vendored identities")
    return Change(relative_path(text_field(obj, "path")), old, new)


def unique(values: Sequence[str], label: str) -> None:
    """Refuse duplicate declarations, including duplicate change paths."""
    if len(values) != len(set(values)):
        raise IntegrityError(f"Duplicate {label}")


def parse_tree(value: JsonValue) -> Tree:
    """Parse a full archive comparison, never a wildcard exclusion list."""
    obj = fields(value, {"path", "source", "prefix", "changes"})
    changes = tuple(parse_change(change) for change in array_value(obj["changes"]))
    unique([change.path for change in changes], "change path")
    return Tree(
        relative_path(text_field(obj, "path")),
        text_field(obj, "source"),
        relative_path(text_field(obj, "prefix")),
        changes,
    )


def parse_wrap(value: JsonValue) -> Wrap:
    """Parse native source identities without permitting VCS or unhashed fetches."""
    obj = fields(value, {"path", "source", "patch_source", "patch_directory", "fallback_urls"})
    return Wrap(
        relative_path(text_field(obj, "path")),
        text_field(obj, "source"),
        None if obj["patch_source"] is None else text_field(obj, "patch_source"),
        None
        if obj["patch_directory"] is None
        else relative_path(text_field(obj, "patch_directory")),
        tuple(https_url(string_value(url)) for url in array_value(obj["fallback_urls"])),
    )


def parse_manifest(data: bytes) -> Manifest:
    """Validate identities, exact schema, references and vendor coverage declarations."""
    obj = fields(decode_json(data), {"sources", "trees", "files", "maintained_files", "wraps"})
    sources = [parse_source(item) for item in array_value(obj["sources"])]
    unique([source.identifier for source in sources], "source identifier")
    trees = tuple(parse_tree(item) for item in array_value(obj["trees"]))
    wraps = tuple(parse_wrap(item) for item in array_value(obj["wraps"]))
    files: dict[str, str] = {}
    for item in array_value(obj["files"]):
        record = fields(item, {"path", "source"})
        path = relative_path(text_field(record, "path"))
        if path in files:
            raise IntegrityError("Duplicate copied file")
        files[path] = text_field(record, "source")
    maintained = tuple(
        relative_path(string_value(item)) for item in array_value(obj["maintained_files"])
    )
    unique([tree.path for tree in trees], "tree path")
    unique([wrap.path for wrap in wraps], "wrap path")
    unique(maintained, "maintained path")
    manifest = Manifest(
        {source.identifier: source for source in sources}, trees, files, maintained, wraps
    )
    validate_references(manifest)
    return manifest


def validate_references(manifest: Manifest) -> None:
    """Require every source and checked-in vendor path to have an accountable role."""
    if any(
        len(items) > MAX_DECLARATIONS
        for items in (
            manifest.sources,
            manifest.trees,
            manifest.files,
            manifest.wraps,
            manifest.maintained_files,
        )
    ):
        raise IntegrityError("Manifest declaration budget exceeded")
    references = {tree.source for tree in manifest.trees} | set(manifest.files.values())
    references |= {wrap.source for wrap in manifest.wraps}
    references |= {wrap.patch_source for wrap in manifest.wraps if wrap.patch_source is not None}
    if references != set(manifest.sources):
        raise IntegrityError("Missing or stale source records")
    paths = (
        [tree.path for tree in manifest.trees]
        + list(manifest.files)
        + list(manifest.maintained_files)
    )
    if any(not path.startswith("vendor/") for path in paths):
        raise IntegrityError("Vendor records must stay inside vendor/")
    if any(manifest.sources[tree.source].format == "file" for tree in manifest.trees):
        raise IntegrityError("A tree requires an archive source")
    if any(manifest.sources[source].format != "file" for source in manifest.files.values()):
        raise IntegrityError("A copied file requires a raw file source")
    native_sources = {wrap.source for wrap in manifest.wraps} | {
        wrap.patch_source for wrap in manifest.wraps if wrap.patch_source is not None
    }
    if any(manifest.sources[source].format == "file" for source in native_sources):
        raise IntegrityError("Native wraps require archive sources")
    if any(
        not path.endswith("/README.md") and path != "vendor/native-components.json"
        for path in manifest.maintained_files
    ):
        raise IntegrityError(
            "Only declared README/native-component metadata may be repository-authored"
        )


def read_regular(path: Path, limit: int) -> bytes:
    """Bound regular-file reads and reject symlinks, special files and concurrent mutation."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise IntegrityError(f"Not a bounded regular file: {path}")
        data = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        if len(data) > limit or (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise IntegrityError(f"File changed or exceeded its limit: {path}")
        return data


def tree_files(root: Path) -> dict[str, bytes]:
    """Read a complete local tree without following any directory or file links."""
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise IntegrityError(f"Not a source directory: {root}")
    result: dict[str, bytes] = {}
    total = 0
    entries = 0
    for parent, directories, filenames in os.walk(root, followlinks=False):
        entries += len(directories) + len(filenames)
        if entries > MAX_FILES:
            raise IntegrityError("Too many vendor filesystem entries")
        for name in directories:
            if not stat.S_ISDIR((Path(parent) / name).lstat().st_mode):
                raise IntegrityError("Symlink or special vendor directory")
        for name in sorted(filenames):
            path = Path(parent) / name
            key = relative_path(path.relative_to(root).as_posix())
            data = read_regular(path, MAX_MEMBER)
            total += len(data)
            if total > MAX_CONTENT:
                raise IntegrityError("Vendor content budget exceeded")
            result[key] = data
    return result


@dataclass
class ArchiveBudget:
    """Track metadata and actual retained content before allocating member bodies."""

    seen: set[str]
    files: dict[str, bytes]
    size: int = 0

    def reserve(self, name: str, size: int, *, directory: bool) -> str:
        """Reject duplicates, unsafe names, parent collisions and excessive expansion."""
        key = relative_path(name.removesuffix("/") if directory else name)
        if key in self.seen or len(self.seen) >= MAX_FILES:
            raise IntegrityError("Duplicate or excessive archive members")
        if any(str(parent) in self.files for parent in PurePosixPath(key).parents):
            raise IntegrityError("Archive file/directory collision")
        if not directory and any(existing.startswith(key + "/") for existing in self.seen):
            raise IntegrityError("Archive file/directory collision")
        self.seen.add(key)
        self.size += size
        if size < 0 or size > MAX_MEMBER or self.size > MAX_CONTENT:
            raise IntegrityError("Archive expansion budget exceeded")
        return key


def tar_members(data: bytes) -> dict[str, bytes]:
    """Inspect authenticated gzip tar members without applying filesystem operations."""
    budget = ArchiveBudget(set(), {})
    # Bound the entire decompressed stream before tarfile interprets long-name or
    # PAX metadata, which may otherwise allocate before yielding a member.
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
        expanded = compressed.read(MAX_CONTENT + 1)
    if len(expanded) > MAX_CONTENT:
        raise IntegrityError("Archive expansion budget exceeded")
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r|") as archive:
        for member in archive:
            if not (member.isfile() or member.isdir()) or member.sparse is not None:
                raise IntegrityError("Unsupported tar member type")
            key = budget.reserve(member.name, member.size, directory=member.isdir())
            if member.isdir():
                if member.size != 0:
                    raise IntegrityError("Nonempty archive directory")
                continue
            stream = archive.extractfile(member)
            if stream is None:
                raise IntegrityError("Unreadable tar member")
            with stream:
                body = stream.read(MAX_MEMBER + 1)
            if len(body) != member.size:
                raise IntegrityError("Tar member size differs")
            budget.files[key] = body
    return budget.files


def zip_members(data: bytes) -> dict[str, bytes]:
    """Inspect authenticated zip content, refusing links, encryption and special entries."""
    budget = ArchiveBudget(set(), {})
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for member in archive.infolist():
            if member.orig_filename != member.filename:
                raise IntegrityError("Ambiguous zip member name")
            mode = member.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if member.flag_bits & 1 or kind not in {0, stat.S_IFREG, stat.S_IFDIR}:
                raise IntegrityError("Unsupported zip member type")
            if member.is_dir() != (kind == stat.S_IFDIR) and kind != 0:
                raise IntegrityError("Ambiguous zip directory")
            key = budget.reserve(member.filename, member.file_size, directory=member.is_dir())
            if member.is_dir():
                if member.file_size != 0:
                    raise IntegrityError("Nonempty archive directory")
                continue
            with archive.open(member) as stream:
                body = stream.read(MAX_MEMBER + 1)
            if len(body) != member.file_size:
                raise IntegrityError("Zip member size differs")
            budget.files[key] = body
    return budget.files


def archive_files(source: Source, data: bytes) -> dict[str, bytes]:
    """Authenticate bytes before invoking an archive parser."""
    if len(data) > MAX_DOWNLOAD or sha256(data) != source.sha256:
        raise IntegrityError(f"Source SHA-256 or size differs: {source.identifier}")
    if source.format == "tar.gz":
        return tar_members(data)
    if source.format == "zip":
        return zip_members(data)
    raise IntegrityError("A raw file cannot be parsed as an archive")


class HTTPSRedirects(urllib.request.HTTPRedirectHandler):
    """Validate every redirect target before any follow-up request."""

    @override
    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: IO[bytes],
        code: int,
        msg: str,
        headers: HTTPMessage,
        newurl: str,
    ) -> urllib.request.Request | None:
        """Keep redirected crate and release downloads on HTTPS."""
        _ = https_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(url: str, destination: Path) -> None:
    """Fetch bounded bytes; the parent process separately imposes a wall-clock deadline."""
    opener = urllib.request.build_opener(HTTPSRedirects())
    request = urllib.request.Request(  # noqa: S310 -- HTTPS validated here and at every redirect.
        https_url(url), headers={"User-Agent": "simplestchat-vendor-integrity"}
    )
    with cast("HTTPResponse", opener.open(request, timeout=15)) as response:
        if response.status != HTTP_OK:
            raise IntegrityError("Source download did not return HTTP 200")
        _ = https_url(response.url)
        total = 0
        descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            while chunk := response.read(CHUNK):
                total += len(chunk)
                if total > MAX_DOWNLOAD:
                    raise IntegrityError("Source download exceeded its byte budget")
                _ = stream.write(chunk)


def private_directory(path: Path, *, create: bool) -> Path:
    """Require a caller-owned private directory and reject a final symlink."""
    if create:
        path.mkdir(mode=0o700)
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_mode & 0o077
    ):
        raise IntegrityError(f"Directory must be owned and private (0700): {path}")
    return path.resolve()


def source_bytes(source: Source, cache: Path, *, offline: bool) -> bytes:
    """Use only authenticated cache bytes or a bounded, freshly authenticated download."""
    path = cache / source.sha256
    if not path.exists() and not path.is_symlink():
        if offline:
            raise IntegrityError(f"Offline source missing: {source.identifier}")
        with tempfile.TemporaryDirectory(prefix="download-", dir=cache) as temporary:
            fetched = Path(temporary) / "source"
            status, _, error = bounded_process.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "_download",
                    "--url",
                    source.url,
                    "--destination",
                    str(fetched),
                ],
                limits=bounded_process.Limits(timeout=120, stdout=4096, stderr=4096),
            )
            if status != 0:
                diagnostic = error.decode("utf-8", errors="replace")
                raise IntegrityError(f"Source download failed: {source.identifier}: {diagnostic}")
            data = read_regular(fetched, MAX_DOWNLOAD)
            if sha256(data) != source.sha256:
                raise IntegrityError(f"Source SHA-256 differs: {source.identifier}")
            # Hard-link publication cannot overwrite an existing cache identity.
            os.link(fetched, path)
    data = read_regular(path, MAX_DOWNLOAD)
    if sha256(data) != source.sha256:
        raise IntegrityError(f"Source SHA-256 differs: {source.identifier}")
    return data


def render_diff(path: str, old: bytes | None, new: bytes | None) -> bytes:
    """Retain the complete text diff, or complete base64 bytes for a binary deviation."""
    before, after = old or b"", new or b""
    header = f"diff --git a/{path} b/{path}\n"
    if old is None:
        header += "new file\n"
    if new is None:
        header += "deleted file\n"
    first = "a/" + path if old is not None else "/dev/null"
    second = "b/" + path if new is not None else "/dev/null"
    try:
        old_text, new_text = before.decode("utf-8"), after.decode("utf-8")
        if "\x00" in old_text or "\x00" in new_text:
            raise UnicodeError  # noqa: TRY301 -- Route binary text through the same complete encoding.
    except UnicodeError:
        return (
            (
                header + f"--- {first}\n+++ {second}\nBinary old sha256={sha256(before)} base64="
            ).encode()
            + base64.b64encode(before)
            + f"\nBinary new sha256={sha256(after)} base64=".encode()
            + base64.b64encode(after)
            + b"\n"
        )
    lines = difflib.unified_diff(
        old_text.splitlines(keepends=True),
        new_text.splitlines(keepends=True),
        fromfile=first,
        tofile=second,
    )
    return (
        header
        + "".join(
            line if line.endswith("\n") else line + "\n\\ No newline at end of file\n"
            for line in lines
        )
    ).encode()


def compare_tree(
    tree: Tree, upstream: Mapping[str, bytes], local: Mapping[str, bytes]
) -> tuple[JsonObject, bytes, list[str]]:
    """Compare every byte and reject both undeclared deviations and stale allowances."""
    prefix = tree.prefix + "/"
    if any(not path.startswith(prefix) for path in upstream):
        raise IntegrityError(f"Archive contains files outside its declared prefix: {tree.source}")
    original = {path.removeprefix(prefix): body for path, body in upstream.items()}
    if not original:
        raise IntegrityError(f"Empty upstream tree: {tree.source}")
    declared = {change.path: change for change in tree.changes}
    actual: dict[str, Change] = {}
    records: list[JsonValue] = []
    diffs: list[bytes] = []
    diff_size = 0
    for path in sorted(original.keys() | local.keys()):
        old, new = original.get(path), local.get(path)
        change = Change(
            path, None if old is None else sha256(old), None if new is None else sha256(new)
        )
        records.append(
            {
                "path": path,
                "upstream_sha256": change.upstream_sha256,
                "vendored_sha256": change.vendored_sha256,
            }
        )
        if old != new:
            actual[path] = change
            rendered = render_diff(tree.path + "/" + path, old, new)
            diff_size += len(rendered)
            if diff_size > MAX_DIFF:
                raise IntegrityError("Vendor diff budget exceeded")
            diffs.append(rendered)
    errors = [
        f"Unlisted or stale deviation: {tree.path}/{path}"
        for path in sorted(actual.keys() | declared.keys())
        if actual.get(path) != declared.get(path)
    ]
    return {"path": tree.path, "source": tree.source, "files": records}, b"".join(diffs), errors


def validate_wrap(wrap: Wrap, manifest: Manifest, local: Mapping[str, bytes]) -> None:
    """Compare active Meson URLs/hashes and reject new fetch mechanisms or VCS wraps."""
    parser = configparser.ConfigParser(interpolation=None, strict=True)
    parser.read_string(local[wrap.path].decode("utf-8"))
    if (
        parser.defaults()
        or "wrap-file" not in parser
        or set(parser.sections()) - {"wrap-file", "provide"}
    ):
        raise IntegrityError(f"Only pinned wrap-file dependencies are allowed: {wrap.path}")
    section = parser["wrap-file"]
    allowed = {
        "directory",
        "source_url",
        "source_filename",
        "source_hash",
        "patch_url",
        "patch_filename",
        "patch_hash",
        "patch_directory",
        "source_fallback_url",
        "wrapdb_version",
    }
    if set(section) - allowed:
        raise IntegrityError(f"Unsupported native wrap fields: {wrap.path}")
    source = manifest.sources[wrap.source]
    if section.get("source_url") != source.url or section.get("source_hash") != source.sha256:
        raise IntegrityError(f"Native source pin differs: {wrap.path}")
    _ = relative_path(section.get("directory", ""))
    _ = relative_path(section.get("source_filename", ""))
    fallbacks = (section["source_fallback_url"],) if "source_fallback_url" in section else ()
    if fallbacks != wrap.fallback_urls:
        raise IntegrityError(f"Native fallback pin differs: {wrap.path}")
    validate_wrap_patch(wrap, manifest, section, local)


def validate_wrap_patch(
    wrap: Wrap, manifest: Manifest, section: configparser.SectionProxy, local: Mapping[str, bytes]
) -> None:
    """Require complete remote patch hashes or a declared, populated local overlay."""
    if wrap.patch_source is not None:
        source = manifest.sources[wrap.patch_source]
        if (
            wrap.patch_directory is not None
            or section.get("patch_url") != source.url
            or section.get("patch_hash") != source.sha256
        ):
            raise IntegrityError(f"Native patch pin differs: {wrap.path}")
        _ = relative_path(section.get("patch_filename", ""))
    elif any(key in section for key in ("patch_url", "patch_hash", "patch_filename")):
        raise IntegrityError(f"Unlisted remote native patch: {wrap.path}")
    if section.get("patch_directory") != wrap.patch_directory:
        raise IntegrityError(f"Native patch directory differs: {wrap.path}")
    if wrap.patch_directory is not None:
        prefix = str(PurePosixPath(wrap.path).parent / "packagefiles" / wrap.patch_directory) + "/"
        if not any(path.startswith(prefix) for path in local):
            raise IntegrityError(f"Missing local native overlay: {wrap.path}")


def validate_coverage(manifest: Manifest, local: Mapping[str, bytes]) -> None:
    """Discover every vendor file and wrap, including newly introduced dependencies."""
    if "vendor/native-components.json" in manifest.maintained_files:
        metadata = fields(
            decode_json(local["vendor/native-components.json"]),
            {
                "wrap_components",
                "adapted_component",
                "openssl",
                "registry_component",
                "native_links",
            },
        )
        for record in array_value(metadata["wrap_components"]):
            _ = fields(record, {"source", "name", "version", "linkage", "usage", "license"})
        for key in ("adapted_component", "openssl", "registry_component", "native_links"):
            _ = object_value(metadata[key])
    covered = set(manifest.files) | set(manifest.maintained_files) | {"vendor/integrity.json"}
    for tree in manifest.trees:
        covered |= {path for path in local if path.startswith(tree.path + "/")}
    if covered != set(local):
        raise IntegrityError(f"Unlisted or missing vendor files: {sorted(covered ^ set(local))}")
    if {wrap.path for wrap in manifest.wraps} != {path for path in local if path.endswith(".wrap")}:
        raise IntegrityError("Unlisted or missing native wraps")
    for wrap in manifest.wraps:
        validate_wrap(wrap, manifest, local)


def write_private(path: Path, data: bytes) -> None:
    """Create new private evidence without overwriting or following a previous path."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        _ = stream.write(data)


def json_bytes(value: JsonValue) -> bytes:
    """Serialize deterministic evidence with actual newlines."""
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def verify(root: Path, cache: Path, output: Path, *, offline: bool) -> None:
    """Authenticate sources and emit a full comparison only after all checks succeed."""
    cache = private_directory(cache, create=not cache.exists())
    output = private_directory(output, create=True)
    local = {"vendor/" + path: body for path, body in tree_files(root / "vendor").items()}
    manifest_data = local["vendor/integrity.json"]
    if len(manifest_data) > MAX_MANIFEST:
        raise IntegrityError("Manifest size limit exceeded")
    manifest = parse_manifest(manifest_data)
    validate_coverage(manifest, local)
    source_records: list[JsonValue] = []
    source_size = 0
    for identifier, source in sorted(manifest.sources.items()):
        body = source_bytes(source, cache, offline=offline)
        source_size += len(body)
        if source_size > MAX_CONTENT:
            raise IntegrityError("Combined source byte budget exceeded")
        source_records.append(
            {
                "id": identifier,
                "url": source.url,
                "sha256": source.sha256,
                "size": len(body),
                "format": source.format,
            }
        )
    errors: list[str] = []
    reports: list[JsonValue] = []
    diff = bytearray()
    for tree in manifest.trees:
        source = manifest.sources[tree.source]
        upstream = archive_files(source, source_bytes(source, cache, offline=True))
        selected = {
            path.removeprefix(tree.path + "/"): body
            for path, body in local.items()
            if path.startswith(tree.path + "/")
        }
        report, changes, issues = compare_tree(tree, upstream, selected)
        reports.append(report)
        errors.extend(issues)
        diff.extend(changes)
        if len(diff) > MAX_DIFF:
            raise IntegrityError("Vendor diff budget exceeded")
    for path, identifier in sorted(manifest.files.items()):
        if local[path] != source_bytes(manifest.sources[identifier], cache, offline=True):
            errors.append(f"Copied upstream file differs: {path}")
    write_private(output / "vendor.diff", bytes(diff))
    if errors:
        write_private(output / "failure.json", json_bytes({"errors": list[JsonValue](errors)}))
        raise IntegrityError("; ".join(errors))
    write_private(
        output / "report.json",
        json_bytes(
            {
                "manifest_sha256": sha256(manifest_data),
                "diff_sha256": sha256(bytes(diff)),
                "sources": source_records,
                "trees": reports,
                "files": [
                    {"path": path, "source": source, "sha256": sha256(local[path])}
                    for path, source in sorted(manifest.files.items())
                ],
                "maintained_files": [
                    {"path": path, "sha256": sha256(local[path])}
                    for path in manifest.maintained_files
                ],
            }
        ),
    )


@dataclass
class Arguments(argparse.Namespace):
    """Typed standalone CLI arguments; network subprocess has a separate command."""

    command: str = ""
    root: Path = Path(__file__).resolve().parents[1]
    cache: Path = Path()
    output: Path = Path()
    offline: bool = False
    url: str = ""
    destination: Path = Path()


def main(argv: Sequence[str] | None = None) -> int:
    """Run the current contract with concise, non-successful failure diagnostics."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("verify", argument_default=argparse.SUPPRESS)
    _ = check.add_argument("--root", type=Path)
    _ = check.add_argument("--cache", type=Path, required=True)
    _ = check.add_argument("--output", type=Path, required=True)
    _ = check.add_argument("--offline", action="store_true")
    fetch = commands.add_parser(
        "_download", argument_default=argparse.SUPPRESS, help=argparse.SUPPRESS
    )
    _ = fetch.add_argument("--url", required=True)
    _ = fetch.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args(argv, namespace=Arguments())
    try:
        if args.command == "_download":
            download(args.url, args.destination)
        else:
            verify(args.root.resolve(), args.cache, args.output, offline=args.offline)
    except (
        ValueError,
        EOFError,
        OSError,
        KeyError,
        configparser.Error,
        tarfile.TarError,
        zipfile.BadZipFile,
        bounded_process.ProcessError,
    ) as error:
        print(f"Vendor integrity failed: {error}", file=sys.stderr)  # noqa: T201 -- CLI diagnostic.
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
