"""Require actual compiler-observed native media sources in the CodeQL database."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import cast

from security_findings import list_value, object_value
from security_tools import ToolError, bounded_file, require, string

REQUIRED = frozenset(
    {
        "vendor/mediasoup-sys-0.19.0/src/RTC/DtlsTransport.cpp",
        "vendor/mediasoup-sys-0.19.0/src/RTC/ICE/StunPacket.cpp",
        "vendor/mediasoup-sys-0.19.0/src/RTC/SCTP/association/Association.cpp",
        "vendor/mediasoup-sys-0.19.0/src/RTC/RTP/Packet.cpp",
    }
)


def verify(path: Path) -> int:
    """Reject an empty or incomplete extraction even when the compiler returned zero."""
    raw = object_value(cast("object", json.loads(bounded_file(path, 4 * 1024**2))))
    table = object_value(raw["#select"])
    files: set[str] = set()
    for value in list_value(table["tuples"]):
        row = list_value(value)
        require(len(row) == 1, "codeql_coverage_columns")
        files.add(string(row[0]))
    require(files >= REQUIRED, "codeql_native_extraction_incomplete")
    return len(files)


def main() -> int:
    """Print only the source count; never include extracted source or query results."""
    try:
        require(len(sys.argv[1:]) == 1, "codeql_coverage_argument")
        count = verify(Path(sys.argv[1]))
    except (ToolError, OSError, ValueError, KeyError):
        _ = sys.stderr.write("Native CodeQL extraction evidence failed validation.\n")
        return 1
    _ = sys.stdout.write(f"Verified native CodeQL extraction: {count} compiled files.\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
