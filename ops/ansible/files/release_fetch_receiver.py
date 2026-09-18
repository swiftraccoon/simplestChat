"""Receive one verified GitHub artifact URL over stdin; never log its signature.

The controller sends a public envelope, waits for the ready receipt, then obtains
and sends the expiring URL. Only four selected artifact files are extracted into
private evidence. Docker, services, credentials and deployment state are untouched.
"""

import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import ssl
import stat
import sys
import tempfile
import time
import zipfile
from contextlib import closing
from dataclasses import dataclass
from email.message import Message
from http import HTTPStatus
from pathlib import Path, PurePosixPath
from types import FrameType
from typing import IO, NoReturn, Protocol, TextIO, cast, override, runtime_checkable
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, HTTPSHandler, ProxyHandler, Request, build_opener

from release_artifact import ArtifactError, Manifest, sha256_file, validate_manifest, verify_archive
from release_json import (
    DuplicateJsonError,
    JsonObject,
    JsonValue,
    integer_value,
    string_value,
)
from release_json import decode_json as decode_value

ROOT = Path("/srv/simplestchat-public")
CONFIG = Path("/etc/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
LINE_BYTES = 16384
CHUNK = 1024 * 1024
MAX_ZIP_BYTES = 2 * 1024**3
MAX_IMAGE_BYTES = 2 * 1024**3
MAX_METADATA_BYTES = 8 * CHUNK
MAX_EXTRACTED_BYTES = MAX_IMAGE_BYTES + 16 * CHUNK
MAX_ZIP_MEMBERS = 256
ZIP_END_HEADER_BYTES = 22
ZIP64_OFFSET = 0xFFFFFFFF
ASCII_SPACE = 32
ASCII_DELETE = 127
SELECTED = ("image.tar", "release.json", "outcome.json", "source.json")
SOURCE_KEYS = {
    "Dockerfile",
    ".dockerignore",
    "Cargo.lock",
    "web/package-lock.json",
    "build/pip-constraints.txt",
}
ENVELOPE_KEYS = {
    "schemaVersion",
    "repository",
    "artifactId",
    "buildRunId",
    "ciRunId",
    "revision",
    "artifactZipBytes",
    "zipSha256",
}
FAILURE_CLASSES = {
    "precondition_failed",
    "ambiguous_json",
    "invalid_json",
    "invalid_request",
    "invalid_envelope",
    "invalid_url",
    "unsafe_filesystem",
    "unfinished_operation",
    "http_response_rejected",
    "download_size_mismatch",
    "zip_checksum_mismatch",
    "zip_shape_rejected",
    "zip_member_size_mismatch",
    "build_outcome_rejected",
    "source_evidence_rejected",
    "release_revision_mismatch",
    "retained_artifact_differs",
    "root_required",
    "insufficient_disk",
    "deadline_exceeded",
    "interrupted",
}


class FetchError(Exception):
    """Only fixed coarse classes, never exception messages, enter public output."""

    def __init__(self, failure: object) -> None:
        """Retain an opaque failure without permitting it into public receipts."""
        super().__init__(failure)
        self.failure: object = failure


class LineSource(Protocol):
    """Provide bounded binary line input for the two-message handshake."""

    def readline(self, size: int = -1, /) -> bytes:
        """Read at most the requested bytes without losing newline evidence."""
        ...


class Headers(Protocol):
    """Expose only the string headers needed to validate a download."""

    def get(self, key: str, default: str | None = None, /) -> str | None:
        """Return a header or the specified fallback."""
        ...


@runtime_checkable
class DownloadResponse(Protocol):
    """Describe the bounded subset of urllib's dynamically typed response."""

    @property
    def status(self) -> int:
        """Return the HTTP status code."""
        ...

    @property
    def headers(self) -> Headers:
        """Return response headers without retaining URL-bearing exceptions."""
        ...

    def read(self, size: int = -1, /) -> bytes:
        """Read no more than the caller's remaining archive allowance."""
        ...

    def close(self) -> None:
        """Release the underlying connection on every exit path."""
        ...


@dataclass(slots=True, kw_only=True)
class DownloadProgress:
    """Track the exact bytes retained before a download failure or success."""

    downloaded_bytes: int


def require(condition: object, failure: str = "precondition_failed") -> None:
    """Reject an unmet invariant using only a fixed public failure class."""
    if not condition:
        raise FetchError(failure)


def decode_json(data: str | bytes) -> JsonValue:
    """Validate JSON while preserving the protocol's coarse ambiguity classes."""
    try:
        return decode_value(data)
    except DuplicateJsonError:
        message = "ambiguous_json"
        raise FetchError(message) from None
    except (json.JSONDecodeError, UnicodeDecodeError):
        # Preserve the established validation_failed class for malformed encoding/syntax.
        raise
    except ValueError:
        message = "invalid_json"
        raise FetchError(message) from None


def read_line(source: LineSource) -> JsonObject:
    """Read one bounded, newline-terminated JSON object from private stdin."""
    data = source.readline(LINE_BYTES + 1)
    require(0 < len(data) <= LINE_BYTES and data.endswith(b"\n"), "invalid_request")
    value = decode_json(data)
    if not isinstance(value, dict):
        message = "invalid_request"
        raise FetchError(message)
    return value


def validate_envelope(value: JsonValue) -> JsonObject:
    """Validate canonical identities and archive bounds before creating evidence."""
    if not isinstance(value, dict):
        message = "invalid_envelope"
        raise FetchError(message)
    require(set(value) == ENVELOPE_KEYS, "invalid_envelope")
    require(type(value["schemaVersion"]) is int and value["schemaVersion"] == 1, "invalid_envelope")
    repository = value["repository"]
    require(
        isinstance(repository, str)
        and re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", repository
        ),
        "invalid_envelope",
    )
    for key in ("artifactId", "buildRunId", "ciRunId"):
        identifier = value[key]
        require(type(identifier) is int and 0 < identifier <= 2**63 - 1, "invalid_envelope")
    revision, digest, size = value["revision"], value["zipSha256"], value["artifactZipBytes"]
    require(
        isinstance(revision, str) and re.fullmatch(r"[a-f0-9]{40}", revision), "invalid_envelope"
    )
    require(isinstance(digest, str) and re.fullmatch(r"[a-f0-9]{64}", digest), "invalid_envelope")
    require(type(size) is int and 0 < size <= MAX_ZIP_BYTES, "invalid_envelope")
    return value


