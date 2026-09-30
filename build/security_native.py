"""Bind reviewed native sources to one successful production Cargo build.

This is build-time evidence, not an assertion that every object in an archive
survived linker garbage collection. Cargo messages select actual build outputs;
the vendor receipt authenticates source inputs, and the final binary hash binds
the evidence to the executable subsequently inspected in the exported image.
"""

# Fixed validation errors form the standalone build-gate contract.
# ruff: noqa: EM101, TRY003
from __future__ import annotations

import argparse
import hashlib
import os
import re
import stat
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import security_elf as elf
import security_vendor as vendor

# isort: split
import bounded_process
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    json_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

MAX_MESSAGES = 32 * 1024 * 1024
MAX_REPORT = 8 * 1024 * 1024
MAX_ARTIFACT = 512 * 1024 * 1024
MAX_RECORDS = 20000
MAX_LICENSE = 4096
RPM_FIELDS = 3
SOURCE_REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
SYSTEM_ROOT = Path("/usr")


class NativeError(ValueError):
    """Native provenance or the actual production build differs from reviewed policy."""


@dataclass(frozen=True)
class Build:
    """Paths to one production build and its authenticated source evidence."""

    root: Path
    vendor_report: Path
    cargo_messages: Path
    cargo_home: Path
    openssl_prefix: Path
    output: Path


@dataclass(frozen=True)
class CargoBuild:
    """Only events from a complete successful invocation, including cached outputs."""

    binary: Path
    artifacts: Mapping[str, JsonObject]
    scripts: Mapping[tuple[str, str], JsonObject]


def text(obj: JsonObject, key: str) -> str:
    """Read a required string field without coercion."""
    return string_value(obj[key])


def strings(value: JsonValue) -> list[str]:
    """Read a complete string array."""
    return [string_value(item) for item in array_value(value)]


def reviewed_license(component: JsonObject) -> str:
    """Require an explicit bounded SPDX expression in maintained native metadata."""
    expression = text(component, "license")
    if (
        re.fullmatch(r"[A-Za-z0-9.+() -]{1,4096}", expression) is None
        or expression.strip() != expression
    ):
        raise NativeError("Missing or invalid reviewed native license expression")
    return expression


def toml(path: Path) -> JsonObject:
    """Read bounded TOML into the existing checked JSON value domain."""
    return object_value(
        json_value(tomllib.loads(vendor.read_regular(path, vendor.MAX_MANIFEST).decode()))
    )


def record_hash(path: Path, *, archive: bool = False) -> JsonObject:
    """Hash a bounded regular artifact without retaining its whole contents."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_ARTIFACT:
            raise NativeError("Missing or excessive native artifact")
        if archive:
            if stream.read(8) != b"!<arch>\n":
                raise NativeError("Native library must be a regular self-contained ar archive")
            _ = stream.seek(0)
        digest = hashlib.sha256()
        size = 0
        while block := stream.read(vendor.CHUNK):
            size += len(block)
            if size > MAX_ARTIFACT:
                raise NativeError("Native artifact size limit exceeded")
            digest.update(block)
        after = os.fstat(stream.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise NativeError("Native artifact changed while being hashed")
    return {"path": str(path), "sha256": digest.hexdigest(), "size": size}


def beneath(path: Path, directory: Path) -> Path:
    """Require an absolute resolved artifact path within its declared build directory."""
    if not path.is_absolute() or not path.resolve().is_relative_to(directory.resolve()):
        raise NativeError("Native artifact escaped its declared directory")
    return path.resolve(strict=True)


def validate_manifest(root: Path) -> tuple[JsonObject, vendor.Manifest]:
    """Check native classifications against complete vendor sources and the Cargo lock."""
    manifest = vendor.fields(
        decode_json(
            vendor.read_regular(root / "vendor/native-components.json", vendor.MAX_MANIFEST)
        ),
        {"wrap_components", "adapted_component", "openssl", "registry_component", "native_links"},
    )
    integrity = vendor.parse_manifest(
        vendor.read_regular(root / "vendor/integrity.json", vendor.MAX_MANIFEST)
    )
    components = [
        vendor.fields(item, {"source", "name", "version", "linkage", "usage", "license"})
        for item in array_value(manifest["wrap_components"])
    ]
    source_ids = [text(component, "source") for component in components]
    vendor.unique(source_ids, "native component source")
    if set(source_ids) != {wrap.source for wrap in integrity.wraps}:
        raise NativeError("Missing or stale native wrap component")
    for component in components:
        _ = reviewed_license(component)
        if text(component, "linkage") not in {"static", "header-only"} or text(
            component, "usage"
        ) not in {"production", "native-test", "windows-only"}:
            raise NativeError("Unsupported native component classification")
        if (
            not text(component, "name")
            or not text(component, "version")
            or text(component, "version") not in text(component, "source")
        ):
            raise NativeError("Native component version differs from its pinned source")
    validate_adapted(root, manifest["adapted_component"])
    validate_openssl(root, manifest["openssl"])
    validate_registry_lock(root, manifest["registry_component"])
    for key in ("adapted_component", "openssl", "registry_component"):
        _ = reviewed_license(object_value(manifest[key]))
    providers = object_value(manifest["native_links"])
    if set(providers) != {"mediasoup-sys", "openssl-sys", "aws-lc-sys"}:
        raise NativeError("Missing or stale native link provider")
    for value in providers.values():
        provider = vendor.fields(value, {"static", "system"})
        libraries = strings(provider["static"]) + strings(provider["system"])
        vendor.unique(libraries, "native library")
        if any(re.fullmatch(r"[A-Za-z0-9_+.-]{1,128}", name) is None for name in libraries):
            raise NativeError("Invalid native library name")
    return manifest, integrity


def validate_adapted(root: Path, value: JsonValue) -> None:
    """Keep adapted libwebrtc's upstream identity separate from a release-version claim."""
    component = vendor.fields(value, {"name", "version", "revision", "path", "license"})
    path = vendor.relative_path(text(component, "path"))
    if (
        path != "vendor/mediasoup-sys-0.17.0/deps/libwebrtc"
        or text(component, "name") != "libwebrtc"
    ):
        raise NativeError("Unsupported adapted native component")
    readme = vendor.read_regular(root / path / "README.md", vendor.MAX_MANIFEST).decode()
    if (
        re.fullmatch(r"[a-f0-9]{40}", text(component, "revision")) is None
        or f"- libwebrtc branch: {text(component, 'version')}\n" not in readme
        or f"- libwebrtc commit: {text(component, 'revision')}\n" not in readme
    ):
        raise NativeError("Adapted native source identity differs")


