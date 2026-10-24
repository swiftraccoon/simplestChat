"""Run unchanged cached verification with real files and inert verifier observations.

Only the maintained verifier subprocess and UTC source are adapted. The original
ZIP, archive, trust and final receiver functions decide every result. Fake
verifier output never establishes cryptographic proof. Exported input bytes are
pristine; the actual reference runs on a separate private candidate.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import io
import json
import os
import stat
import struct
import tempfile
import warnings
import zipfile
from dataclasses import dataclass
from datetime import UTC, datetime, tzinfo
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, final, override
from unittest.mock import patch
from zlib import crc32

from test_release_attestation import verified_output
from test_release_fetch import envelope, fixture_files
from test_support import ROOT, array, obj

# isort: split
import bounded_process
import release_attestation as attest
import release_fetch_receiver as receiver
import release_trust as trust
from release_json import JsonObject, JsonValue, decode_json, integer_value

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence


_ = ROOT
TODAY = "2050-01-01"
MAX_FIXTURE_BYTES = 2 * 1024**2
MAX_OUTPUT_BYTES = 131072
SELECTED = (*trust.BOUND_FILES, trust.PREDICATE, trust.BUNDLE)
ENVIRONMENT = {
    "PATH": "$FIXTURE/bin",
    "HOME": "$FIXTURE/home",
    "USER": "fixture",
    "LOGNAME": "fixture",
    "GH_TOKEN": "INERT_GH_TOKEN",
    "GITHUB_TOKEN": "INERT_GITHUB_TOKEN",
    "XDG_CONFIG_HOME": "$FIXTURE/config",
    "LC_ALL": "C",
}
CERTIFICATE_KEYS = (
    "subjectAlternativeName",
    "issuer",
    "buildSignerDigest",
    "sourceRepositoryURI",
    "sourceRepositoryDigest",
    "sourceRepositoryRef",
    "runInvocationURI",
    "runnerEnvironment",
    "buildTrigger",
)
SCENARIOS = (
    "success",
    "repeat-fresh",
    "required",
    "attempt-large",
    "identifiers-max",
    "optional-utc-error",
    "required-utc-error",
    "late-utc-error",
    "late-expiry",
    "expiry-today",
    "expiry-yesterday",
    "expiry-invalid",
    "expiry-unicode",
    "runtime-required-integer",
    "runtime-schema-boolean",
    "runtime-passed-integer",
    "runtime-image",
    "runtime-required-mismatch",
    "runtime-hash",
    "vex-hash",
    "required-scope",
    "required-evidence",
    "required-vex-empty",
    "optional-vex-nonempty",
    "security-passed-integer",
    "security-image",
    "security-sbom",
    "build-passed",
    "build-image",
    "manifest-archive",
    "predicate-schema-boolean",
    "predicate-extra",
    "predicate-digest",
    "predicate-attempt",
    "encoding-predicate-utf16",
    "encoding-security-bom",
    "ignored-large-integer",
    "ignored-bool-null",
    "late-source",
    "late-outcome-error",
    "late-manifest-date",
    "late-archive",
    "gh-failure-first",
    "gh-failure-second",
    "gh-invalid-json",
    "gh-duplicate-json",
    "gh-empty",
    "gh-multiple",
    "gh-no-timestamp",
    "gh-statement-type",
    "gh-predicate-type",
    "gh-claim",
    "gh-subject-missing",
    "gh-subject-duplicate",
    "gh-subject-extra",
    "gh-subject-reordered",
    "gh-claim-schema-bool",
    "gh-claim-run-float",
    "gh-large-integer",
    "gh-utf16",
    "gh-surrogate",
    *(f"gh-certificate-{key}" for key in CERTIFICATE_KEYS),
    "mutate-bundle",
    "mutate-source",
    "mutate-outcome",
    "mutate-manifest",
    "mutate-zip",
    "exchange-bundle",
    "zip-store",
    "zip-store-trailing",
    "zip-deflate-extra-output",
    "zip-comment",
    "zip-local-zip64",
    "zip-central-zip64",
    "zip-extra-opaque",
    "zip-extra-bad-crc",
    "zip-directory-data",
    "zip-prefix-files",
    "zip-large-central-record",
    "zip-local-metadata-diff",
    "zip-local-flags-diff",
    "zip-selected-patched-flag",
    "zip-selected-strong-flag",
    "zip-unselected-flags",
    "zip-shared-offset-extra-first",
    "zip-shared-offset-extra-last",
    "zip-shared-offset-wide-extent",
    "zip-eocd-count-diff",
    "zip-selected-crc",
    "zip-local-name",
    "zip-duplicate",
    "zip-traversal",
    "zip-backslash",
    "zip-missing",
    "zip-empty-selected",
    "zip-bad-comment",
    "zip-eocd-zip64",
    "zip-trailing",
    "zip-symlink",
    "zip-method",
    "zip-encrypted-extra",
    "zip-mode",
    "zip-unicode-override",
)
PROFILE = {
    "mutate-zip": "retained-originals",
    "exchange-bundle": "original-inode",
    "gh-surrogate": "unicode-scalar",
}


def encoded(value: JsonValue) -> bytes:
    """Keep ordinary original JSON tokens without lossy numeric conversion."""
    return json.dumps(value, ensure_ascii=True).encode()


def digest(data: bytes) -> str:
    """Pin original fixture bytes."""
    return hashlib.sha256(data).hexdigest()


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create one fresh private fixture, with no replacement or shell action."""
    with path.open("xb") as output:
        _ = output.write(data)
    path.chmod(mode)


