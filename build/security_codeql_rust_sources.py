"""Bind Rust macro locations to their complete, committed literal include closure.

Rust extraction can locate an included expression at the parent's include! call.
Reviewing only the parent hash would leave the actual expression unbound. This
reader never executes Rust or expands macros; unsupported include syntax fails.
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path, PurePosixPath

from security_codeql_sources import tracked_bytes
from security_tools import require

MAX_FILES = 64
MAX_EDGES = 256
MAX_BYTES = 16 * 1024**2
MAX_DEPTH = 16
MAX_TOKENS = 500_000
MAX_PATH = 1024
MIN_INVOCATION = 4
TOKEN = re.compile(
    r'\s+|//[^\n]*|/\*|(?P<raw>(?:br|cr|r)(?P<hash>\#{0,255})")'
    + r'|(?P<quote>(?:b|c)?")|(?:b)?\'(?:\\.|[^\'\\\n])\''
    + r"|(?P<identifier>(?:r\#)?[A-Za-z_][A-Za-z0-9_]*)|\S",
)
COMMENT = re.compile(r"/\*|\*/")


def tokens(text: str) -> list[tuple[str, str]]:
    """Skip comments and literal contents before recognizing include macro tokens."""
    result: list[tuple[str, str]] = []
    position = 0
    while position < len(text):
        match = TOKEN.match(text, position)
        require(match is not None, "codeql_rust_token")
        if match is None:
            break
        value = match.group()
        position = match.end()
        if value.isspace() or value.startswith("//"):
            continue
        if value == "/*":
            depth = 1
            for marker in COMMENT.finditer(text, position):
                depth += 1 if marker.group() == "/*" else -1
                if depth == 0:
                    position = marker.end()
                    break
            require(depth == 0, "codeql_rust_comment")
            continue
        if match.group("raw") is not None:
            end = text.find('"' + match.group("hash"), position)
            require(end >= 0, "codeql_rust_string")
            position = end + 1 + len(match.group("hash"))
            result.append(("opaque", ""))
        elif match.group("quote") is not None:
            start = position
            while position < len(text) and text[position] != '"':
                position += 2 if text[position] == "\\" else 1
            require(position < len(text), "codeql_rust_string")
            result.append(("literal" if value == '"' else "opaque", text[start:position]))
            position += 1
        else:
            result.append(("identifier" if match.group("identifier") else "other", value))
        require(len(result) <= MAX_TOKENS, "codeql_rust_token_limit")
    return result


def literal_includes(text: str) -> list[str]:
    """Accept only plain local string paths, with comments allowed between tokens."""
    stream = tokens(text)
    result: list[str] = []
    for index, token in enumerate(stream):
        if token not in {("identifier", "include"), ("identifier", "r#include")}:
            continue
        following = stream[index + 1 : index + 6]
        if not following or following[0] != ("other", "!"):
            continue
        require(
            len(following) >= MIN_INVOCATION
            and following[1] == ("other", "(")
            and following[2][0] == "literal"
            and (
                following[3] == ("other", ")") or following[3:] == [("other", ","), ("other", ")")]
            ),
            "codeql_rust_include_literal",
        )
        path = following[2][1]
        require(
            0 < len(path) <= MAX_PATH
            and re.fullmatch(r"[A-Za-z0-9_./-]+", path)
            and not PurePosixPath(path).is_absolute(),
            "codeql_rust_include_path",
        )
        result.append(path)
        require(len(result) <= MAX_EDGES, "codeql_rust_include_edges")
    return result


def included_sources(root: Path, revision: str, name: str) -> dict[str, str]:
    """Hash a bounded literal include closure, rejecting edited, linked or escaped files."""
    pending: list[tuple[str, int, frozenset[str]]] = [(name, 0, frozenset())]
    hashes: dict[str, str] = {}
    total_bytes = 0
    edges = 0
    while pending:
        selected, depth, ancestors = pending.pop()
        require(selected not in ancestors, "codeql_rust_include_cycle")
        require(depth <= MAX_DEPTH, "codeql_rust_include_depth")
        if selected in hashes:
            continue
        require(len(hashes) < MAX_FILES, "codeql_rust_include_limit")
        data = tracked_bytes(root, revision, selected)
        total_bytes += len(data)
        require(total_bytes <= MAX_BYTES, "codeql_rust_include_bytes")
        hashes[selected] = hashlib.sha256(data).hexdigest()
        for relative in literal_includes(data.decode()):
            edges += 1
            require(edges <= MAX_EDGES, "codeql_rust_include_edges")
            dependency = include_path(root, selected, relative)
            pending.append((dependency, depth + 1, ancestors | {selected}))
    return {path: hashes[path] for path in sorted(hashes) if path != name}


def include_path(root: Path, selected: str, relative: str) -> str:
    """Normalize local parents without hiding a symlink or temporary root escape."""
    parts = list(PurePosixPath(selected).parent.parts)
    for part in relative.split("/"):
        if part in {"", "."}:
            continue
        if part == "..":
            require(bool(parts), "codeql_rust_include_escape")
            _ = parts.pop()
        else:
            parts.append(part)
            require(not root.joinpath(*parts).is_symlink(), "codeql_source_symlink")
    require(bool(parts), "codeql_rust_include_escape")
    return "/".join(parts)