def validate_openssl(root: Path, value: JsonValue) -> None:
    """Bind OpenSSL identity to the actual checksum-checking build helper."""
    component = vendor.fields(value, {"version", "source", "installer_sha256", "license"})
    source = vendor.parse_source(component["source"])
    version = text(component, "version")
    installer = vendor.read_regular(root / "build/install-openssl.sh", vendor.MAX_MANIFEST)
    if (
        vendor.sha256(installer) != vendor.digest(component["installer_sha256"])
        or source.identifier != "openssl-" + version
        or source.format != "tar.gz"
        or source.url
        != f"https://github.com/openssl/openssl/releases/download/openssl-{version}/openssl-{version}.tar.gz"
        or f"openssl_version='{version}'\n".encode() not in installer
        or f"openssl_sha256='{source.sha256}'\n".encode() not in installer
    ):
        raise NativeError("OpenSSL source or installer identity differs")


def validate_registry_lock(root: Path, value: JsonValue) -> None:
    """Require the declared bundled native crate to match the exact registry lock entry."""
    component = vendor.fields(
        value, {"name", "version", "revision", "crate", "crate_version", "source", "license"}
    )
    source = vendor.parse_source(component["source"])
    name, version = text(component, "crate"), text(component, "crate_version")
    packages = [object_value(item) for item in array_value(toml(root / "Cargo.lock")["package"])]
    matches = [
        item for item in packages if item.get("name") == name and item.get("version") == version
    ]
    if (
        name != "aws-lc-sys"
        or text(component, "name") != "AWS-LC"
        or re.fullmatch(r"[a-f0-9]{40}", text(component, "revision")) is None
        or len(matches) != 1
        or matches[0].get("source") != SOURCE_REGISTRY
        or matches[0].get("checksum") != source.sha256
        or source.identifier != name + "-" + version
        or source.url != f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
        or source.format != "tar.gz"
    ):
        raise NativeError("Bundled native registry source differs from Cargo.lock")