def validate_url(value: JsonValue) -> str:
    """Accept only one ASCII HTTPS Azure artifact URL without redirection."""
    if not isinstance(value, dict):
        message = "invalid_url"
        raise FetchError(message)
    require(set(value) == {"url"}, "invalid_url")
    url = value["url"]
    if not isinstance(url, str):
        message = "invalid_url"
        raise FetchError(message)
    require(
        0 < len(url) <= LINE_BYTES
        and not any(
            ord(character) <= ASCII_SPACE or ord(character) >= ASCII_DELETE for character in url
        ),
        "invalid_url",
    )
    parsed = urlsplit(url)
    require(
        parsed.scheme == "https"
        and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}\.blob\.core\.windows\.net", parsed.netloc)
        and parsed.path.startswith("/")
        and parsed.query
        and not parsed.fragment,
        "invalid_url",
    )
    return url


def protected(
    path: Path, *, directory: bool = False, mode: int = 0o600, limit: int | None = None
) -> None:
    """Require root ownership and exact private mode, type, and optional size."""
    metadata = path.lstat()
    require(metadata.st_uid == 0 and stat.S_IMODE(metadata.st_mode) == mode, "unsafe_filesystem")
    require(
        stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode),
        "unsafe_filesystem",
    )
    if limit is not None:
        require(0 < metadata.st_size <= limit, "unsafe_filesystem")