@final
class FixedDateTime(datetime):
    """Preserve real calendar parsing while controlling only fresh UTC samples."""

    calls: ClassVar[int] = 0
    clock: ClassVar[str] = "fixed"

    @classmethod
    @override
    def now(cls, tz: tzinfo | None = None) -> FixedDateTime:
        cls.calls += 1
        if cls.clock == "error" or (cls.clock == "late-error" and cls.calls > 1):
            message = "inert UTC source failure"
            raise OSError(message)
        day = 2 if cls.clock == "late-expiry" and cls.calls > 1 else 1
        return cls(2050, 1, day, tzinfo=UTC).astimezone(tz)


def conditional_cases(
    scenario: str, security: JsonObject, proof: JsonObject, vex: JsonObject
) -> None:
    """Select one complete conditional proof and its closed expiry/disposition case."""
    required = scenario.startswith(("required", "expiry", "late-utc", "late-expiry"))
    if required:
        security["vexRequired"] = True
        proof.update(required=True, scope="managed-default-server-dtls")
        proof["sourceReview"] = {"expires": "2050-01-02"}
        for key in ("binarySha256", "nativeSha256", "elfSha256", "sbomSha256"):
            proof[key] = security.setdefault(key, "b" * 64)
        vex["statements"] = [{"status": "not_affected"}]
    if scenario.startswith("expiry-"):
        obj(proof, "sourceReview")["expires"] = {
            "expiry-today": "2050-01-01",
            "expiry-yesterday": "2049-12-31",
            "expiry-invalid": "2050-02-30",
            "expiry-unicode": "\uff12\uff10\uff15\uff10-01-02",
        }[scenario]
    if scenario in {"required-vex-empty", "optional-vex-nonempty"}:
        vex["statements"] = [] if required else [{"status": "not_affected"}]