def validate_vendor_receipt(build: Build, integrity: vendor.Manifest) -> str:
    """Bind source evidence to every current vendor file before using its native identities."""
    data = vendor.read_regular(build.vendor_report, MAX_REPORT)
    report = vendor.fields(
        decode_json(data),
        {"manifest_sha256", "diff_sha256", "sources", "trees", "files", "maintained_files"},
    )
    if report["manifest_sha256"] != vendor.sha256(
        vendor.read_regular(build.root / "vendor/integrity.json", vendor.MAX_MANIFEST)
    ):
        raise NativeError("Vendor receipt manifest differs")
    if report["diff_sha256"] != vendor.sha256(
        vendor.read_regular(build.vendor_report.parent / "vendor.diff", vendor.MAX_DIFF)
    ):
        raise NativeError("Vendor receipt diff differs")
    source_records = [object_value(item) for item in array_value(report["sources"])]
    vendor.unique([text(item, "id") for item in source_records], "receipt source")
    if {text(item, "id") for item in source_records} != set(integrity.sources):
        raise NativeError("Vendor receipt source coverage differs")
    for item in source_records:
        source = integrity.sources[text(item, "id")]
        if (item.get("url"), item.get("sha256"), item.get("format")) != (
            source.url,
            source.sha256,
            source.format,
        ):
            raise NativeError("Vendor receipt upstream source differs")
    local = {
        "vendor/" + path: body for path, body in vendor.tree_files(build.root / "vendor").items()
    }
    vendor.validate_coverage(integrity, local)
    reports = [object_value(item) for item in array_value(report["trees"])]
    if [text(item, "path") for item in reports] != [tree.path for tree in integrity.trees]:
        raise NativeError("Vendor receipt tree coverage differs")
    for tree, result in zip(integrity.trees, reports, strict=True):
        validate_receipt_tree(tree, result, local)
    validate_receipt_files(integrity, report, local)
    return vendor.sha256(data)


def validate_receipt_files(
    integrity: vendor.Manifest, report: JsonObject, local: Mapping[str, bytes]
) -> None:
    """Verify exact copied-file and repository metadata coverage in the vendor receipt."""
    for key in ("files", "maintained_files"):
        records = [object_value(item) for item in array_value(report[key])]
        expected = set(integrity.files) if key == "files" else set(integrity.maintained_files)
        vendor.unique([text(item, "path") for item in records], "receipt file")
        if {text(item, "path") for item in records} != expected:
            raise NativeError("Vendor receipt file coverage differs")
        if any(item.get("sha256") != vendor.sha256(local[text(item, "path")]) for item in records):
            raise NativeError("Vendor receipt current file differs")


def validate_receipt_tree(
    tree: vendor.Tree, report: JsonObject, local: Mapping[str, bytes]
) -> None:
    """Check every reported current file and every recorded upstream deviation."""
    if report.get("source") != tree.source:
        raise NativeError("Vendor receipt tree source differs")
    records = [
        vendor.fields(item, {"path", "upstream_sha256", "vendored_sha256"})
        for item in array_value(report["files"])
    ]
    vendor.unique([text(item, "path") for item in records], "receipt tree member")
    expected = {
        path.removeprefix(tree.path + "/") for path in local if path.startswith(tree.path + "/")
    }
    declared = {change.path: change for change in tree.changes}
    current: set[str] = set()
    differences: set[str] = set()
    for item in records:
        path = vendor.relative_path(text(item, "path"))
        old, new = item["upstream_sha256"], item["vendored_sha256"]
        if old is not None:
            _ = vendor.digest(old)
        if new is not None:
            current.add(path)
            if new != vendor.sha256(local[tree.path + "/" + path]):
                raise NativeError("Vendor receipt tree bytes differ")
        if old != new:
            differences.add(path)
            record = declared.get(path)
            if record is None or (old, new) != (record.upstream_sha256, record.vendored_sha256):
                raise NativeError("Vendor receipt deviation differs")
    if current != expected or differences != set(declared):
        raise NativeError("Vendor receipt member coverage differs")


def package_name(identifier: str) -> str:
    """Read current Cargo Package ID specifications without historical fallbacks."""
    name, separator, version = identifier.rpartition("#")[2].partition("@")
    if not separator or not version or re.fullmatch(r"[A-Za-z0-9_-]+", name) is None:
        raise NativeError("Unsupported Cargo package identifier")
    return name


