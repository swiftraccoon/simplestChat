"""Hash uniquely resolved Gitleaks match regions without publishing their bytes.

Coordinates belong to the pinned scanner's fragments, not ordinary text lines.
Repeated coordinates on a fragmented long line are deliberately ambiguous.
These diagnostics never participate in exception matching.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
import time
from bisect import bisect_left, bisect_right
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    integer_value,
    object_value,
    string_value,
)
from security_secret_projection import FORMAT as PROJECTION_FORMAT
from security_secret_projection import PREFIX, identity
from security_tools import load_lock, require

if TYPE_CHECKING:
    from collections.abc import Iterator
    from pathlib import Path
    from typing import BinaryIO

VERSION = "8.30.1"
FORMAT = "gitleaks-8.30.1-projection-match"
BUFFER = 100000
PEEK = 25000
READER_BUFFER = 4096
BOUNDARY_NEWLINES = 2
MAX_FILE = 256 * 1024**2 + len(PREFIX)
MAX_TOTAL = 4 * 1024**3
MAX_FINDINGS = 20000
MAX_CANDIDATES = 100000
SECONDS = 90
WHITESPACE = b" \t\r\n"
COORDINATES = ("StartLine", "EndLine", "StartColumn", "EndColumn")


@dataclass
class Reader:
    """Mirror bufio.Reader's regular-file reads, including unread boundary lookahead."""

    source: BinaryIO
    pending: bytes = b""
    position: int = 0

    def block(self) -> bytes:
        """Consume pending bytes before making a direct file read, as Go large Read does."""
        if self.position < len(self.pending):
            data = self.pending[self.position :]
            self.position = len(self.pending)
            return data
        return self.source.read(BUFFER)

    def byte(self) -> bytes:
        """ReadByte refills the pinned default 4096-byte buffered reader."""
        if self.position == len(self.pending):
            self.pending = self.source.read(READER_BUFFER)
            self.position = 0
        result = self.pending[self.position : self.position + 1]
        self.position += len(result)
        return result


def fragments(source: BinaryIO) -> Iterator[bytes]:
    """Port only pinned sources/file.go and common.go framing, never its detection rules."""
    reader = Reader(source)
    while data := reader.block():
        trailing = data[len(data.rstrip(WHITESPACE)) :]
        if trailing.count(b"\n") >= BOUNDARY_NEWLINES:
            yield data
            continue
        part = bytearray(data)
        newlines = 0
        while True:
            last = part[-1]
            if last == ord("\n"):
                newlines += 1
            elif last not in WHITESPACE:
                newlines = 0
            if newlines >= BOUNDARY_NEWLINES or len(part) - len(data) >= PEEK:
                break
            byte = reader.byte()
            if not byte:
                break
            part.extend(byte)
        yield bytes(part)


def region(data: bytes, lines: list[int], coordinates: tuple[int, int, int, int]) -> bytes | None:
    """Invert and round-trip the pinned byte-column convention; never infer a secret group."""
    first, last, column, end_column = coordinates
    if not (0 <= first <= last <= len(lines) and column > 0 and end_column > 0):
        return None
    start = (lines[first - 1] if first else 0) + column - 1
    end = (lines[last - 1] if last else 0) + end_column
    if not (0 <= start < end <= len(data)):
        return None
    # The scanner counts the preceding LF as column one. When a multiline
    # match ends after the last LF, its report can have EndColumn=0; do not guess.
    actual_first = bisect_right(lines, start)
    actual_last = bisect_left(lines, end)
    if actual_first != first or actual_last != last:
        return None
    return data[start:end]


@dataclass
class Finding:
    """Retain at most one candidate digest and an ambiguity flag, never candidate text."""

    coordinates: tuple[int, int, int, int]
    evidence: JsonObject
    candidate: tuple[int, str] | None = None
    ambiguous: bool = False

    def consider(self, data: bytes) -> None:
        """Two possible spans remain ambiguous even if their content happens to agree."""
        if self.candidate is not None:
            self.ambiguous = True
        else:
            self.candidate = len(data), hashlib.sha256(data).hexdigest()