def metadata_cases(scenario: str, files: dict[str, bytes], selected: JsonObject) -> JsonObject:
    """Bind selected mutations so whole trust guards, rather than stale hashes, decide."""
    security, proof = (
        obj(decode_json(files["image-security.json"])),
        obj(decode_json(files["runtime-proof.json"])),
    )
    outcome, manifest = (
        obj(decode_json(files["outcome.json"])),
        obj(decode_json(files["release.json"])),
    )
    vex = obj(decode_json(files["vex.openvex.json"]))
    conditional_cases(scenario, security, proof, vex)
    if scenario == "attempt-large":
        selected["runAttempt"] = 2**31
    if scenario == "identifiers-max":
        for key in ("artifactId", "buildRunId", "ciRunId", "runAttempt"):
            selected[key] = 2**53 - 1
    changes: dict[str, tuple[JsonObject, str, JsonValue]] = {
        "runtime-required-integer": (security, "vexRequired", 0),
        "runtime-schema-boolean": (proof, "schemaVersion", True),
        "runtime-passed-integer": (proof, "passed", 1),
        "runtime-image": (proof, "imageId", "sha256:" + "1" * 64),
        "runtime-required-mismatch": (proof, "required", True),
        "required-scope": (proof, "scope", "all-programs"),
        "required-evidence": (proof, "binarySha256", "1" * 64),
        "security-passed-integer": (security, "passed", 1),
        "security-image": (security, "imageId", "sha256:" + "1" * 64),
        "build-passed": (outcome, "passed", False),
        "build-image": (outcome, "exportedImageId", "sha256:" + "1" * 64),
        "manifest-archive": (manifest, "archiveSha256", "1" * 64),
        "late-outcome-error": (outcome, "error", "inert failure"),
        "late-manifest-date": (manifest, "createdAt", "not-a-UTC-date"),
    }
    if scenario in changes:
        record, key, value = changes[scenario]
        record[key] = value
    if scenario == "late-source":
        files["source.json"] = b'{"revision":"bad","inputsSha256":{}}'
    if scenario == "late-archive":
        files["image.tar"] = b"not a tar archive; still exactly hashed"
        checksum = digest(files["image.tar"])
        manifest["archiveSha256"] = checksum
        security["archiveSha256"] = checksum
        proof["archiveSha256"] = checksum
    files["outcome.json"], files["release.json"] = encoded(outcome), encoded(manifest)
    files["runtime-proof.json"], files["vex.openvex.json"] = encoded(proof), encoded(vex)
    security["runtimeProofSha256"] = digest(files["runtime-proof.json"])
    security["vexSha256"] = digest(files["vex.openvex.json"])
    security["sbomSha256"] = digest(files["sbom.spdx.json"])
    for case, key in (
        ("runtime-hash", "runtimeProofSha256"),
        ("vex-hash", "vexSha256"),
        ("security-sbom", "sbomSha256"),
    ):
        if scenario == case:
            security[key] = "1" * 64
    files["image-security.json"] = security_bytes(security, scenario)
    return claim_cases(scenario, files, selected)


def security_bytes(security: JsonObject, scenario: str) -> bytes:
    """Preserve ignored tokens and an explicit accepted byte encoding."""
    if scenario == "ignored-large-integer":
        security["ignored"] = 2**180 + 1
    if scenario == "ignored-bool-null":
        security["ignored"] = [True, False, None, 9007199254740993]
    data = encoded(security)
    if scenario == "encoding-security-bom":
        data = b"\xef\xbb\xbf" + data
    return data


def claim_cases(scenario: str, files: dict[str, bytes], selected: JsonObject) -> JsonObject:
    """Authenticate final fixture bytes, then apply a selected malformed claim."""
    claim = obj(decode_json(files[trust.PREDICATE]))
    claim["runId"], claim["runAttempt"] = selected["ciRunId"], selected["runAttempt"]
    claim["fileDigests"] = {name: digest(files[name]) for name in trust.BOUND_FILES}
    if scenario == "predicate-schema-boolean":
        claim["schemaVersion"] = True
    if scenario == "predicate-extra":
        claim["extra"] = None
    if scenario == "predicate-digest":
        obj(claim, "fileDigests")["source.json"] = "1" * 64
    if scenario == "predicate-attempt":
        claim["runAttempt"] = 2
    files[trust.PREDICATE] = encoded(claim)
    if scenario == "encoding-predicate-utf16":
        files[trust.PREDICATE] = files[trust.PREDICATE].decode().encode("utf-16")
    return claim