def cargo_build(root: Path, data: bytes) -> CargoBuild:  # noqa: C901, PLR0912 -- One ordered stream owns completion and artifact identity.
    """Select one complete default-production invocation, including cached build scripts."""
    if len(data) > MAX_MESSAGES:
        raise NativeError("Cargo message budget exceeded")
    artifacts: dict[str, JsonObject] = {}
    scripts: dict[tuple[str, str], JsonObject] = {}
    binaries: list[Path] = []
    finished = False
    for number, line in enumerate(data.splitlines()):
        if number >= MAX_RECORDS or finished:
            raise NativeError("Cargo messages continue after completion or exceed budget")
        item = object_value(decode_json(line))
        reason = text(item, "reason")
        if reason == "build-finished":
            if item.get("success") is not True:
                raise NativeError("Native evidence requires a successful Cargo build")
            finished = True
        elif reason == "build-script-executed":
            script_key = (text(item, "package_id"), text(item, "out_dir"))
            if script_key in scripts:
                raise NativeError("Duplicate native build script event")
            scripts[script_key] = item
        elif reason == "compiler-artifact":
            identifier = text(item, "package_id")
            target = object_value(item["target"])
            if text(target, "name") == "simplestChat" and strings(target["kind"]) == ["bin"]:
                validate_production_artifact(root, item)
                binaries.append(beneath(Path(text(item, "executable")), root / "target/release"))
            # Cargo may emit custom-build and library artifacts for the same package.
            prior = artifacts.get(identifier)
            if prior is not None and prior.get("manifest_path") != item.get("manifest_path"):
                raise NativeError("Ambiguous Cargo package source path")
            artifacts[identifier] = item
        elif reason != "compiler-message":
            raise NativeError("Unsupported Cargo message kind")
    if not finished or len(binaries) != 1:
        raise NativeError("Missing successful production binary artifact")
    if binaries[0] != (root / "target/release/simplestChat").resolve(strict=True):
        raise NativeError("Production binary artifact path differs")
    return CargoBuild(binaries[0], artifacts, scripts)


def validate_production_artifact(root: Path, item: JsonObject) -> None:
    """Reject test/all-feature/load-generator evidence for the production server."""
    profile = object_value(item["profile"])
    if (
        Path(text(item, "manifest_path")).resolve() != root / "Cargo.toml"
        or strings(item["features"]) != ["default"]
        or profile.get("test") is not False
        or profile.get("debug_assertions") is not False
        or profile.get("opt_level") != "3"
        or text(item, "executable") not in strings(item["filenames"])
    ):
        raise NativeError("Cargo artifact is not the default release production server")


def registry_component(build: Build, graph: CargoBuild, value: JsonValue) -> JsonObject:
    """Authenticate the exact bundled AWS-LC crate and all unpacked build inputs."""
    component = object_value(value)
    name, version = text(component, "crate"), text(component, "crate_version")
    identifier = SOURCE_REGISTRY + "#" + name + "@" + version
    if identifier not in graph.artifacts or not any(key[0] == identifier for key in graph.scripts):
        raise NativeError("Declared bundled native crate is absent from the production build")
    source = vendor.parse_source(component["source"])
    manifest = beneath(
        Path(text(graph.artifacts[identifier], "manifest_path")), build.cargo_home / "registry/src"
    )
    directory = manifest.parent
    if (
        directory.name != source.identifier
        or directory.parent.parent != (build.cargo_home / "registry/src").resolve()
    ):
        raise NativeError("Native registry source directory differs")
    archive_path = (
        build.cargo_home / "registry/cache" / directory.parent.name / (source.identifier + ".crate")
    )
    original = vendor.archive_files(source, vendor.read_regular(archive_path, vendor.MAX_DOWNLOAD))
    prefix = source.identifier + "/"
    if any(not path.startswith(prefix) for path in original):
        raise NativeError("Native crate archive prefix differs")
    original = {path.removeprefix(prefix): body for path, body in original.items()}
    local = vendor.tree_files(directory)
    if local.pop(".cargo-ok", None) != b'{"v":1}' or original != local:
        raise NativeError("Bundled native source differs from authenticated crate bytes")
    metadata = object_value(json_value(tomllib.loads(original["Cargo.toml.orig"].decode())))
    package = object_value(metadata["package"])
    declared = object_value(object_value(package["metadata"])["aws-lc-sys"])
    header = original["aws-lc/include/openssl/base.h"].decode()
    if (
        declared.get("commit-hash") != component["revision"]
        or f'#define AWSLC_VERSION_NUMBER_STRING "{text(component, "version")}"\n' not in header
    ):
        raise NativeError("Bundled AWS-LC version or revision differs")
    packaged = object_value(
        object_value(json_value(tomllib.loads(original["Cargo.toml"].decode())))["package"]
    )
    if packaged.get("license") != reviewed_license(component):
        raise NativeError("Bundled AWS-LC license differs from authenticated source")
    return {
        "name": component["name"],
        "version": component["version"],
        "revision": component["revision"],
        "crate": name,
        "crate_version": version,
        "source_sha256": source.sha256,
        "verified_files": len(original),
        "license": component["license"],
    }


