"""Check production ELF hardening and Rust metadata without executing image files.

Only ELF64 little-endian Linux executables for the supported image platforms are
accepted. All offsets, tables, strings and decompression are bounded before use.
Dependency paths are interpreted inside the supplied image filesystem, including
absolute symlinks; host paths and the host dynamic linker are never consulted.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import struct
import sys
import zlib
from collections import deque
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import cast

MAX_BINARY = 256 * 1024 * 1024
MAX_METADATA = 8 * 1024 * 1024
MAX_STRINGS = 1024 * 1024
MAX_PACKAGES = 16384
MAX_TABLE = 8192
MAX_LINKS = 40
MAX_NAME = 256
ELF_HEADER = struct.Struct("<16sHHIQQQIHHHHHH")
PROGRAM_HEADER = struct.Struct("<IIQQQQQQ")
SECTION_HEADER = struct.Struct("<IIQQQQIIQQ")
DYNAMIC_ENTRY = struct.Struct("<qQ")
PT_LOAD = 1
PT_DYNAMIC = 2
PT_INTERP = 3
PT_GNU_STACK = 0x6474E551
PT_GNU_RELRO = 0x6474E552
DT_NEEDED = 1
DT_STRTAB = 5
DT_STRSZ = 10
DT_RPATH = 15
DT_TEXTREL = 22
DT_BIND_NOW = 24
DT_RUNPATH = 29
DT_FLAGS = 30
DT_FLAGS_1 = 0x6FFFFFFB
DF_TEXTREL = 4
DF_BIND_NOW = 8
DF_1_NOW = 1
DF_1_PIE = 0x08000000
ET_DYN = 3
PF_X = 1
PF_W = 2
SHT_PROGBITS = 1
SHT_STRTAB = 3
SHT_NOBITS = 8
PLATFORMS = {
    "linux/amd64": (62, "/lib64/ld-linux-x86-64.so.2"),
    "linux/arm64": (183, "/lib/ld-linux-aarch64.so.1"),
}
LIBRARY_DIRECTORIES = ("/lib64", "/usr/lib64", "/lib", "/usr/lib")
RUNTIME_LIBRARIES = frozenset({"libc.so.6", "libm.so.6", "libgcc_s.so.1"})


class ElfError(ValueError):
    """A fixed, safe-to-report ELF or artifact validation failure."""


def require(condition: object, reason: str) -> None:
    """Reject unsupported or incomplete evidence without echoing its contents."""
    if not condition:
        raise ElfError(reason)


def region(data: bytes, offset: int, size: int) -> bytes:
    """Read a complete bounded region; Python integers cannot wrap offsets."""
    check_bounds(data, offset, size)
    return data[offset : offset + size]


def check_bounds(data: bytes, offset: int, size: int) -> None:
    """Validate unused regions without repeatedly copying large overlapping tables."""
    require(0 <= offset <= len(data) and 0 <= size <= len(data) - offset, "elf_region_bounds")


def c_string(data: bytes, offset: int) -> str:
    """Read one bounded NUL-terminated ASCII metadata string."""
    require(0 <= offset < len(data), "elf_string_offset")
    end = data.find(b"\0", offset)
    require(end >= offset and end - offset <= MAX_NAME, "elf_string_bounds")
    try:
        return data[offset:end].decode("ascii")
    except UnicodeError as error:
        reason = "elf_string_encoding"
        raise ElfError(reason) from error


@dataclass(frozen=True)
class Segment:
    """The fields required from a validated ELF64 program header."""

    kind: int
    flags: int
    offset: int
    address: int
    file_size: int
    memory_size: int


@dataclass(frozen=True)
class Elf:
    """Static facts needed by the executable and library policy checks."""

    segments: tuple[Segment, ...]
    dynamic: dict[int, list[int]]
    needed: tuple[str, ...]
    interpreter: str | None
    audit_section: bytes | None


def program_headers(data: bytes, offset: int, count: int, entry_size: int) -> tuple[Segment, ...]:
    """Validate every file-backed segment before any virtual-address lookup."""
    require(entry_size == PROGRAM_HEADER.size and 0 < count <= MAX_TABLE, "elf_program_table")
    table = region(data, offset, count * entry_size)
    result: list[Segment] = []
    for raw in PROGRAM_HEADER.iter_unpack(table):
        kind, flags, start, address, _, file_size, memory_size, _ = cast("tuple[int, ...]", raw)
        require(file_size <= memory_size, "elf_segment_size")
        check_bounds(data, start, file_size)
        result.append(Segment(kind, flags, start, address, file_size, memory_size))
    return tuple(result)


def virtual_region(data: bytes, segments: tuple[Segment, ...], address: int, size: int) -> bytes:
    """Resolve metadata only through exactly one file-backed LOAD segment."""
    matches = [
        segment
        for segment in segments
        if segment.kind == PT_LOAD
        and segment.address <= address
        and address + size <= segment.address + segment.file_size
    ]
    require(len(matches) == 1, "elf_virtual_region")
    segment = matches[0]
    return region(data, segment.offset + address - segment.address, size)


def dynamic_table(data: bytes, segments: tuple[Segment, ...]) -> dict[int, list[int]]:
    """Read one terminated dynamic table with bounded entries and no ambiguity."""
    tables = [segment for segment in segments if segment.kind == PT_DYNAMIC]
    require(len(tables) == 1, "elf_dynamic_table_count")
    table = tables[0]
    require(
        0 < table.file_size <= MAX_TABLE * DYNAMIC_ENTRY.size
        and table.file_size % DYNAMIC_ENTRY.size == 0,
        "elf_dynamic_table_size",
    )
    result: dict[int, list[int]] = {}
    terminated = False
    for raw in DYNAMIC_ENTRY.iter_unpack(region(data, table.offset, table.file_size)):
        tag, value = cast("tuple[int, int]", raw)
        if terminated:
            require(tag == 0 and value == 0, "elf_dynamic_trailing_data")
        elif tag == 0:
            terminated = True
        else:
            result.setdefault(tag, []).append(value)
    require(terminated, "elf_dynamic_unterminated")
    for tag in (DT_STRTAB, DT_STRSZ, DT_FLAGS, DT_FLAGS_1, DT_BIND_NOW):
        require(len(result.get(tag, [])) <= 1, "elf_duplicate_dynamic_tag")
    return result


def audit_section(
    data: bytes, offset: int, count: int, size: int, names_index: int
) -> bytes | None:
    """Locate the one current cargo-auditable section without external parsers."""
    require(
        size == SECTION_HEADER.size and 0 < count <= MAX_TABLE and 0 <= names_index < count,
        "elf_section_table",
    )
    table = region(data, offset, count * size)
    sections = [cast("tuple[int, ...]", raw) for raw in SECTION_HEADER.iter_unpack(table)]
    strings = sections[names_index]
    require(strings[1] == SHT_STRTAB and 0 < strings[5] <= MAX_STRINGS, "elf_section_names_size")
    names = region(data, strings[4], strings[5])
    found: list[bytes] = []
    for section in sections:
        name, kind, _, _, start, length, _, _, _, _ = section
        if kind != SHT_NOBITS:
            check_bounds(data, start, length)
        if c_string(names, name) == ".dep-v0":
            require(kind == SHT_PROGBITS and 0 < length <= MAX_METADATA, "elf_audit_section_size")
            found.append(region(data, start, length))
    require(len(found) <= 1, "elf_audit_section_duplicate")
    return found[0] if found else None


def parse(data: bytes, platform: str) -> Elf:
    """Parse only supported ELF64 Linux metadata, never program instructions."""
    require(platform in PLATFORMS and ELF_HEADER.size <= len(data) <= MAX_BINARY, "elf_input")
    raw = ELF_HEADER.unpack(region(data, 0, ELF_HEADER.size))
    ident = cast("bytes", raw[0])
    fields = cast("tuple[int, ...]", raw[1:])
    (
        kind,
        machine,
        version,
        entrypoint,
        phoff,
        shoff,
        _,
        header_size,
        phsize,
        phcount,
        shsize,
        shcount,
        names,
    ) = fields
    require(
        ident[:7] == b"\x7fELF\x02\x01\x01"
        and ident[7] in (0, 3)
        and version == 1
        and kind == ET_DYN
        and machine == PLATFORMS[platform][0]
        and header_size == ELF_HEADER.size,
        "elf_header_identity",
    )
    segments = program_headers(data, phoff, phcount, phsize)
    dynamic = dynamic_table(data, segments)
    require(DT_STRTAB in dynamic and DT_STRSZ in dynamic, "elf_dynamic_strings_missing")
    strings_size = dynamic[DT_STRSZ][0]
    require(0 < strings_size <= MAX_STRINGS, "elf_dynamic_strings_size")
    strings = virtual_region(data, segments, dynamic[DT_STRTAB][0], strings_size)
    needed = tuple(c_string(strings, item) for item in dynamic.get(DT_NEEDED, []))
    require(
        len(needed) == len(set(needed))
        and all(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", name) for name in needed),
        "elf_needed_name",
    )
    interpreters = [segment for segment in segments if segment.kind == PT_INTERP]
    require(len(interpreters) <= 1, "elf_interpreter_count")
    interpreter = None
    if interpreters:
        require(
            any(
                segment.kind == PT_LOAD
                and segment.flags & PF_X
                and segment.address <= entrypoint < segment.address + segment.file_size
                for segment in segments
            ),
            "elf_entrypoint_not_executable",
        )
        segment = interpreters[0]
        encoded = region(data, segment.offset, segment.file_size)
        interpreter = c_string(encoded, 0)
        require(len(interpreter) + 1 == len(encoded), "elf_interpreter_trailing_data")
    return Elf(
        segments,
        dynamic,
        needed,
        interpreter,
        audit_section(data, shoff, shcount, shsize, names),
    )


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    """Reject duplicate metadata keys rather than selecting an interpretation."""
    result: dict[str, object] = {}
    for key, value in pairs:
        require(key not in result, "auditable_duplicate_key")
        result[key] = value
    return result


def dependency_metadata(section: bytes | None) -> dict[str, object]:
    """Validate bounded current cargo-auditable metadata and its dependency DAG."""
    require(section is not None, "auditable_metadata_missing")
    assert section is not None  # noqa: S101 -- Narrowing follows the explicit runtime guard.
    try:
        stream = zlib.decompressobj()
        decoded = stream.decompress(section, MAX_METADATA + 1)
        require(
            len(decoded) <= MAX_METADATA
            and stream.eof
            and not stream.unconsumed_tail
            and not stream.unused_data,
            "auditable_compression_bounds",
        )
        value = cast("object", json.loads(decoded, object_pairs_hook=unique_object))
    except (zlib.error, UnicodeError, json.JSONDecodeError, RecursionError) as error:
        reason = "auditable_metadata_invalid"
        raise ElfError(reason) from error
    require(isinstance(value, dict), "auditable_object")
    document = cast("dict[str, object]", value)
    require(set(document) <= {"packages", "format"}, "auditable_fields")
    require(type(document.get("format")) is int and document["format"] == 1, "auditable_format")
    packages_value = document.get("packages")
    require(isinstance(packages_value, list), "auditable_packages")
    packages = cast("list[object]", packages_value)
    require(0 < len(packages) <= MAX_PACKAGES, "auditable_package_count")
    edges: list[list[int]] = []
    roots: list[int] = []
    for index, package_value in enumerate(packages):
        require(isinstance(package_value, dict), "auditable_package_object")
        package = cast("dict[str, object]", package_value)
        require(
            {"name", "version", "source"} <= set(package)
            and set(package) <= {"name", "version", "source", "root", "kind", "dependencies"},
            "auditable_package_fields",
        )
        for key in ("name", "version", "source"):
            item = package[key]
            require(
                isinstance(item, str) and 0 < len(item) <= MAX_NAME, "auditable_package_identity"
            )
        require(
            re.fullmatch(r"[A-Za-z0-9_-]+", cast("str", package["name"]))
            and re.fullmatch(
                r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)*", cast("str", package["version"])
            )
            and package["source"] in ("local", "crates.io", "git", "registry"),
            "auditable_package_identity",
        )
        require(package.get("kind", "runtime") in ("build", "runtime"), "auditable_kind")
        require(type(package.get("root", False)) is bool, "auditable_root_type")
        if package.get("root") is True:
            require(
                package["name"] == "simplestChat" and package["source"] == "local", "auditable_root"
            )
            roots.append(index)
        dependencies_value = package.get("dependencies", [])
        require(isinstance(dependencies_value, list), "auditable_dependencies")
        dependencies = cast("list[object]", dependencies_value)
        require(
            all(type(item) is int and 0 <= item < len(packages) for item in dependencies),
            "auditable_dependency_index",
        )
        indices = cast("list[int]", dependencies)
        require(len(indices) == len(set(indices)), "auditable_dependency_duplicate")
        edges.append(indices)
    require(len(roots) == 1, "auditable_root_count")
    indegrees = [0] * len(packages)
    for dependencies in edges:
        for dependency in dependencies:
            indegrees[dependency] += 1
    ready = deque(index for index, degree in enumerate(indegrees) if degree == 0)
    visited = 0
    while ready:
        visited += 1
        for dependency in edges[ready.popleft()]:
            indegrees[dependency] -= 1
            if indegrees[dependency] == 0:
                ready.append(dependency)
    require(visited == len(packages), "auditable_dependency_cycle")
    return {
        "format": 1,
        "sha256": hashlib.sha256(decoded).hexdigest(),
        "compressedSha256": hashlib.sha256(section).hexdigest(),
        "packageCount": len(packages),
        "packages": packages,
    }


def hardening(elf: Elf, platform: str) -> dict[str, bool]:
    """Require all encoded executable hardening properties independently."""
    flags = elf.dynamic.get(DT_FLAGS, [0])[0]
    flags1 = elf.dynamic.get(DT_FLAGS_1, [0])[0]
    stacks = [segment for segment in elf.segments if segment.kind == PT_GNU_STACK]
    checks = {
        "pie": bool(flags1 & DF_1_PIE) and elf.interpreter == PLATFORMS[platform][1],
        "relro": any(
            segment.kind == PT_GNU_RELRO and segment.memory_size for segment in elf.segments
        ),
        "bindNow": DT_BIND_NOW in elf.dynamic or bool(flags & DF_BIND_NOW or flags1 & DF_1_NOW),
        "nonExecutableStack": len(stacks) == 1 and not bool(stacks[0].flags & PF_X),
        "noWritableExecutableLoad": not any(
            segment.kind == PT_LOAD and segment.flags & PF_X and segment.flags & PF_W
            for segment in elf.segments
        ),
        "noTextRelocations": DT_TEXTREL not in elf.dynamic and not bool(flags & DF_TEXTREL),
        "noRpath": DT_RPATH not in elf.dynamic and DT_RUNPATH not in elf.dynamic,
        "approvedLibraries": bool(elf.needed) and set(elf.needed) <= RUNTIME_LIBRARIES,
    }
    require(
        all(checks.values()),
        "elf_hardening:" + ",".join(name for name, ok in checks.items() if not ok),
    )
    return checks


def image_path(root: Path, name: str) -> Path:
    """Resolve image symlinks without allowing an absolute link to target the host."""
    require(name.startswith("/") and "\0" not in name, "image_path_invalid")
    pending = deque(PurePosixPath(name).parts[1:])
    resolved: list[str] = []
    links = 0
    while pending:
        part = pending.popleft()
        if part in ("", "."):
            continue
        if part == "..":
            require(bool(resolved), "image_path_escape")
            _ = resolved.pop()
            continue
        path = root.joinpath(*resolved, part)
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            links += 1
            require(links <= MAX_LINKS, "image_symlink_limit")
            target = PurePosixPath(path.readlink())
            if target.is_absolute():
                resolved = []
                parts = target.parts[1:]
            else:
                parts = target.parts
            pending.extendleft(reversed(parts))
        else:
            require(
                stat.S_ISDIR(info.st_mode) if pending else stat.S_ISREG(info.st_mode),
                "image_file_kind",
            )
            resolved.append(part)
    require(bool(resolved), "image_path_empty")
    return root.joinpath(*resolved)


def read_binary(path: Path) -> bytes:
    """Refuse links and special files independently of image path resolution."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_BINARY, "elf_file_size")
        data = source.read(MAX_BINARY + 1)
        require(len(data) == info.st_size, "elf_file_changed")
        return data