def central_records(raw: bytearray) -> list[int]:
    """Locate actual fixture central records using their encoded lengths."""
    end = raw.rfind(b"PK\x05\x06")
    position = struct.unpack_from("<L", raw, end + 16)[0]
    offsets: list[int] = []
    while bytes(raw[position : position + 4]) == b"PK\x01\x02":
        offsets.append(position)
        position += 46 + sum(struct.unpack_from("<HHH", raw, position + 28))
    return offsets


def zip_info(name: str, archive: zipfile.ZipFile) -> zipfile.ZipInfo:
    """Retain mature string-writer defaults with a fixed inert fixture date."""
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = archive.compression
    info.compress_level = archive.compresslevel
    if info.filename.endswith("/"):
        info.external_attr = (0o40775 << 16) | 0x10
    else:
        info.external_attr = 0o600 << 16
    return info


def zip_original(files: dict[str, bytes], scenario: str) -> bytearray:
    """Use mature ZIP creation, then change only the named closed header fixture."""
    output = io.BytesIO()
    compression = (
        zipfile.ZIP_STORED
        if scenario in {"zip-store", "zip-store-trailing"}
        else zipfile.ZIP_DEFLATED
    )
    with zipfile.ZipFile(output, "w", compression=compression) as archive:
        for name, original in files.items():
            data = original
            if name == "image.tar" and scenario in {
                "zip-store-trailing",
                "zip-deflate-extra-output",
            }:
                data += b"unselected inflated suffix"
            if scenario == "zip-missing" and name == "source.json":
                continue
            if scenario == "zip-empty-selected" and name == "source.json":
                data = b""
            if scenario == "zip-local-zip64":
                with archive.open(name, "w", force_zip64=True) as member:
                    _ = member.write(data)
            else:
                archive.writestr(zip_info(name, archive), data)
        zip_extras(archive, scenario)
    return bytearray(output.getvalue())


def zip_extras(archive: zipfile.ZipFile, scenario: str) -> None:
    """Create only the closed unselected metadata and payload fixtures."""
    extras = {
        "zip-extra-opaque": (("unselected.bin", b"\x00\xffopaque"),),
        "zip-extra-bad-crc": (("unselected.bin", b"unread"),),
        "zip-unselected-flags": (("unselected.bin", b"unread flags"),),
        "zip-directory-data": (("notes/", b"directory payload is not selected"),),
        "zip-prefix-files": (("foo", b"x"), ("foo/bar", b"y")),
        "zip-duplicate": (("foo", b"x"), ("foo/", b"y")),
        "zip-traversal": (("../bad", b"x"),),
        "zip-backslash": (("foo\\bar", b"x"),),
    }
    for name, data in extras.get(scenario, ()):
        archive.writestr(zip_info(name, archive), data)
    if scenario == "zip-comment":
        archive.comment = b"ordinary bounded archive comment"
    if scenario == "zip-large-central-record":
        info = zipfile.ZipInfo("n" * 65535)
        info.extra = struct.pack("<HH", 0xCAFE, 65531) + b"x" * 65531
        info.comment = b"c" * 65535
        archive.writestr(info, b"")
    if scenario in {"zip-symlink", "zip-method", "zip-encrypted-extra", "zip-mode"}:
        info = zipfile.ZipInfo("unselected.bin")
        info.create_system = 3
        info.external_attr = (stat.S_IFLNK if scenario == "zip-symlink" else stat.S_IFDIR) << 16
        if scenario in {"zip-method", "zip-encrypted-extra"}:
            info.external_attr = stat.S_IFREG << 16
        archive.writestr(info, b"x")
    if scenario == "zip-unicode-override":
        name = b"original"
        info = zipfile.ZipInfo(name.decode())
        field = b"\x01" + struct.pack("<L", crc32(name)) + b"different"
        info.extra = struct.pack("<HH", 0x7075, len(field)) + field
        archive.writestr(info, b"x")
    if scenario == "zip-central-zip64":
        # A member-level ZIP64 central extra with ordinary EOCD metadata.
        info = zipfile.ZipInfo("unselected.bin")
        info.extra = struct.pack("<HHQQ", 1, 16, 1, 1)
        archive.writestr(info, b"x")