def package_license(package: JsonObject, files: Mapping[str, bytes]) -> JsonObject:
    """Preserve upstream SPDX text or an exact license-file identity without inference."""
    expression = package.get("license")
    if expression is not None and (
        not isinstance(expression, str) or not expression.strip() or len(expression) > MAX_LICENSE
    ):
        raise NativeError("Invalid upstream license expression")
    reference = package.get("license-file")
    license_file: JsonValue = None
    if reference is not None:
        path = vendor.relative_path(string_value(reference))
        if path not in files:
            raise NativeError("Declared license file is absent from authenticated source")
        license_file = {"path": path, "sha256": vendor.sha256(files[path])}
    return {"expression": expression, "license_file": license_file}


def registry_license_source(
    build: Build, path: Path, locked: JsonObject
) -> tuple[JsonObject, dict[str, bytes]]:
    """Authenticate registry metadata from its locked archive, independently of extracted files."""
    name, version = text(locked, "name"), text(locked, "version")
    if locked.get("source") != SOURCE_REGISTRY:
        raise NativeError("Rust license provenance requires a pinned crates.io archive")
    manifest = beneath(path, build.cargo_home / "registry/src")
    package_id = name + "-" + version
    if (
        manifest.parent.name != package_id
        or manifest.parent.parent.parent != (build.cargo_home / "registry/src").resolve()
    ):
        raise NativeError("Rust license source directory differs")
    archive = (
        build.cargo_home / "registry/cache" / manifest.parent.parent.name / (package_id + ".crate")
    )
    return registry_archive_license(archive, locked)


def registry_archive_license(
    archive: Path, locked: JsonObject
) -> tuple[JsonObject, dict[str, bytes]]:
    """Read declarations only from a Cargo.lock-authenticated crates.io archive."""
    name, version = text(locked, "name"), text(locked, "version")
    if locked.get("source") != SOURCE_REGISTRY:
        raise NativeError("Rust license provenance requires a pinned crates.io archive")
    package_id = name + "-" + version
    source = vendor.Source(
        package_id,
        f"https://static.crates.io/crates/{name}/{package_id}.crate",
        vendor.digest(locked["checksum"]),
        "tar.gz",
    )
    members = vendor.archive_files(source, vendor.read_regular(archive, vendor.MAX_DOWNLOAD))
    prefix = package_id + "/"
    if any(not member.startswith(prefix) for member in members):
        raise NativeError("Rust license archive prefix differs")
    files = {member.removeprefix(prefix): body for member, body in members.items()}
    package = object_value(
        object_value(json_value(tomllib.loads(files["Cargo.toml"].decode())))["package"]
    )
    return package, files


def binary_dependencies(path: Path) -> tuple[JsonObject, JsonObject]:
    """Bind the bounded embedded dependency graph to the same exact executable bytes."""
    binary = record_hash(path)
    data = elf.read_binary(path)
    if vendor.sha256(data) != binary["sha256"] or len(data) != binary["size"]:
        raise NativeError("Production binary changed during metadata inspection")
    machine = int.from_bytes(data[18:20], "little")
    platform = next((name for name, value in elf.PLATFORMS.items() if value[0] == machine), None)
    if platform is None:
        raise NativeError("Unsupported production binary architecture")
    metadata = object_value(
        json_value(elf.dependency_metadata(elf.parse(data, platform).audit_section))
    )
    return binary, metadata


def rust_licenses(
    build: Build, graph: CargoBuild, integrity: vendor.Manifest, embedded: JsonObject
) -> list[JsonValue]:
    """Authenticate the union of compiler events and the exact binary's embedded graph."""
    locked = [
        object_value(item) for item in array_value(toml(build.root / "Cargo.lock")["package"])
    ]
    records: list[JsonValue] = []
    embedded_identities = {
        (text(item, "name"), text(item, "version"), text(item, "source"))
        for value in array_value(embedded["packages"])
        for item in [object_value(value)]
    }
    if len(embedded_identities) != len(array_value(embedded["packages"])):
        raise NativeError("Ambiguous embedded Rust package identity")
    registry_indexes: set[str] = set()
    for identifier, artifact in sorted(graph.artifacts.items()):
        name = package_name(identifier)
        version = identifier.rpartition("@")[2]
        matches = [
            item for item in locked if item.get("name") == name and item.get("version") == version
        ]
        if len(matches) != 1:
            raise NativeError("Actual Cargo artifact is missing or ambiguous in Cargo.lock")
        package_lock = matches[0]
        path = Path(text(artifact, "manifest_path"))
        first_party = path.resolve() == build.root / "Cargo.toml"
        if identifier.startswith(SOURCE_REGISTRY + "#"):
            package, files = registry_license_source(build, path, package_lock)
            registry_indexes.add(path.resolve(strict=True).parent.parent.name)
            source_hash = package_lock["checksum"]
            source_kind = "crates.io"
        else:
            package, files, source_hash = local_license_source(build, integrity, path, package_lock)
            source_kind = "local"
        if package.get("name") != name or package.get("version") != version:
            raise NativeError("Cargo artifact package identity differs from authenticated metadata")
        if first_party and package.get("publish") is not False:
            raise NativeError("First-party Cargo publication policy must be false")
        records.append(
            {
                "package_id": identifier,
                "name": name,
                "version": version,
                "source": source_kind,
                "source_sha256": source_hash,
                "manifest_sha256": vendor.sha256(files["Cargo.toml"]),
                "first_party": first_party,
                "evidence": {
                    "compilerArtifact": True,
                    "embeddedMetadata": (name, version, source_kind) in embedded_identities,
                },
                **({"cargo_publish": False} if first_party else {}),
                "license": package_license(package, files),
            }
        )
    recorded = {
        (text(item, "name"), text(item, "version"), text(item, "source"))
        for value in records
        for item in [object_value(value)]
    }
    records.extend(
        embedded_registry_licenses(build, locked, embedded_identities - recorded, registry_indexes)
    )
    if len(records) > MAX_RECORDS:
        raise NativeError("Rust license evidence exceeds the record budget")
    return records


