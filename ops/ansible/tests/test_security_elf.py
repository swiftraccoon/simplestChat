"""Small inert ELF fixtures exercise hardening, parser bounds and image isolation."""

from __future__ import annotations

import hashlib
import json
import struct
import tempfile
import unittest
import zlib
from dataclasses import replace
from pathlib import Path
from typing import cast
from unittest.mock import patch

from test_support import ROOT, obj, objects, string

# isort: split

import security_elf as elf
from release_json import decode_json, object_value

FILE_SIZE = 4096
BASE = 0x400000
DYNAMIC_OFFSET = 512
STRINGS_OFFSET = 1024
INTERPRETER_OFFSET = 1280
AUDIT_OFFSET = 1408
NAMES_OFFSET = 1792
SECTIONS_OFFSET = 2048
PROGRAM_COUNT = 5
SECTION_COUNT = 3
EXECUTABLE_FLAGS = 5


def metadata() -> dict[str, object]:
    """Represent the selected current production graph, with one local root."""
    return {
        "format": 1,
        "packages": [
            {
                "name": "simplestChat",
                "version": "0.1.0",
                "source": "local",
                "root": True,
                "dependencies": [1],
            },
            {"name": "fixture", "version": "1.0.0", "source": "crates.io"},
        ],
    }


def fixture(*, library: bool = False, needed: tuple[str, ...] | None = None) -> bytes:
    """Construct metadata-only ELF bytes; no executable program is present."""
    data = bytearray(FILE_SIZE)
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    data[: elf.ELF_HEADER.size] = elf.ELF_HEADER.pack(
        ident,
        elf.ET_DYN,
        62,
        1,
        BASE,
        elf.ELF_HEADER.size,
        SECTIONS_OFFSET,
        0,
        elf.ELF_HEADER.size,
        elf.PROGRAM_HEADER.size,
        PROGRAM_COUNT,
        elf.SECTION_HEADER.size,
        SECTION_COUNT,
        1,
    )
    dependencies = needed if needed is not None else (() if library else ("libc.so.6",))
    strings = b"\0" + b"".join(name.encode("ascii") + b"\0" for name in dependencies)
    entries = [
        (elf.DT_STRTAB, BASE + STRINGS_OFFSET),
        (elf.DT_STRSZ, len(strings)),
        (elf.DT_FLAGS_1, elf.DF_1_NOW | elf.DF_1_PIE),
    ]
    offset = 1
    for name in dependencies:
        entries.append((elf.DT_NEEDED, offset))
        offset += len(name) + 1
    entries.append((0, 0))
    dynamic = b"".join(elf.DYNAMIC_ENTRY.pack(*entry) for entry in entries)
    interpreter = b"/lib64/ld-linux-x86-64.so.2\0"
    programs = [
        (elf.PT_LOAD, EXECUTABLE_FLAGS, 0, BASE, 0, FILE_SIZE, FILE_SIZE, FILE_SIZE),
        (
            elf.PT_DYNAMIC,
            4,
            DYNAMIC_OFFSET,
            BASE + DYNAMIC_OFFSET,
            0,
            len(dynamic),
            len(dynamic),
            8,
        ),
        (
            0 if library else elf.PT_INTERP,
            4,
            INTERPRETER_OFFSET,
            BASE + INTERPRETER_OFFSET,
            0,
            len(interpreter),
            len(interpreter),
            1,
        ),
        (elf.PT_GNU_STACK, 6, 0, 0, 0, 0, 0, 8),
        (
            elf.PT_GNU_RELRO,
            4,
            DYNAMIC_OFFSET,
            BASE + DYNAMIC_OFFSET,
            0,
            len(dynamic),
            len(dynamic),
            8,
        ),
    ]
    for index, header in enumerate(programs):
        start = elf.ELF_HEADER.size + index * elf.PROGRAM_HEADER.size
        data[start : start + elf.PROGRAM_HEADER.size] = elf.PROGRAM_HEADER.pack(*header)
    data[DYNAMIC_OFFSET : DYNAMIC_OFFSET + len(dynamic)] = dynamic
    data[STRINGS_OFFSET : STRINGS_OFFSET + len(strings)] = strings
    data[INTERPRETER_OFFSET : INTERPRETER_OFFSET + len(interpreter)] = interpreter
    compressed = zlib.compress(json.dumps(metadata()).encode())
    data[AUDIT_OFFSET : AUDIT_OFFSET + len(compressed)] = compressed
    names = b"\0.shstrtab\0.dep-v0\0"
    data[NAMES_OFFSET : NAMES_OFFSET + len(names)] = names
    sections = [
        (0,) * 10,
        (1, 3, 0, 0, NAMES_OFFSET, len(names), 0, 0, 1, 0),
        (11, elf.SHT_PROGBITS, 0, 0, AUDIT_OFFSET, len(compressed), 0, 0, 1, 0),
    ]
    for index, header in enumerate(sections):
        start = SECTIONS_OFFSET + index * elf.SECTION_HEADER.size
        data[start : start + elf.SECTION_HEADER.size] = elf.SECTION_HEADER.pack(*header)
    return bytes(data)