def zip_bytes(files: dict[str, bytes], scenario: str) -> bytes:
    """Modify only the named mature-writer header, without introducing a ZIP parser."""
    raw = zip_original(files, scenario)
    records = central_records(raw)
    first, last = records[0], records[-1]
    end = raw.rfind(b"PK\x05\x06")
    changes: dict[str, tuple[tuple[str, int, tuple[int, ...]], ...]] = {
        "zip-selected-crc": (("<L", first + 16, (1,)),),
        "zip-extra-bad-crc": (("<L", last + 16, (1,)),),
        "zip-local-metadata-diff": (("<H", 8, (99,)), ("<LLL", 14, (0x12345678, 1, 2))),
        "zip-local-flags-diff": (("<H", 6, (0x860,)),),
        "zip-selected-patched-flag": (("<H", first + 8, (0x20,)),),
        "zip-selected-strong-flag": (("<H", first + 8, (0x40,)),),
        "zip-unselected-flags": (("<H", last + 8, (0x60,)),),
        "zip-eocd-count-diff": (("<HH", end + 8, (1, 1)),),
        "zip-bad-comment": (("<H", end + 20, (1,)),),
        "zip-eocd-zip64": (("<L", end + 16, (0xFFFFFFFF,)),),
        "zip-method": (("<H", last + 10, (12,)),),
        "zip-encrypted-extra": (("<H", last + 8, (1,)),),
        "zip-central-zip64": (("<LL", last + 20, (0xFFFFFFFF, 0xFFFFFFFF)),),
    }
    for spelling, offset, values in changes.get(scenario, ()):
        struct.pack_into(spelling, raw, offset, *values)
    if scenario in {"zip-store-trailing", "zip-deflate-extra-output"}:
        checksum, size = crc32(files["image.tar"]), len(files["image.tar"])
        for offset in (14, first + 16):
            struct.pack_into("<L", raw, offset, checksum)
        for offset in (22, first + 24):
            struct.pack_into("<L", raw, offset, size)
    if scenario == "zip-local-name":
        raw[30] ^= 1
    if scenario == "zip-trailing":
        raw.extend(b"trailing")
    if scenario.startswith("zip-shared-offset-"):
        raw = shared_offset(
            raw, before=scenario.endswith("first"), wide=scenario.endswith("wide-extent")
        )
    return bytes(raw)


def shared_offset(raw: bytearray, *, before: bool, wide: bool) -> bytearray:
    """Add a safe unopened central alias without creating or extracting a local file."""
    end = raw.rfind(b"PK\x05\x06")
    size, directory = struct.unpack_from("<LL", raw, end + 12)
    if wide:
        # Preserve small physical bytes but use a valid selected ZIP64 compressed extent.
        name_bytes, extra_bytes = struct.unpack_from("<HH", raw, directory + 28)
        field = struct.pack("<HHQ", 1, 8, 0xFFFFFFFFFFFFFFFF)
        at = directory + 46 + name_bytes
        raw[at:at] = field
        struct.pack_into("<L", raw, directory + 20, 0xFFFFFFFF)
        struct.pack_into("<H", raw, directory + 30, extra_bytes + len(field))
        end += len(field)
        size += len(field)
        struct.pack_into("<L", raw, end + 12, size)
    header = bytearray(raw[directory : directory + 46])
    name = b"unselected.bin"
    struct.pack_into("<HH", header, 8, 0, 0)
    struct.pack_into("<LLL", header, 16, 0, 0, 0)
    struct.pack_into("<HHH", header, 28, len(name), 0, 0)
    struct.pack_into("<L", header, 42, 0)
    extra = header + name
    at = directory if before else end
    result = raw[:at] + extra + raw[at:]
    end += len(extra)
    count = struct.unpack_from("<H", result, end + 10)[0] + 1
    struct.pack_into("<HH", result, end + 8, count, count)
    struct.pack_into("<L", result, end + 12, size + len(extra))
    return result