def embedded_registry_licenses(
    build: Build,
    locked: list[JsonObject],
    identities: set[tuple[str, str, str]],
    registry_indexes: set[str],
) -> list[JsonValue]:
    """Authenticate additional embedded identities without asserting they were compiled."""
    records: list[JsonValue] = []
    for name, version, source_kind in sorted(identities):
        if source_kind != "crates.io" or len(registry_indexes) != 1:
            raise NativeError("Embedded-only package requires one proven crates.io cache")
        matches = [
            item for item in locked if item.get("name") == name and item.get("version") == version
        ]
        if len(matches) != 1 or matches[0].get("source") != SOURCE_REGISTRY:
            raise NativeError("Embedded Rust package is missing or ambiguous in Cargo.lock")
        package_lock = matches[0]
        archive = (
            build.cargo_home
            / "registry/cache"
            / next(iter(registry_indexes))
            / (name + "-" + version + ".crate")
        )
        package, files = registry_archive_license(archive, package_lock)
        if package.get("name") != name or package.get("version") != version:
            raise NativeError("Embedded package identity differs from authenticated metadata")
        records.append(
            {
                "package_id": SOURCE_REGISTRY + "#" + name + "@" + version,
                "name": name,
                "version": version,
                "source": source_kind,
                "source_sha256": package_lock["checksum"],
                "manifest_sha256": vendor.sha256(files["Cargo.toml"]),
                "first_party": False,
                "evidence": {"compilerArtifact": False, "embeddedMetadata": True},
                "license": package_license(package, files),
            }
        )
    return records


def local_license_source(
    build: Build, integrity: vendor.Manifest, path: Path, locked: JsonObject
) -> tuple[JsonObject, dict[str, bytes], JsonValue]:
    """Local license declarations require the verified vendor tree or the first-party manifest."""
    if locked.get("source") is not None:
        raise NativeError("Unsupported non-registry Rust source")
    path = beneath(path, build.root)
    package = object_value(toml(path)["package"])
    if path == build.root / "Cargo.toml":
        files = {"Cargo.toml": vendor.read_regular(path, vendor.MAX_MANIFEST)}
        if package.get("license-file") is not None:
            name = vendor.relative_path(string_value(package["license-file"]))
            files[name] = vendor.read_regular(
                beneath(build.root / name, build.root), vendor.MAX_MANIFEST
            )
        return package, files, None
    matches = [tree for tree in integrity.trees if path == build.root / tree.path / "Cargo.toml"]
    if len(matches) != 1:
        raise NativeError("Unlisted local Rust package license source")
    return package, vendor.tree_files(path.parent), integrity.sources[matches[0].source].sha256


def archive_record(path: Path) -> JsonObject:
    """Reject thin archives; each receipt must identify self-contained library bytes."""
    return record_hash(path, archive=True)


def resolve_archive(build: Build, paths: Sequence[Path], library: str, out_dir: Path) -> Path:
    """Require one static archive in the recorded search path and its expected origin."""
    candidates = {
        (path / f"lib{library}.a").resolve()
        for path in paths
        if (path / f"lib{library}.a").is_file()
    }
    if len(candidates) != 1:
        raise NativeError("Missing or ambiguous native static archive")
    selected = candidates.pop()
    if library in {"ssl", "crypto"}:
        return beneath(selected, build.openssl_prefix / "lib")
    if library == "stdc++":
        return beneath(selected, SYSTEM_ROOT)
    return beneath(selected, out_dir)