def sync_directory(path: Path) -> None:
    """Persist directory entries before announcing publication or settlement."""
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def write_json(path: Path, value: JsonValue) -> None:
    """Create immutable private evidence and persist its complete JSON value."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        _ = output.write("\n")
        output.flush()
        os.fsync(output.fileno())


def finished_journal(path: Path) -> None:
    """Refuse an existing operation that has not durably finalized its ownership."""
    if path.exists() or path.is_symlink():
        protected(path, limit=LINE_BYTES)
        value = decode_json(path.read_bytes())
        require(
            isinstance(value, dict)
            and type(value.get("schemaVersion")) is int
            and value["schemaVersion"] == 1
            and value.get("finalized") is True,
            "unfinished_operation",
        )


class NoRedirect(HTTPRedirectHandler):
    """Prevent expiring artifact credentials from following a redirect."""

    @override
    def redirect_request(
        self, req: Request, fp: IO[bytes], code: int, msg: str, headers: Message, newurl: str
    ) -> None:
        """Refuse every redirect without constructing another request."""
        del req, fp, code, msg, headers, newurl


def download(url: str, target: Path, envelope: JsonObject, report: JsonObject) -> None:
    """Retain only bounded, checksum-matching bytes from a validated TLS response."""
    _ = validate_url({"url": url})
    context = ssl.create_default_context(cafile="/etc/ssl/certs/ca-certificates.crt")
    opener = build_opener(ProxyHandler({}), HTTPSHandler(context=context), NoRedirect())
    request = Request(  # noqa: S310 - validated HTTPS Azure URL; redirects and proxies disabled.
        url, headers={"Accept": "application/zip", "Accept-Encoding": "identity"}
    )
    expected = integer_value(envelope["artifactZipBytes"])
    progress = DownloadProgress(downloaded_bytes=integer_value(report["downloadedBytes"]))
    digest = hashlib.sha256()
    # urllib exposes an untyped return; admit only the response operations used below.
    raw_response = cast("object", opener.open(request, timeout=15))
    if not isinstance(raw_response, DownloadResponse):
        message = "http_response_rejected"
        raise FetchError(message)
    with closing(raw_response) as response, target.open("xb") as output:
        require(response.status == HTTPStatus.OK, "http_response_rejected")
        require(
            response.headers.get("Content-Encoding", "identity") == "identity",
            "http_response_rejected",
        )
        length = response.headers.get("Content-Length")
        require(length is None or length == str(expected), "http_response_rejected")
        while True:
            chunk = response.read(min(CHUNK, expected - progress.downloaded_bytes + 1))
            if not chunk:
                break
            require(progress.downloaded_bytes + len(chunk) <= expected, "download_size_mismatch")
            _ = output.write(chunk)
            digest.update(chunk)
            progress.downloaded_bytes += len(chunk)
            report["downloadedBytes"] = progress.downloaded_bytes
        output.flush()
        os.fsync(output.fileno())
    require(report["downloadedBytes"] == expected, "download_size_mismatch")
    require(digest.hexdigest() == envelope["zipSha256"], "zip_checksum_mismatch")


def validate_zip_directory(path: Path) -> None:
    """Bound central-directory allocation before zipfile reads its metadata."""
    size = path.stat().st_size
    require(ZIP_END_HEADER_BYTES <= size <= MAX_ZIP_BYTES, "zip_shape_rejected")
    with path.open("rb") as source:
        _ = source.seek(max(0, size - 65557))
        tail = source.read(65557)
    offset = tail.rfind(b"PK\x05\x06")
    require(offset >= 0 and len(tail) - offset >= ZIP_END_HEADER_BYTES, "zip_shape_rejected")
    disk, directory_disk, disk_entries, entries = (
        int.from_bytes(tail[offset + start : offset + start + 2], "little")
        for start in (4, 6, 8, 10)
    )
    directory_bytes, directory_offset = (
        int.from_bytes(tail[offset + start : offset + start + 4], "little") for start in (12, 16)
    )
    comment_bytes = int.from_bytes(tail[offset + 20 : offset + 22], "little")
    end_offset = size - len(tail) + offset
    require(
        offset + 22 + comment_bytes == len(tail)
        and disk == directory_disk == 0
        and disk_entries == entries
        and 0 < entries <= MAX_ZIP_MEMBERS
        and 0 < directory_bytes <= CHUNK
        and directory_offset != ZIP64_OFFSET
        and directory_offset + directory_bytes == end_offset,
        "zip_shape_rejected",
    )


def selected_members(archive: zipfile.ZipFile) -> dict[str, zipfile.ZipInfo]:
    """Select four canonical regular files after bounding every archive member."""
    members = archive.infolist()
    require(0 < len(members) <= MAX_ZIP_MEMBERS, "zip_shape_rejected")
    require(
        sum(member.file_size for member in members) <= MAX_EXTRACTED_BYTES, "zip_shape_rejected"
    )
    indexed: dict[str, zipfile.ZipInfo] = {}
    for member in members:
        name = member.filename
        path = PurePosixPath(name)
        canonical = name.rstrip("/")
        require(
            name
            and name == member.orig_filename
            and "\\" not in name
            and not any(ord(char) < ASCII_SPACE or ord(char) >= ASCII_DELETE for char in name)
            and not path.is_absolute()
            and ".." not in path.parts
            and str(path) == canonical
            and name == canonical + ("/" if member.is_dir() else "")
            and canonical not in ("", ".")
            and canonical not in indexed,
            "zip_shape_rejected",
        )
        kind = stat.S_IFMT(member.external_attr >> 16)
        require(
            kind in ((0, stat.S_IFDIR) if member.is_dir() else (0, stat.S_IFREG))
            and (not member.external_attr & 0x10 or member.is_dir())
            and not member.flag_bits & 1
            and member.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
            "zip_shape_rejected",
        )
        require(
            0
            <= member.file_size
            <= (MAX_IMAGE_BYTES if name == "image.tar" else MAX_METADATA_BYTES),
            "zip_shape_rejected",
        )
        indexed[canonical] = member
    for name in SELECTED:
        require(
            name in indexed
            and indexed[name].filename == name
            and not indexed[name].is_dir()
            and indexed[name].file_size > 0,
            "zip_shape_rejected",
        )
    return {name: indexed[name] for name in SELECTED}


def extract_member(archive: zipfile.ZipFile, member: zipfile.ZipInfo, destination: Path) -> None:
    """Extract one prevalidated member without retaining bytes beyond its bound."""
    total = 0
    with archive.open(member) as source, destination.open("xb") as output:
        while chunk := source.read(min(CHUNK, member.file_size - total + 1)):
            require(total + len(chunk) <= member.file_size, "zip_member_size_mismatch")
            _ = output.write(chunk)
            total += len(chunk)
        require(total == member.file_size, "zip_member_size_mismatch")
        output.flush()
        os.fsync(output.fileno())


def validate_build_evidence(attempt: Path, envelope: JsonObject) -> Manifest:
    """Bind successful source/build evidence and archive to the requested revision."""
    outcome = decode_json((attempt / "outcome.json").read_bytes())
    require(
        isinstance(outcome, dict)
        and type(outcome.get("schemaVersion")) is int
        and outcome["schemaVersion"] == 1
        and outcome.get("passed") is True
        and "error" in outcome
        and outcome["error"] is None
        and outcome.get("revision") == envelope["revision"],
        "build_outcome_rejected",
    )
    source = decode_json((attempt / "source.json").read_bytes())
    require(
        isinstance(source, dict)
        and set(source) == {"revision", "inputsSha256"}
        and source["revision"] == envelope["revision"]
        and isinstance(source["inputsSha256"], dict)
        and set(source["inputsSha256"]) == SOURCE_KEYS
        and all(
            isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value)
            for value in source["inputsSha256"].values()
        ),
        "source_evidence_rejected",
    )
    manifest = validate_manifest(attempt / "release.json")
    require(manifest["revision"] == envelope["revision"], "release_revision_mismatch")
    _ = verify_archive(attempt / "image.tar", manifest)
    return manifest


def publish(attempt: Path, destination: Path) -> None:
    """Hardlink verified bytes without ever replacing a retained artifact."""
    # Check both existing selections before creating either new hardlink.
    for name in ("release.json", "image.tar"):
        target, source = destination / name, attempt / name
        if target.exists() or target.is_symlink():
            protected(target)
            require(
                target.stat().st_size == source.stat().st_size
                and sha256_file(target) == sha256_file(source),
                "retained_artifact_differs",
            )
    for name in ("release.json", "image.tar"):
        target = destination / name
        try:
            os.link(attempt / name, target, follow_symlinks=False)
        except FileExistsError:
            protected(target)
            require(
                target.stat().st_size == (attempt / name).stat().st_size
                and sha256_file(target) == sha256_file(attempt / name),
                "retained_artifact_differs",
            )
    sync_directory(destination)


def failure_class(error: BaseException) -> str:  # noqa: PLR0911 - explicit redaction classes are deliberately separate.
    """Map failures to fixed categories without exposing URLs or exception text."""
    if isinstance(error, FetchError):
        return (
            error.failure
            if isinstance(error.failure, str) and error.failure in FAILURE_CLASSES
            else "validation_failed"
        )
    if isinstance(error, HTTPError):
        error.close()
        return "http_error"
    if isinstance(error, (URLError, TimeoutError)):
        return "transport_error"
    if isinstance(error, zipfile.BadZipFile):
        return "zip_error"
    if isinstance(error, OSError):
        return "filesystem_error"
    if isinstance(error, (ArtifactError, ValueError, KeyError, TypeError)):
        return "validation_failed"
    return "internal_error"


def main(source: LineSource | None = None, output: TextIO | None = None) -> int:  # noqa: PLR0915 - keep lock, publication, and receipt settlement in one visible transaction.
    """Receive, verify, and publish one artifact under the persistent workload lock."""
    input_stream: LineSource = sys.stdin.buffer if source is None else source
    output_stream: TextIO = sys.stdout if output is None else output
    _ = os.umask(0o077)
    began = time.monotonic()
    attempt: Path | None = None
    lock: IO[bytes] | None = None
    report: JsonObject = {
        "schemaVersion": 1,
        "status": "failed",
        "passed": False,
        "settled": False,
        "phase": "envelope",
        "failureClass": None,
        "evidence": None,
        "revision": None,
        "artifactId": None,
        "zipSha256": None,
        "artifactZipBytes": None,
        "downloadedBytes": 0,
        "archiveSha256": None,
        "manifestSha256": None,
        "elapsedSeconds": 0,
        "finalized": False,
    }

    def interrupted(number: int, _frame: FrameType | None) -> NoReturn:
        raise FetchError("deadline_exceeded" if number == signal.SIGALRM else "interrupted")

    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        _ = signal.signal(number, interrupted)
    _ = signal.alarm(300)
    try:
        require(os.geteuid() == 0, "root_required")
        envelope = validate_envelope(read_line(input_stream))
        report.update(
            {
                key: envelope[key]
                for key in ("revision", "artifactId", "zipSha256", "artifactZipBytes")
            }
        )
        report["phase"] = "preflight"
        for path in (ROOT, CONFIG, ROOT / "releases", ROOT / "results"):
            protected(path, directory=True, mode=0o700)
        WORK.mkdir(mode=0o700, exist_ok=True)
        protected(WORK, directory=True, mode=0o700)
        try:
            descriptor = os.open(
                WORK / "workload.lock", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
            )
            os.close(descriptor)
        except FileExistsError:
            pass
        protected(WORK / "workload.lock")
        lock = (WORK / "workload.lock").open("rb")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finished_journal(WORK / "current.json")
        finished_journal(ROOT / "release-state.json")
        destination = ROOT / "releases" / string_value(envelope["revision"])
        destination.mkdir(mode=0o700, exist_ok=True)
        protected(destination, directory=True, mode=0o700)
        sync_directory(ROOT / "releases")
        attempt = Path(tempfile.mkdtemp(prefix="download.", dir=destination))
        report["evidence"] = str(attempt)
        write_json(attempt / "envelope.json", envelope)
        sync_directory(attempt)
        sync_directory(destination)
        require(
            shutil.disk_usage(destination).free
            >= integer_value(envelope["artifactZipBytes"]) + MAX_EXTRACTED_BYTES + 1024**3,
            "insufficient_disk",
        )
        report["phase"] = "await_url"
        _ = output_stream.write(json.dumps({"schemaVersion": 1, "status": "ready"}) + "\n")
        output_stream.flush()
        url = validate_url(read_line(input_stream))
        report["phase"] = "download"
        download(url, attempt / "artifact.zip", envelope, report)
        del url
        report["phase"] = "extract"
        validate_zip_directory(attempt / "artifact.zip")
        with zipfile.ZipFile(attempt / "artifact.zip") as archive:
            for name, member in selected_members(archive).items():
                extract_member(archive, member, attempt / name)
        report["phase"] = "verify"
        manifest = validate_build_evidence(attempt, envelope)
        report["archiveSha256"] = manifest["archiveSha256"]
        report["manifestSha256"] = sha256_file(attempt / "release.json")
        sync_directory(attempt)
        report["phase"] = "publish"
        publish(attempt, destination)
        report.update(
            {"status": "complete", "passed": True, "phase": "complete", "finalized": True}
        )
    except BaseException as error:  # noqa: BLE001 - all failures must emit only redacted classes.
        report.update({"status": "failed", "passed": False})
        report["failureClass"] = failure_class(error)
    finally:
        report["elapsedSeconds"] = round(time.monotonic() - began, 3)
        report["settled"] = attempt is not None
        try:
            if attempt is not None:
                write_json(attempt / "fetch-outcome.json", report)
                sync_directory(attempt)
        except BaseException:  # noqa: BLE001 - failed evidence persistence must never report success.
            report.update(
                {
                    "status": "failed",
                    "passed": False,
                    "settled": False,
                    "failureClass": "receipt_write_failed",
                }
            )
        finally:
            if lock is not None:
                lock.close()
            _ = signal.alarm(0)
        _ = output_stream.write(json.dumps(report) + "\n")
        output_stream.flush()
    return 0 if report["passed"] and report["settled"] and report["finalized"] else 1