def response_identity(value: list[JsonValue], claim: JsonObject, scenario: str) -> None:
    """Apply one certificate/statement identity or opaque-token response mutation."""
    result = obj(value[0], "verificationResult")
    statement = obj(result, "statement")
    if scenario.startswith("gh-certificate-"):
        obj(result, "signature", "certificate")[scenario.removeprefix("gh-certificate-")] = "wrong"
    if scenario == "gh-no-timestamp":
        result["verifiedTimestamps"] = []
    if scenario == "gh-statement-type":
        statement["_type"] = "wrong"
    if scenario == "gh-predicate-type":
        statement["predicateType"] = "wrong"
    if scenario == "gh-claim":
        obj(statement, "predicate")["revision"] = "1" * 40
    if scenario == "gh-claim-schema-bool":
        obj(statement, "predicate")["schemaVersion"] = True
    if scenario == "gh-claim-run-float":
        obj(statement, "predicate")["runId"] = float(integer_value(claim["runId"]))
    if scenario == "gh-large-integer":
        result["ignored"] = 2**180 + 1
    if scenario == "gh-surrogate":
        result["ignored"] = "\ud800"


def response_bytes(claim: JsonObject, scenario: str, index: int) -> bytes:
    """Preserve exact raw response spelling and closed certificate/statement cases."""
    value = verified_output(copy.deepcopy(claim))
    response_identity(value, claim, scenario)
    statement = obj(value[0], "verificationResult", "statement")
    subjects = array(statement, "subject")
    if scenario == "gh-subject-missing":
        _ = subjects.pop()
    if scenario == "gh-subject-duplicate":
        subjects[-1] = copy.deepcopy(subjects[0])
    if scenario == "gh-subject-extra":
        subjects.append(copy.deepcopy(subjects[0]))
    if scenario == "gh-subject-reordered":
        subjects.reverse()
    if scenario == "gh-empty":
        value = []
    if scenario == "gh-multiple":
        value.append(copy.deepcopy(value[0]))
    if scenario == "gh-invalid-json":
        return b"not JSON"
    if scenario == "gh-duplicate-json":
        return b'[{"verificationResult":{},"verificationResult":{}}]'
    raw = json.dumps(value, indent=index % 3, sort_keys=index % 2 == 0).encode()
    return raw.decode().encode("utf-16") if scenario == "gh-utf16" else raw


@dataclass
class Fixture:
    """Closed original data and controlled response observations, never live authority."""

    files: dict[str, bytes]
    selection: JsonObject
    responses: list[bytes]
    statuses: list[int]
    repeat: int
    mutation: JsonObject | None
    clock: str
    profile: str | None


def fixture(scenario: str) -> Fixture:
    """Construct a bounded original candidate before invoking any reference function."""
    if scenario not in SCENARIOS:
        message = "unknown closed verify scenario"
        raise ValueError(message)
    files = fixture_files()
    selected = envelope()
    del selected["attestation"]
    claim = metadata_cases(scenario, files, selected)
    archive = zip_bytes(files, scenario)
    files["artifact.zip"] = archive
    selected["artifactZipBytes"], selected["zipSha256"] = len(archive), digest(archive)
    mutation: JsonObject | None = None
    if scenario.startswith("mutate-") or scenario == "exchange-bundle":
        target = {
            "mutate-bundle": trust.BUNDLE,
            "mutate-source": "source.json",
            "mutate-outcome": "outcome.json",
            "mutate-manifest": "release.json",
            "mutate-zip": "artifact.zip",
            "exchange-bundle": trust.BUNDLE,
        }[scenario]
        mutation = {
            "afterCall": 4,
            "target": target,
            "kind": "exchange" if scenario == "exchange-bundle" else "replace",
            "path": "mutations/changed.bin",
        }
        changed = files[target]
        if scenario != "exchange-bundle":
            changed += b"\nchanged by owned fixture\n"
        mutation["bytes"], mutation["sha256"] = len(changed), digest(changed)
    statuses = [0, 0, 0, 0]
    if scenario.startswith("gh-failure-"):
        statuses[0 if scenario.endswith("first") else 1] = 7
    return Fixture(
        files,
        selected,
        [response_bytes(claim, scenario, i) for i in range(4)],
        statuses,
        2 if scenario == "repeat-fresh" else 1,
        mutation,
        {
            "optional-utc-error": "error",
            "required-utc-error": "error",
            "late-utc-error": "late-error",
            "late-expiry": "late-expiry",
        }.get(scenario, "fixed"),
        PROFILE.get(scenario),
    )