def link_archives(
    build: Build, graph: CargoBuild, manifest: JsonObject
) -> tuple[list[JsonValue], Path]:
    """Resolve exactly the reviewed static libraries from actual Cargo link declarations."""
    providers = object_value(manifest["native_links"])
    seen: set[str] = set()
    records: list[JsonValue] = []
    runtime: Path | None = None
    for (identifier, _), event in sorted(graph.scripts.items()):
        libraries = strings(event["linked_libs"])
        if not libraries:
            continue
        name = package_name(identifier)
        if name not in providers or identifier not in graph.artifacts:
            raise NativeError("Unlisted native link provider or missing compiler artifact")
        seen.add(name)
        provider = object_value(providers[name])
        expected = strings(provider["static"])
        allowed = {"static=" + item for item in expected} | set(strings(provider["system"]))
        if set(libraries) - allowed or not {"static=" + item for item in expected} <= set(
            libraries
        ):
            raise NativeError("Native link declarations differ from reviewed policy")
        out_dir = beneath(Path(text(event, "out_dir")), build.root / "target/release/build")
        paths = [Path(item.removeprefix("native=")) for item in strings(event["linked_paths"])]
        if any(not path.is_absolute() for path in paths):
            raise NativeError("Native linker search path must be absolute")
        for library in expected:
            selected = resolve_archive(build, paths, library, out_dir)
            if library == "stdc++":
                runtime = selected
            record = archive_record(selected)
            record.update({"provider": name, "library": library, "out_dir": str(out_dir)})
            records.append(record)
    if seen != set(providers) or runtime is None:
        raise NativeError("Missing native link provider or static C++ runtime")
    return records, runtime


def command(argv: Sequence[str], *, limit: int = MAX_REPORT) -> str:
    """Run fixed build-host inspection tools under process/output budgets."""
    status, output, error = bounded_process.run(
        argv, limits=bounded_process.Limits(timeout=30, stdout=limit, stderr=65536)
    )
    if status != 0 or error:
        raise NativeError("Build-host provenance command failed")
    return output.decode("utf-8")


def builder_rpm_records(lines: Sequence[str]) -> tuple[list[str], list[str]]:
    """Separate RPM 6 signing-key pseudo records from source-backed build packages.

    A gpg-pubkey database entry has no architecture or source RPM because it is
    an imported signing key, not installed software. Preserve its full fingerprint
    and creation timestamp, while requiring every ordinary package's source RPM.
    """
    packages: list[str] = []
    signing_keys: list[str] = []
    if len(set(lines)) != len(lines):
        raise NativeError("Duplicate builder RPM provenance")
    for line in sorted(lines):
        parts = line.split("\t")
        if len(parts) != RPM_FIELDS or not all(parts):
            raise NativeError("Incomplete builder RPM provenance")
        if parts[0] == "gpg-pubkey":
            if (
                re.fullmatch(r"gpg-pubkey\t0:[a-f0-9]{40}-[a-f0-9]{8}\.\(none\)\t\(none\)", line)
                is None
            ):
                raise NativeError("Invalid builder RPM signing-key provenance")
            signing_keys.append(line)
        elif parts[2].endswith(".src.rpm"):
            packages.append(line)
        else:
            raise NativeError("Incomplete builder RPM provenance")
    return packages, signing_keys


def toolchain_evidence(root: Path, runtime: Path) -> JsonObject:
    """Record actual compiler identity and builder RPM/source-RPM ownership."""
    rust = command(["rustc", "--version", "--verbose"], limit=65536)
    package = object_value(toml(root / "Cargo.toml")["package"])
    if f"release: {text(package, 'rust-version')}\n" not in rust:
        raise NativeError("Rust compiler differs from the reviewed production version")
    compiler = command(["c++", "--version"], limit=65536)
    rpm_format = "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\t%{SOURCERPM}\n"
    packages, signing_keys = builder_rpm_records(
        command(["rpm", "--query", "--all", "--queryformat", rpm_format]).splitlines()
    )
    owner = command(
        ["rpm", "--query", "--file", str(runtime), "--queryformat", rpm_format]
    ).splitlines()
    if len(owner) != 1 or not owner[0].startswith("libstdc++-static\t") or owner[0] not in packages:
        raise NativeError("Static C++ runtime is not owned by the recorded builder RPM")
    runtime_license = command(
        ["rpm", "--query", "--file", str(runtime), "--queryformat", "%{LICENSE}\n"], limit=65536
    ).strip()
    _ = reviewed_license({"license": runtime_license})
    return {
        "rustc": rust,
        "cxx": compiler,
        "builder_rpms": list[JsonValue](packages),
        "builder_signing_keys": list[JsonValue](signing_keys),
        "static_cxx_owner": owner[0],
        "static_cxx_license": runtime_license,
    }