def alter(data: bytes, offset: int, format_string: str, value: int) -> bytes:
    """Change one encoded field, retaining all unrelated fixture evidence."""
    result = bytearray(data)
    struct.pack_into(format_string, result, offset, value)
    return bytes(result)


def rootfs(directory: Path) -> None:
    """Create a tiny image tree with Fedora-style absolute and relative links."""
    (directory / "app").mkdir()
    (directory / "usr/lib64").mkdir(parents=True)
    (directory / "lib64").symlink_to("usr/lib64")
    _ = (directory / "app/simplestChat").write_bytes(fixture())
    _ = (directory / "usr/lib64/ld-linux-x86-64.so.2").write_bytes(fixture(library=True))
    _ = (directory / "usr/lib64/libc-real.so.6").write_bytes(fixture(library=True))
    (directory / "usr/lib64/libc.so.6").symlink_to("/usr/lib64/libc-real.so.6")


class ElfSecurityTests(unittest.TestCase):
    """Unsupported, malformed and incomplete inputs never count as hardened."""

    def test_exact_binary_and_image_libraries_are_bound_to_hashes(self) -> None:
        """Successful evidence includes independent hardening and actual dependency bytes."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            rootfs(directory)
            report = elf.audit(directory)
            self.assertTrue(report["passed"])
            self.assertEqual(report["binaryBytes"], FILE_SIZE)
            self.assertEqual(len(cast("list[object]", report["libraries"])), 2)
            self.assertIn("/usr/lib64/libc-real.so.6", json.dumps(report))
            self.assertIn("compressedSha256", json.dumps(report))

    def test_all_hardening_flags_are_required_independently(self) -> None:
        """A single lost protection fails even when the other flags remain valid."""
        good = elf.parse(fixture(), "linux/amd64")
        self.assertTrue(all(elf.hardening(good, "linux/amd64").values()))
        for key, value, expected in (
            (elf.DT_FLAGS_1, [elf.DF_1_NOW], "pie"),
            (elf.DT_FLAGS_1, [elf.DF_1_PIE], "bindNow"),
            (elf.DT_RPATH, [0], "noRpath"),
            (elf.DT_RUNPATH, [0], "noRpath"),
            (elf.DT_TEXTREL, [0], "noTextRelocations"),
            (elf.DT_FLAGS, [elf.DF_TEXTREL], "noTextRelocations"),
        ):
            with (
                self.subTest(expected=expected),
                patch.dict(good.dynamic, {key: value}),
                self.assertRaisesRegex(elf.ElfError, expected),
            ):
                _ = elf.hardening(good, "linux/amd64")
        for segment_kind, replacement, expected in (
            (elf.PT_GNU_STACK, elf.PF_X, "nonExecutableStack"),
            (elf.PT_LOAD, elf.PF_W | elf.PF_X, "noWritableExecutableLoad"),
        ):
            segments = tuple(
                elf.Segment(
                    item.kind,
                    replacement if item.kind == segment_kind else item.flags,
                    item.offset,
                    item.address,
                    item.file_size,
                    item.memory_size,
                )
                for item in good.segments
            )
            changed = elf.Elf(
                segments, good.dynamic, good.needed, good.interpreter, good.audit_section
            )
            with self.subTest(expected=expected), self.assertRaisesRegex(elf.ElfError, expected):
                _ = elf.hardening(changed, "linux/amd64")
        changed = elf.Elf(
            tuple(item for item in good.segments if item.kind != elf.PT_GNU_RELRO),
            good.dynamic,
            good.needed,
            good.interpreter,
            good.audit_section,
        )
        with self.assertRaisesRegex(elf.ElfError, "relro"):
            _ = elf.hardening(changed, "linux/amd64")

    def test_input_headers_tables_and_dynamic_strings_are_bounded(self) -> None:
        """Sizes and offsets are checked before unpacking or virtual-address resolution."""
        changes = [
            b"not an ELF",
            fixture()[:100],
            alter(fixture(), 18, "<H", 183),
            alter(fixture(), 32, "<Q", FILE_SIZE),
            alter(fixture(), 56, "<H", 0),
            alter(fixture(), DYNAMIC_OFFSET + 8, "<Q", BASE + FILE_SIZE),
            alter(
                fixture(), DYNAMIC_OFFSET + elf.DYNAMIC_ENTRY.size + 8, "<Q", elf.MAX_STRINGS + 1
            ),
        ]
        for data in changes:
            with self.subTest(bytes=len(data)), self.assertRaises(elf.ElfError):
                _ = elf.parse(data, "linux/amd64")

    def test_dependency_metadata_must_be_present_current_bounded_and_unambiguous(self) -> None:
        """Do not fall back to guessed Rust dependencies when the section is absent."""
        valid = elf.dependency_metadata(elf.parse(fixture(), "linux/amd64").audit_section)
        self.assertEqual(valid["packageCount"], 2)
        cases = [
            None,
            b"not zlib",
            zlib.compress(b"{}"),
            zlib.compress(b'{"format":1,"format":1,"packages":[]}'),
            zlib.compress(json.dumps(metadata()).encode()) + b"extra",
        ]
        for content in cases:
            with self.subTest(content=content is None), self.assertRaises(elf.ElfError):
                _ = elf.dependency_metadata(content)
        with (
            patch.object(elf, "MAX_METADATA", 8),
            self.assertRaisesRegex(elf.ElfError, "compression_bounds"),
        ):
            _ = elf.dependency_metadata(zlib.compress(b"x" * 9))

    def test_dependency_graph_rejects_invalid_roots_indices_and_cycles(self) -> None:
        """Graph traversal is iterative and cannot accept malformed relationships."""
        for packages in (
            [{"name": "simplestChat", "version": "0.1.0", "source": "local"}],
            [{"name": "other", "version": "0.1.0", "source": "local", "root": True}],
            [
                {
                    "name": "simplestChat",
                    "version": "0.1.0",
                    "source": "local",
                    "root": True,
                    "dependencies": [0],
                }
            ],
            [
                {
                    "name": "simplestChat",
                    "version": "0.1.0",
                    "source": "local",
                    "root": True,
                    "dependencies": [True],
                }
            ],
        ):
            with self.subTest(packages=packages), self.assertRaises(elf.ElfError):
                _ = elf.dependency_metadata(
                    zlib.compress(json.dumps({"format": 1, "packages": packages}).encode())
                )

    def test_duplicate_sections_unterminated_tags_and_needed_paths_are_rejected(self) -> None:
        """An otherwise sound executable cannot carry ambiguous metadata or path dependencies."""
        duplicated = bytearray(alter(fixture(), 60, "<H", SECTION_COUNT + 1))
        original = SECTIONS_OFFSET + (SECTION_COUNT - 1) * elf.SECTION_HEADER.size
        duplicate = original + elf.SECTION_HEADER.size
        duplicated[duplicate : duplicate + elf.SECTION_HEADER.size] = duplicated[
            original : original + elf.SECTION_HEADER.size
        ]
        with self.assertRaisesRegex(elf.ElfError, "audit_section_duplicate"):
            _ = elf.parse(bytes(duplicated), "linux/amd64")
        terminated_tag_offset = DYNAMIC_OFFSET + 4 * elf.DYNAMIC_ENTRY.size
        with self.assertRaisesRegex(elf.ElfError, "dynamic_unterminated"):
            _ = elf.parse(
                alter(fixture(), terminated_tag_offset, "<q", elf.DT_NEEDED), "linux/amd64"
            )
        path_dependency = bytearray(fixture())
        path_dependency[STRINGS_OFFSET : STRINGS_OFFSET + len(b"\0../bad.so\0")] = b"\0../bad.so\0"
        with self.assertRaisesRegex(elf.ElfError, "needed_name"):
            _ = elf.parse(bytes(path_dependency), "linux/amd64")

    def test_wrong_interpreter_and_dynamic_crypto_runtime_fail_policy(self) -> None:
        """Platform and static-library assumptions are explicit release requirements."""
        parsed = elf.parse(fixture(), "linux/amd64")
        for changed in (
            replace(parsed, interpreter="/unexpected/ld-fixture.so"),
            replace(parsed, needed=("libssl.so.3",)),
            replace(parsed, needed=("libcrypto.so.3",)),
            replace(parsed, needed=("libstdc++.so.6",)),
        ):
            with self.subTest(needed=changed.needed), self.assertRaises(elf.ElfError):
                _ = elf.hardening(changed, "linux/amd64")

    def test_current_build_uses_one_pinned_auditable_tool_without_path_fallback(self) -> None:
        """Both cache warmup and final production build retain the audited build command."""
        dockerfile = (ROOT / "Dockerfile").read_text()
        invocation = (
            "/opt/security-tools/bin/cargo-auditable auditable build"
            + " --locked --release --bin simplestChat"
        )
        self.assertEqual(dockerfile.count(invocation), 2)
        self.assertIn("security_tools.py install --tools cargo-auditable", dockerfile)
        self.assertIn("security_tools.py path cargo-auditable", dockerfile)
        self.assertNotIn("cargo build --locked --release --bin simplestChat", dockerfile)

    def test_failed_library_policy_retains_bounded_identities_without_relaxing_policy(self) -> None:
        """A failing real parser path names conventional SONAMEs in private evidence."""
        needed = ("libc.so.6", "libstdc++.so.6", "ld-linux-x86-64.so.2")
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            rootfs(directory)
            _ = (directory / "app/simplestChat").write_bytes(fixture(needed=needed))
            output = directory / "report.json"
            self.assertEqual(elf.main(["--rootfs", str(directory), "--output", str(output)]), 1)
            report = object_value(decode_json(output.read_bytes()))
            self.assertFalse(report["passed"])
            self.assertEqual(report["reason"], "elf_hardening:approvedLibraries")
            evidence = obj(report, "dynamicDependencies")
            self.assertEqual(evidence["count"], len(needed))
            self.assertEqual(evidence["omitted"], 0)
            self.assertEqual(
                {string(row, "name"): row["approved"] for row in objects(evidence, "identities")},
                {"libc.so.6": True, "libstdc++.so.6": False, "ld-linux-x86-64.so.2": False},
            )
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_dependency_diagnostics_hide_unusual_strings_and_bound_complete_identity(self) -> None:
        """Only a small conventional SONAME set is printed; other bytes are hashed."""
        private = "NeverPublishFixtureCredentialValue.so"
        evidence = elf.dependency_diagnostics((private,))
        self.assertNotIn(private, json.dumps(evidence))
        self.assertEqual(
            evidence["identities"],
            [
                {
                    "name": None,
                    "sha256": hashlib.sha256(private.encode()).hexdigest(),
                    "approved": False,
                }
            ],
        )
        needed = tuple(
            f"libfixture{index}.so.1" for index in range(elf.MAX_DIAGNOSTIC_DEPENDENCIES + 2)
        )
        evidence = elf.dependency_diagnostics(needed)
        self.assertEqual(evidence["count"], len(needed))
        self.assertEqual(evidence["omitted"], 2)
        self.assertEqual(
            len(cast("list[object]", evidence["identities"])), elf.MAX_DIAGNOSTIC_DEPENDENCIES
        )
        self.assertEqual(
            evidence["sha256"],
            hashlib.sha256(b"\0".join(name.encode() for name in needed)).hexdigest(),
        )
        self.assertLess(len(json.dumps(evidence)), 8192)

    def test_invalid_dependency_strings_never_enter_failure_evidence(self) -> None:
        """Parser rejection precedes diagnostics for paths or log-control characters."""
        for name in ("../private-fixture.so", "libprivate\nfixture.so"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                rootfs(directory)
                _ = (directory / "app/simplestChat").write_bytes(fixture(needed=(name,)))
                output = directory / "report.json"
                self.assertEqual(elf.main(["--rootfs", str(directory), "--output", str(output)]), 1)
                self.assertEqual(
                    json.loads(output.read_text()), {"passed": False, "reason": "elf_needed_name"}
                )

    def test_image_paths_cannot_follow_host_files_or_special_resources(self) -> None:
        """Absolute symlinks are rooted in the image, and escapes/cycles fail closed."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "host").symlink_to(ROOT / "Cargo.toml")
            with self.assertRaises(FileNotFoundError):
                _ = elf.image_path(directory, "/host")
            (directory / "escape").symlink_to("../outside")
            with self.assertRaisesRegex(elf.ElfError, "escape"):
                _ = elf.image_path(directory, "/escape")
            (directory / "loop").symlink_to("loop")
            with self.assertRaisesRegex(elf.ElfError, "symlink_limit"):
                _ = elf.image_path(directory, "/loop")
            with self.assertRaises(OSError):
                _ = elf.read_binary(directory / "host")

    def test_missing_library_is_failure_and_output_is_new_private_file(self) -> None:
        """A host-installed libc cannot substitute for missing image evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            rootfs(directory)
            (directory / "usr/lib64/libc-real.so.6").unlink()
            output = directory / "report.json"
            self.assertEqual(elf.main(["--rootfs", str(directory), "--output", str(output)]), 1)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertFalse(json.loads(output.read_text())["passed"])
            with self.assertRaises(FileExistsError):
                _ = elf.main(["--rootfs", str(directory), "--output", str(output)])


if __name__ == "__main__":
    _ = unittest.main()