def library_path(root: Path, name: str) -> Path:
    """Resolve an approved soname using only the image's standard library paths."""
    for directory in LIBRARY_DIRECTORIES:
        try:
            return image_path(root, directory + "/" + name)
        except FileNotFoundError:
            continue
    reason = "elf_dependency_missing"
    raise ElfError(reason)


def audit(root: Path, platform: str = "linux/amd64") -> dict[str, object]:
    """Bind hardening, Cargo metadata and transitive dynamic libraries to image bytes."""
    require(root.is_dir() and not root.is_symlink(), "image_root_invalid")
    binary = image_path(root, "/app/simplestChat")
    data = read_binary(binary)
    executable = parse(data, platform)
    checks = hardening(executable, platform)
    metadata = dependency_metadata(executable.audit_section)
    interpreter = PLATFORMS[platform][1]
    queue = deque(
        [image_path(root, interpreter), *(library_path(root, name) for name in executable.needed)]
    )
    libraries: list[dict[str, object]] = []
    visited: set[Path] = set()
    allowed = RUNTIME_LIBRARIES | {PurePosixPath(interpreter).name}
    while queue:
        path = queue.popleft()
        if path in visited:
            continue
        visited.add(path)
        content = read_binary(path)
        elf = parse(content, platform)
        require(set(elf.needed) <= allowed, "elf_transitive_dependency_forbidden")
        require(DT_RPATH not in elf.dynamic and DT_RUNPATH not in elf.dynamic, "elf_library_rpath")
        libraries.append(
            {
                "path": "/" + path.relative_to(root).as_posix(),
                "sha256": hashlib.sha256(content).hexdigest(),
                "bytes": len(content),
                "needed": list(elf.needed),
            }
        )
        queue.extend(library_path(root, name) for name in elf.needed)
    return {
        "passed": True,
        "platform": platform,
        "binary": "/app/simplestChat",
        "binarySha256": hashlib.sha256(data).hexdigest(),
        "binaryBytes": len(data),
        "checks": checks,
        "auditable": metadata,
        "libraries": libraries,
    }


def main(argv: list[str] | None = None) -> int:
    """Write one new evidence file; malformed input always exits unsuccessfully."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--rootfs", type=Path, required=True)
    _ = parser.add_argument("--platform", choices=tuple(PLATFORMS), default="linux/amd64")
    _ = parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    root, platform, output = (
        cast("Path", args.rootfs),
        cast("str", args.platform),
        cast("Path", args.output),
    )
    try:
        report = audit(root, platform)
    except (ElfError, OSError) as error:
        reason = str(error) if isinstance(error, ElfError) else "elf_filesystem_error"
        report = {"passed": False, "reason": reason}
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as destination:
        _ = destination.write(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] is True else 1


if __name__ == "__main__":
    sys.exit(main())