def populate(root: Path, source: Fixture) -> None:
    """Export candidate and response fixtures separately with fixed private modes."""
    root.mkdir(mode=0o700)
    for name in ("candidate", "responses", "mutations", "bin", "home", "config"):
        (root / name).mkdir(mode=0o700)
    for name, data in source.files.items():
        write(root / "candidate" / name, data)
    for i, data in enumerate(source.responses):
        write(root / "responses" / f"{i:02}.json", data)
    write(root / "metadata.json", encoded(source.selection))
    write(root / "bin/gh", b"#!/bin/sh\nexit 99\n", 0o700)
    if source.mutation is not None:
        target = str(source.mutation["target"])
        data = source.files[target]
        if source.mutation["kind"] != "exchange":
            data += b"\nchanged by owned fixture\n"
        write(root / "mutations/changed.bin", data)
    total = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
    if total > MAX_FIXTURE_BYTES:
        message = "test-only verify fixture export bound"
        raise ValueError(message)


def pins(directory: Path) -> JsonObject:
    """Observe exact retained candidate bytes and fixed modes after completion."""
    result: JsonObject = {}
    for path in sorted(directory.iterdir()):
        data = path.read_bytes()
        result[path.name] = {
            "sha256": digest(data),
            "bytes": len(data),
            "mode": stat.S_IMODE(path.stat().st_mode),
        }
    return result


def stamps(directory: Path) -> dict[str, tuple[int, int, int, int, int]]:
    """Keep inode and write history separate from byte-equality observations."""
    return {
        p.name: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        for p in directory.iterdir()
        if (s := p.stat())
    }


def environment(root: Path) -> dict[str, str]:
    """Select fixed inert values; no inherited real credentials enter the oracle."""
    return {key: value.replace("$FIXTURE", str(root)) for key, value in ENVIRONMENT.items()}


@final
class Verifier:
    """Adapt only the original bounded verifier process and named concurrent mutation."""

    def __init__(self, root: Path, source: Fixture) -> None:
        """Retain one closed fixture and its actual invocation history."""
        self.root = root
        self.source = source
        self.calls: list[JsonValue] = []

    def run(
        self, arguments: Sequence[str], *, env: Mapping[str, str], limits: bounded_process.Limits
    ) -> tuple[int, bytes, bytes]:
        """Return controlled raw bytes after recording the exact real call contract."""
        index = len(self.calls)
        response = self.source.responses[index % 4]
        status = self.source.statuses[index % 4]
        candidate = str(self.root / "candidate")
        self.calls.append(
            {
                "subject": Path(arguments[3]).name,
                "arguments": [arg.replace(candidate, "$CANDIDATE") for arg in arguments],
                "environment": {
                    key: value.replace(str(self.root), "$FIXTURE")
                    for key, value in sorted(env.items())
                },
                "timeoutSeconds": limits.timeout,
                "stdoutLimit": limits.stdout,
                "stderrLimit": limits.stderr,
                "status": status,
                "responseSha256": digest(response),
                "responseBytes": len(response),
            }
        )
        mutation = self.source.mutation
        if mutation is not None and len(self.calls) == mutation["afterCall"]:
            target = self.root / "candidate" / str(mutation["target"])
            data = (self.root / str(mutation["path"])).read_bytes()
            if mutation["kind"] == "exchange":
                fresh = self.root / "candidate/.replacement"
                write(fresh, data)
                _ = fresh.replace(target)
            else:
                with target.open("wb") as output:
                    _ = output.write(data)
        return status, response, b""


