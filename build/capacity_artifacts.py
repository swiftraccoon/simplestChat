"""Collect untrusted generator files without exposing host paths to container data.

Only the generator's three documented JSON basenames may enter a fresh private
directory. Archive metadata never controls host paths, permissions or ownership.
Validate the whole bounded archive before opening any result; retain a host-made
size/digest manifest only after the transfer and every extraction succeeds.
"""

from __future__ import annotations

import hashlib
import json
import os
import stat
import tarfile
from typing import TYPE_CHECKING

import bounded_process

if TYPE_CHECKING:
    from pathlib import Path
    from typing import BinaryIO

MIB = 1024**2
MAX_RESULT = 128 * MIB
MAX_SUMMARY = 8 * MIB
MAX_ARCHIVE = 144 * MIB
EXPECTED = {
    "load_test_summary.json": MAX_SUMMARY,
    "load_test_results.json": MAX_RESULT,
    "load_test_timeout.json": MAX_SUMMARY,
}
REQUIRED = frozenset({"load_test_summary.json", "load_test_results.json"})
MAX_TAR_METADATA = 8192
TAR_BLOCK = 512


class ArtifactError(RuntimeError):
    """Collection failed; partial evidence is not a valid measurement."""


def require(condition: object, code: str) -> None:
    """Fail with a stable diagnostic without echoing archive-controlled strings."""
    if not condition:
        raise ArtifactError(code)


def read_regular(path: Path, maximum: int = MAX_RESULT) -> bytes:
    """Bound a regular-file read and refuse symlinks, devices and changing lengths."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        metadata = os.fstat(descriptor)
        require(stat.S_ISREG(metadata.st_mode), "result_not_regular")
        require(0 <= metadata.st_size <= maximum, "result_too_large")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            contents = source.read(maximum + 1)
        require(len(contents) == metadata.st_size, "result_size_changed")
        return contents
    finally:
        os.close(descriptor)


def new_file(path: Path, contents: bytes) -> None:
    """Write only to a new host-owned regular file, refusing any existing target."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        _ = output.write(contents)


def check_headers(stream: BinaryIO, maximum_headers: int, maximum_file: int) -> None:
    """Bound physical tar headers before tarfile can allocate extended metadata bodies."""
    total = stream.seek(0, os.SEEK_END)
    _ = stream.seek(0)
    headers = 0
    while stream.tell() < total:
        header = stream.read(TAR_BLOCK)
        require(len(header) == TAR_BLOCK, "truncated_tar_header")
        if not any(header):
            require(stream.read(TAR_BLOCK) == bytes(TAR_BLOCK), "missing_tar_end_marker")
            while chunk := stream.read(65536):
                require(not any(chunk), "trailing_tar_data")
            _ = stream.seek(0)
            return
        member = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
        headers += 1
        require(headers <= maximum_headers, "too_many_tar_headers")
        require(
            member.type in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE, tarfile.XHDTYPE),
            "unsafe_tar_header",
        )
        maximum = MAX_TAR_METADATA if member.type == tarfile.XHDTYPE else maximum_file
        require(0 <= member.size <= maximum, "oversized_tar_body")
        following = stream.tell() + ((member.size + TAR_BLOCK - 1) // TAR_BLOCK) * TAR_BLOCK
        require(following <= total, "truncated_tar_body")
        _ = stream.seek(following)
    message = "missing_tar_end_marker"
    raise ArtifactError(message)


def extract(archive: Path, destination: Path) -> dict[str, object]:
    """Validate an uncompressed Docker archive before extracting allowlisted files."""
    require(archive.stat().st_size <= MAX_ARCHIVE, "result_archive_too_large")
    destination.mkdir(mode=0o700)
    with archive.open("rb") as headers:
        check_headers(headers, 8, MAX_RESULT)
    seen: set[str] = set()
    selected: list[tuple[tarfile.TarInfo, str]] = []
    total = 0
    with tarfile.open(archive, mode="r:") as bundle:
        for member in bundle:
            name = member.name.removeprefix("./")
            # Docker may include the copied directory's own header. Nothing is
            # extracted for it, and no other directory or path is accepted.
            if member.isdir() and member.name in (".", "./"):
                require("." not in seen, "duplicate_result_entry")
                seen.add(".")
                continue
            require(name in EXPECTED and member.isfile(), "unsafe_result_entry")
            require(member.sparse is None and not member.linkname, "unsafe_result_entry")
            require(name not in seen, "duplicate_result_entry")
            require(0 <= member.size <= EXPECTED[name], "result_file_too_large")
            seen.add(name)
            total += member.size
            require(total <= MAX_ARCHIVE, "result_archive_too_large")
            selected.append((member, name))
        require(seen >= REQUIRED, "missing_required_results")
        require("load_test_timeout.json" not in seen, "generator_timeout_marker")
        files: dict[str, object] = {}
        for member, name in selected:
            source = bundle.extractfile(member)
            require(source is not None, "missing_result_body")
            if source is None:
                raise ArtifactError("missing_result_body")  # noqa: EM101 -- fixed diagnostic.
            digest = hashlib.sha256()
            descriptor = os.open(
                destination / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
            )
            with source, os.fdopen(descriptor, "wb") as output:
                remaining = member.size
                while remaining:
                    chunk = source.read(min(65536, remaining))
                    require(chunk, "truncated_result_body")
                    _ = output.write(chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
            files[name] = {"bytes": member.size, "sha256": digest.hexdigest()}
    return {"schemaVersion": 1, "complete": True, "files": files}


def collect(executable: str, container: str, directory: Path) -> None:
    """Stream bounded Docker output, require successful transfer, and publish its manifest."""
    archive = directory / "generator-results.tar"
    with archive.open("xb") as output, (directory / "collection.stderr").open("xb") as error:
        status, _, _ = bounded_process.run(
            [executable, "cp", f"{container}:/results/.", "-"],
            output=output,
            error=error,
            limits=bounded_process.Limits(timeout=120, stdout=MAX_ARCHIVE),
        )
    require(status == 0, "result_transfer_failed")
    manifest = extract(archive, directory / "generator")
    digest = hashlib.sha256()
    with archive.open("rb") as transferred:
        while chunk := transferred.read(65536):
            digest.update(chunk)
    manifest["archiveSha256"] = digest.hexdigest()
    manifest["archiveBytes"] = archive.stat().st_size
    new_file(directory / "collection.json", (json.dumps(manifest, indent=2) + "\n").encode())
    archive.unlink()
