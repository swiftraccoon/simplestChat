"""Produce bounded text views of regular files for content-sniffing secret scanners.

Every ASCII printable byte, tab and line ending is retained, including strings
inside otherwise binary inputs. Other bytes become newline delimiters. This is
explicit printable-string coverage, not decryption or arbitrary format decoding.
"""

from __future__ import annotations

import hashlib
import os
import stat
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path

CHUNK = 65536
FORMAT = "ascii-printable"
ASCII_FIRST, ASCII_LAST = 32, 126
# The pinned filetype matcher checks application signatures through ISO offset 32773.
PREFIX = b"SimplestChat printable secret-scan projection\n" + b"\n" * (32768 + 64)
LIMITATIONS = (
    "Only contiguous printable ASCII strings and their scanner-supported encodings are covered",
    "UTF-16, compression, encryption and strings split by nonprintable bytes are not decoded",
    "Lines refer to the projection; original paths and byte hashes remain authoritative",
)
TRANSLATION = bytes(
    value if value in (9, 10, 13) or ASCII_FIRST <= value <= ASCII_LAST else 10
    for value in range(256)
)


class ProjectionError(ValueError):
    """One fixed failure class for incomplete or unstable secret-scan input."""


def require(condition: object, reason: str) -> None:
    """Fail with a fixed diagnostic that never contains input contents."""
    if not condition:
        raise ProjectionError(reason)


@dataclass(frozen=True)
class Projection:
    """Bind the scanner's text bytes to the complete original file bytes."""

    source_sha256: str
    source_bytes: int
    projection_sha256: str
    projection_bytes: int
    format: str = FORMAT


def identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    """Detect replacement or ordinary in-place changes while streaming the input."""
    return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns


def project(source: Path, destination: Path, *, max_bytes: int) -> Projection:
    """Create a new private ASCII projection with bounded memory and output size."""
    require(type(max_bytes) is int and max_bytes >= 0, "secret_projection_limit")
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as incoming:
        before = os.fstat(incoming.fileno())
        require(stat.S_ISREG(before.st_mode), "secret_projection_source_kind")
        require(before.st_size <= max_bytes, "secret_projection_source_size")
        original, projected = hashlib.sha256(), hashlib.sha256()
        total = 0
        target = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(target, "wb") as outgoing:
            _ = outgoing.write(PREFIX)
            projected.update(PREFIX)
            while chunk := incoming.read(CHUNK):
                total += len(chunk)
                require(total <= max_bytes, "secret_projection_source_size")
                original.update(chunk)
                text = chunk.translate(TRANSLATION)
                projected.update(text)
                _ = outgoing.write(text)
        require(
            total == before.st_size
            and identity(before) == identity(os.fstat(incoming.fileno()))
            and identity(before) == identity(source.stat(follow_symlinks=False)),
            "secret_projection_source_changed",
        )
    return Projection(original.hexdigest(), total, projected.hexdigest(), total + len(PREFIX))