def verify_reference(candidate: Path, selection: JsonObject, scenario: str) -> JsonObject:
    """Capture only the one declared overlap warning; all unrelated diagnostics fail."""
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        try:
            return attest.verify_directory(candidate, selection)
        finally:
            expected = int(
                scenario in {"zip-shared-offset-extra-last", "zip-shared-offset-wide-extent"}
            )
            if len(caught) != expected or any(
                warning.category is not UserWarning
                or str(warning.message) != "Overlapped entries: 'image.tar' (possible zip bomb)"
                for warning in caught
            ):
                message = "unexpected warning in closed ZIP reference fixture"
                raise RuntimeError(message)


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Execute the complete unchanged reference, exporting pristine data first."""
    source = fixture(scenario)
    if fixture_directory is not None:
        if (
            not fixture_directory.is_absolute()
            or fixture_directory.parent.resolve() != fixture_directory.parent
        ):
            message = "fixture export needs a fresh canonical absolute path"
            raise ValueError(message)
        populate(fixture_directory, source)
    with tempfile.TemporaryDirectory(prefix="release-verify-reference-") as temporary:
        root = Path(temporary).resolve() / "input"
        populate(root, source)
        candidate = root / "candidate"
        before, original_stamps = pins(candidate), stamps(candidate)
        verifier = Verifier(root, source)
        FixedDateTime.calls, FixedDateTime.clock = 0, source.clock
        results: list[JsonValue] = []
        passes: list[JsonValue] = []
        reason: str | None = None
        with (
            patch.object(bounded_process, "run", verifier.run),
            patch.object(trust, "datetime", FixedDateTime),
            patch.dict(
                os.environ, {**environment(root), "UNRELATED": "must-not-inherit"}, clear=True
            ),
        ):
            for _ in range(source.repeat):
                try:
                    result = verify_reference(candidate, source.selection, scenario)
                except (
                    ValueError,
                    OSError,
                    KeyError,
                    TypeError,
                    zipfile.BadZipFile,
                    NotImplementedError,
                    receiver.FetchError,
                ):
                    passes.append(False)
                    results.append(None)
                    reason = "release_attestation_failed"
                    break
                passes.append(True)
                results.append(result)
        response_pins: list[JsonValue] = [
            {
                "path": f"responses/{i:02}.json",
                "bytes": len(data),
                "sha256": digest(data),
                "status": source.statuses[i],
            }
            for i, data in enumerate(source.responses)
        ]
        return {
            "schemaVersion": 1,
            "scenario": scenario,
            "input": {
                "selection": source.selection,
                "today": TODAY,
                "repeat": source.repeat,
                "responses": response_pins,
                "mutation": source.mutation,
                "clock": source.clock,
                "rustProfile": source.profile,
            },
            "observation": {
                "passed": all(passes),
                "passes": passes,
                "results": results,
                "calls": verifier.calls,
                "candidateUnchanged": before == pins(candidate)
                and original_stamps == stamps(candidate),
                "files": pins(candidate),
                "failureClass": reason,
                "utcSamples": FixedDateTime.calls,
            },
        }


class Options(argparse.Namespace):
    """Typed closed CLI fields; arbitrary attributes never select fixture behavior."""

    scenario: str = ""
    fixture_directory: Path | None = None


def main(arguments: Sequence[str] | None = None) -> int:
    """Emit one bounded ordinary JSON record for an explicitly selected scenario."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    options = parser.parse_args(arguments, namespace=Options())
    scenario: str = options.scenario
    directory: Path | None = options.fixture_directory
    result = json.dumps(run(scenario, directory), sort_keys=True, allow_nan=False).encode() + b"\n"
    if len(result) > MAX_OUTPUT_BYTES:
        message = "test-only verify oracle output bound"
        raise ValueError(message)
    _ = os.write(1, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