def produce(build: Build) -> None:
    """Write one deterministic receipt only after sources and actual link inputs agree."""
    manifest, integrity = validate_manifest(build.root)
    vendor_digest = validate_vendor_receipt(build, integrity)
    cargo_data = vendor.read_regular(build.cargo_messages, MAX_MESSAGES)
    graph = cargo_build(build.root, cargo_data)
    binary, embedded = binary_dependencies(graph.binary)
    registry = registry_component(build, graph, manifest["registry_component"])
    licenses = rust_licenses(build, graph, integrity, embedded)
    archives, runtime = link_archives(build, graph, manifest)
    toolchain = toolchain_evidence(build.root, runtime)
    openssl = object_value(manifest["openssl"])
    header = vendor.read_regular(
        build.openssl_prefix / "include/openssl/opensslv.h", vendor.MAX_MANIFEST
    ).decode()
    if f'# define OPENSSL_VERSION_STR "{text(openssl, "version")}"' not in header:
        raise NativeError("Built OpenSSL headers differ from the declared source version")
    report: JsonObject = {
        "native_manifest_sha256": vendor.sha256(
            vendor.read_regular(build.root / "vendor/native-components.json", vendor.MAX_MANIFEST)
        ),
        "vendor_report_sha256": vendor_digest,
        "cargo_messages_sha256": vendor.sha256(cargo_data),
        "cargo_lock_sha256": vendor.sha256(
            vendor.read_regular(build.root / "Cargo.lock", vendor.MAX_MANIFEST)
        ),
        "binary": binary,
        "wrap_components": manifest["wrap_components"],
        "adapted_component": manifest["adapted_component"],
        "openssl": openssl,
        "registry_component": registry,
        "rust_licenses": licenses,
        "rust_dependency_metadata": {
            key: embedded[key] for key in ("format", "sha256", "compressedSha256", "packageCount")
        },
        "vendor_sources": [
            {
                "id": source.identifier,
                "url": source.url,
                "sha256": source.sha256,
                "format": source.format,
            }
            for source in integrity.sources.values()
        ],
        "vendor_patches": [
            {
                "path": tree.path,
                "source": tree.source,
                "changes": [
                    {
                        "path": change.path,
                        "upstream_sha256": change.upstream_sha256,
                        "vendored_sha256": change.vendored_sha256,
                    }
                    for change in tree.changes
                ],
            }
            for tree in integrity.trees
        ],
        "static_archives": archives,
        "toolchain": toolchain,
        "limits": [
            "Static archive identity does not prove every member survives final linking.",
            "Header-only and adapted components are authenticated source inputs.",
            "Rust license evidence covers compiler events and the exact embedded dependency graph.",
            "Embedded metadata can include optional packages without compiler-artifact events.",
            "The unpublished first-party application has no inferred distribution license.",
            "Runtime-loaded libraries and deployed protections need separate checks.",
        ],
    }
    if record_hash(graph.binary) != binary:
        raise NativeError("Production binary changed before receipt publication")
    encoded = vendor.json_bytes(report)
    if len(encoded) > MAX_REPORT:
        raise NativeError("Native provenance report exceeds its byte budget")
    vendor.write_private(build.output, encoded)


@dataclass
class Arguments(argparse.Namespace):
    """Typed paths for the single current production build contract."""

    root: Path = Path(__file__).resolve().parents[1]
    vendor_report: Path = Path()
    cargo_messages: Path = Path()
    cargo_home: Path = Path()
    openssl_prefix: Path = Path()
    output: Path = Path()


def main(argv: Sequence[str] | None = None) -> int:
    """Produce native provenance without invoking a build or downloading dependencies."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--root", type=Path)
    for name in ("vendor-report", "cargo-messages", "cargo-home", "openssl-prefix", "output"):
        _ = parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args(argv, namespace=Arguments())
    try:
        produce(
            Build(
                args.root.resolve(),
                args.vendor_report,
                args.cargo_messages,
                args.cargo_home.resolve(),
                args.openssl_prefix.resolve(),
                args.output,
            )
        )
    except (ValueError, OSError, KeyError, EOFError, bounded_process.ProcessError) as error:
        print(f"Native provenance failed: {error}", file=sys.stderr)  # noqa: T201 -- CLI diagnostic.
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
