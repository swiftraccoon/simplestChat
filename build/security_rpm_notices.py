"""Collect bounded RPM notice identities from an already authenticated image filesystem.

This is evidence collection, not a license decision or exception importer. No
file contents enter the report. The caller must bind the materialized rootfs and
Syft inventory to the supplied immutable image/archive/source identities.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING
from urllib.parse import parse_qsl, unquote, urlsplit

import security_elf
from security_license_identity import raw_license_identity
from security_tools import require

# isort: split
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

MAX_PACKAGES = 20000
MAX_METADATA_FILES = 200000
MAX_NOTICES = 4096
MAX_NOTICE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_REPORT_BYTES = 16 * 1024 * 1024
MAX_LICENSE_BYTES = 64 * 1024
MAX_PATH = 4096
MAX_EPOCH = 4294967295
CHUNK = 65536
CATALOGER = "rpm-db-cataloger"
NATIVE_CATALOGER = "simplestchat-authenticated-native-inputs"
ARCHITECTURES = {"linux/amd64": "x86_64", "linux/arm64": "aarch64"}
DOC_NOTICE = re.compile(r"(?:COPYING|LICENSE|LICENCE|COPYRIGHT|NOTICE)(?:[._-].*)?", re.IGNORECASE)


def notice_path(value: str) -> bool:
    """Select license directories and named documentation notices, never arbitrary payloads."""
    path = PurePosixPath(value)
    require(
        0 < len(value) <= MAX_PATH
        and value.startswith("/")
        and str(path) == value
        and ".." not in path.parts
        and "\0" not in value
        and "\\" not in value,
        "rpm_notice_path",
    )
    return value.startswith("/usr/share/licenses/") or (
        value.startswith("/usr/share/doc/") and DOC_NOTICE.fullmatch(path.name) is not None
    )


def package_identity(package: JsonObject, architecture: str) -> JsonObject:
    """Require the observed package URL and RPM metadata to describe the same package."""
    require(package.get("metadataType") == "rpm-db-entry", "rpm_notice_metadata_type")
    metadata = object_value(package["metadata"])
    name = string_value(package["name"])
    version, release = string_value(metadata["version"]), string_value(metadata["release"])
    source, arch = string_value(metadata["sourceRpm"]), string_value(metadata["architecture"])
    epoch = metadata["epoch"]
    require(epoch is None or (type(epoch) is int and 0 <= epoch <= MAX_EPOCH), "rpm_notice_epoch")
    normalized_epoch = 0 if epoch is None else integer_value(epoch)
    require(name == metadata["name"] and name and version and release, "rpm_notice_identity")
    version_release = version + "-" + release
    require(
        package["version"]
        == (f"{normalized_epoch}:" if normalized_epoch else "") + version_release,
        "rpm_notice_version",
    )
    purl = string_value(package["purl"])
    parsed = urlsplit(purl)
    pairs = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
    qualifiers = dict(pairs)
    expected = {"distro": "fedora-44"}
    if name == "gpg-pubkey":
        require(not source and not arch and not normalized_epoch, "rpm_notice_key_metadata")
    else:
        require(
            arch in {architecture, "noarch"} and source.endswith(".src.rpm"), "rpm_notice_origin"
        )
        expected.update(arch=arch, upstream=source)
        if normalized_epoch:
            expected["epoch"] = str(normalized_epoch)
    require(
        len(pairs) == len(qualifiers)
        and qualifiers == expected
        and parsed.scheme == "pkg"
        and not parsed.netloc
        and not parsed.fragment
        and unquote(parsed.path) == f"rpm/fedora/{name}@{version_release}"
        and re.search(r"%(?![0-9a-fA-F]{2})", purl) is None,
        "rpm_notice_purl",
    )
    return {
        "name": name,
        "version": package["version"],
        "purl": purl,
        "sourceRpm": source,
        "architecture": arch,
        "epoch": epoch,
    }


def license_identity(package: JsonObject) -> JsonObject:
    """Preserve complete ordered declarations and the exact current review fingerprint."""
    records = array_value(package["licenses"])
    require(len(json.dumps(records).encode()) <= MAX_LICENSE_BYTES, "rpm_notice_license_size")
    raw = raw_license_identity(records)
    expressions = [string_value(object_value(item).get("spdxExpression", "")) for item in records]
    expression = " AND ".join("(" + value + ")" for value in expressions if value)
    complete = bool(expressions) and all(expressions)
    return {
        "records": records,
        "recordsSha256": raw.records_sha256,
        "reviewFingerprint": "license:" + hashlib.sha256(expression.encode()).hexdigest()
        if complete
        else raw.fingerprint,
        "expression": expression or "UNKNOWN",
    }


def file_identity(rootfs: Path, record: JsonObject, remaining: int) -> JsonObject:
    """Hash one regular notice or image-confined notice symlink under strict byte limits."""
    name = string_value(record["path"])
    mode, expected_size = integer_value(record["mode"]), integer_value(record["size"])
    checksum = object_value(record["digest"])
    expected = string_value(checksum["value"])
    regular = stat.S_ISREG(mode)
    require(
        (regular or stat.S_ISLNK(mode))
        and checksum["algorithm"] == "sha256"
        and 0 <= expected_size <= MAX_NOTICE_BYTES
        and (re.fullmatch(r"[a-f0-9]{64}", expected) if regular else expected == ""),
        "rpm_notice_file_metadata",
    )
    resolved = security_elf.image_path(rootfs, name)
    resolved_name = "/" + resolved.relative_to(rootfs).as_posix()
    require(notice_path(resolved_name), "rpm_notice_link_scope")
    descriptor = os.open(resolved, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        require(
            stat.S_ISREG(before.st_mode)
            and 0 <= before.st_size <= MAX_NOTICE_BYTES
            and before.st_size <= remaining,
            "rpm_notice_file_budget",
        )
        sha256 = hashlib.sha256()
        length = 0
        while block := stream.read(CHUNK):
            length += len(block)
            require(length <= MAX_NOTICE_BYTES and length <= remaining, "rpm_notice_file_budget")
            sha256.update(block)
        after = os.fstat(stream.fileno())
        require(
            (before.st_size, before.st_mtime_ns, before.st_ctime_ns)
            == (after.st_size, after.st_mtime_ns, after.st_ctime_ns)
            and length == before.st_size,
            "rpm_notice_changed_during_read",
        )
    actual = sha256.hexdigest()
    return {
        "path": name,
        "resolvedPath": resolved_name,
        "kind": "regular" if regular else "symlink",
        "bytes": length,
        "sha256": actual,
        "rpmFile": record,
        "matchesRpmDigest": actual == expected and length == expected_size if regular else None,
    }


def collect(  # noqa: PLR0913 - Each immutable artifact binding is explicit at the call site.
    rootfs: Path,
    packages: Sequence[JsonObject],
    *,
    image_id: str,
    archive_sha256: str,
    sbom_sha256: str,
    revision: str,
    platform: str,
) -> JsonObject:
    """Collect exact runtime observations without granting license approval or writing files."""
    require(
        platform in ARCHITECTURES
        and re.fullmatch(r"sha256:[a-f0-9]{64}", image_id)
        and re.fullmatch(r"[a-f0-9]{64}", archive_sha256)
        and re.fullmatch(r"[a-f0-9]{64}", sbom_sha256)
        and re.fullmatch(r"[a-f0-9]{40}", revision),
        "rpm_notice_artifact_identity",
    )
    require(
        rootfs.is_absolute() and rootfs.resolve(strict=True) == rootfs and rootfs.is_dir(),
        "rpm_notice_rootfs",
    )
    require(0 < len(packages) <= MAX_PACKAGES, "rpm_notice_package_budget")
    result: list[JsonValue] = []
    excluded: list[JsonValue] = []
    seen: set[str] = set()
    metadata_files = count = total = directories = 0
    passed = True
    for package in packages:
        if package.get("type") != "rpm":
            continue
        if package.get("foundBy") == NATIVE_CATALOGER:
            require(
                package.get("metadataType") == "rpm-db-entry"
                and object_value(package["metadata"]).get("files") == [],
                "rpm_notice_supplemental_origin",
            )
            excluded.append(
                {"purl": package["purl"], "reason": "static-build-input-not-runtime-files"}
            )
            continue
        require(package.get("foundBy") == CATALOGER, "rpm_notice_cataloger")
        identity = package_identity(package, ARCHITECTURES[platform])
        scope = string_value(identity["purl"])
        require(scope not in seen, "rpm_notice_duplicate_package")
        seen.add(scope)
        notices: list[JsonValue] = []
        file_paths: set[str] = set()
        for value in array_value(object_value(package["metadata"])["files"]):
            metadata_files += 1
            require(metadata_files <= MAX_METADATA_FILES, "rpm_notice_metadata_budget")
            record = object_value(value)
            name = string_value(record["path"])
            require(name not in file_paths, "rpm_notice_duplicate_path")
            file_paths.add(name)
            if not notice_path(name):
                continue
            mode = integer_value(record["mode"])
            if stat.S_ISDIR(mode):
                directories += 1
                continue
            count += 1
            require(count <= MAX_NOTICES, "rpm_notice_count_budget")
            observed = file_identity(rootfs, record, MAX_TOTAL_BYTES - total)
            total += integer_value(observed["bytes"])
            passed = passed and observed["matchesRpmDigest"] is not False
            notices.append(observed)
        result.append({**identity, "licenses": license_identity(package), "notices": notices})
    require(bool(result), "rpm_notice_runtime_inventory_missing")
    report: JsonObject = {
        "schemaVersion": 1,
        "passed": passed,
        "imageId": image_id,
        "archiveSha256": archive_sha256,
        "sbomSha256": sbom_sha256,
        "revision": revision,
        "platform": platform,
        "runtimePackages": result,
        "excludedStaticInputs": excluded,
        "metadataFiles": metadata_files,
        "noticeFiles": count,
        "noticeBytes": total,
        "noticeDirectories": directories,
        "scope": "Runtime RPM notice-file identities; no license approval and no file contents",
        "limits": {
            "packages": MAX_PACKAGES,
            "metadataFiles": MAX_METADATA_FILES,
            "notices": MAX_NOTICES,
            "fileBytes": MAX_NOTICE_BYTES,
            "totalBytes": MAX_TOTAL_BYTES,
            "reportBytes": MAX_REPORT_BYTES,
        },
    }
    # Match security_image.write's exact published representation, including its newline.
    encoded = (json.dumps(report, indent=2, sort_keys=True) + "\n").encode()
    require(len(encoded) <= MAX_REPORT_BYTES, "rpm_notice_report_budget")
    return report