@dataclass
class Budget:
    """Bound aggregate reads, candidate work and elapsed time independently of scanner output."""

    deadline: float = field(default_factory=lambda: time.monotonic() + SECONDS)
    candidates: int = 0
    bytes: int = 0

    def check(self) -> None:
        """Failures reveal only the fixed resource class."""
        require(time.monotonic() <= self.deadline, "secret_span_deadline")
        require(self.candidates <= MAX_CANDIDATES, "secret_span_candidate_budget")
        require(self.bytes <= MAX_TOTAL, "secret_span_read_budget")


def inspect(path: Path, expected: JsonObject, findings: list[Finding], budget: Budget) -> None:
    """Authenticate the complete existing projection while locating bounded candidate regions."""
    size = expected["projectionBytes"]
    require(type(size) is int and 0 < size <= MAX_FILE, "secret_span_file_size")
    size = integer_value(size)
    checksum = string_value(expected["projectionSha256"])
    require(re.fullmatch(r"[a-f0-9]{64}", checksum), "secret_span_file_digest")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb", buffering=0) as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_size == size, "secret_span_file_kind")
        digest, total, line = hashlib.sha256(), 0, 1
        ordered = sorted(findings, key=lambda item: item.coordinates[0])
        starts = [item.coordinates[0] for item in ordered]
        for data in fragments(source):
            total += len(data)
            budget.bytes += len(data)
            budget.check()
            require(total <= size, "secret_span_file_changed")
            digest.update(data)
            lines = [index for index, byte in enumerate(data) if byte == ord("\n")]
            for item in ordered[
                bisect_left(starts, line) : bisect_right(starts, line + len(lines))
            ]:
                budget.candidates += 1
                budget.check()
                first, last, column, end_column = item.coordinates
                value = region(data, lines, (first - line, last - line, column, end_column))
                if value is not None:
                    item.consider(value)
            line += len(lines)
        require(
            total == size
            and digest.hexdigest() == checksum
            and identity(before) == identity(os.fstat(source.fileno()))
            and identity(before) == identity(path.stat(follow_symlinks=False)),
            "secret_span_file_changed",
        )


def collect(value: JsonValue, paths: JsonObject, directory: Path) -> JsonObject:
    """Add exact or explicitly unresolved diagnostics without changing the blocking verdict."""
    require(load_lock()[0]["gitleaks"].version == VERSION, "secret_span_scanner_version")
    rows = array_value(value)
    require(len(rows) <= MAX_FINDINGS, "secret_span_finding_count")
    grouped: dict[str, list[Finding]] = {}
    ordered: list[Finding] = []
    for raw in rows:
        finding = object_value(raw)
        name = string_value(finding["File"]).removeprefix("/layers/")
        require(re.fullmatch(r"content-[0-9]{6}", name) and name in paths, "secret_span_file")
        expected = object_value(paths[name])
        require(expected.get("projectionFormat") == PROJECTION_FORMAT, "secret_span_format")
        values = [finding[key] for key in COORDINATES]
        require(
            all(type(number) is int and 0 <= number <= MAX_FILE for number in values),
            "secret_span_coordinates",
        )
        first, last, column, end_column = (integer_value(number) for number in values)
        evidence: JsonObject = {
            "rule": finding["RuleID"],
            "path": expected["path"],
            "fileSha256": expected["sha256"],
            "projectionSha256": expected["projectionSha256"],
            "startLine": first,
            "endLine": last,
            "startColumn": column,
            "endColumn": end_column,
        }
        item = Finding((first, last, column, end_column), evidence)
        grouped.setdefault(name, []).append(item)
        ordered.append(item)
    budget = Budget()
    require(
        not rows or (directory.is_dir() and not directory.is_symlink()), "secret_span_directory"
    )
    for name, findings in grouped.items():
        inspect(directory / name, object_value(paths[name]), findings, budget)
    results: list[JsonValue] = []
    for item in ordered:
        entry = item.evidence
        entry["status"] = "ambiguous" if item.ambiguous else "unresolved"
        if item.candidate is not None and not item.ambiguous:
            entry.update(
                status="resolved", spanBytes=item.candidate[0], spanSha256=item.candidate[1]
            )
        results.append(entry)
    return {"format": FORMAT, "findings": results}
