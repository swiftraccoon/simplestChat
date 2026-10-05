"""Authenticate the external AWS-LC build used by the unmodified Rust wrapper."""

# Fixed validation errors are the standalone native build-gate contract.
# ruff: noqa: EM101, TRY003
from __future__ import annotations

import gzip
import io
import re
import tarfile
from pathlib import Path
from typing import TYPE_CHECKING

import security_vendor as vendor
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

MAX_ARCHIVE = 192 * 1024 * 1024
MAX_EXPANDED = 384 * 1024 * 1024
MAX_MEMBER = 64 * 1024 * 1024

OPTIONS = {
    "CMAKE_BUILD_TYPE": "Release",
    "BUILD_SHARED_LIBS": "OFF",
    "BUILD_TESTING": "OFF",
    "BUILD_TOOL": "OFF",
    "BUILD_LIBSSL": "OFF",
    "ENABLE_SOURCE_MODIFICATION": "OFF",
    "BUILD_AWSLC_PROVIDER": "OFF",
    "ENABLE_CRYPTO_POLICIES": "OFF",
    "GENERATE_RUST_BINDINGS": "ON",
    "RUST_BINDINGS_TARGET_VERSION": "1.70",
    "BORINGSSL_PREFIX": "simplestchat_awslc_5_11_0",
    "CMAKE_POSITION_INDEPENDENT_CODE": "ON",
    "CMAKE_INSTALL_LIBDIR": "lib",
}
FILES = (
    "lib/libcrypto-awslc.a",
    "share/rust/aws_lc_bindings.rs",
    "include/openssl/base.h",
    "include/openssl/boringssl_prefix_symbols.h",
    "share/simplestchat/aws-lc-source.tar.gz",
    "share/simplestchat/bindgen-cli.crate",
    "share/simplestchat/CMakeCache.txt",
    "share/simplestchat/symbols.txt",
)


def validate(root: Path, component: JsonObject) -> None:
    """Bind native source, generator and namespace to the exact reviewed installer."""
    system = vendor.fields(
        component["system"],
        {
            "source",
            "source_license_sha256",
            "bindgen_source",
            "installer_sha256",
            "symbol_prefix",
            "configure_options",
        },
    )
    _ = vendor.digest(system["source_license_sha256"])
    source = vendor.parse_source(system["source"])
    generator = vendor.parse_source(system["bindgen_source"])
    version = string_value(component["version"])
    generator_version = generator.identifier.removeprefix("bindgen-cli-")
    installer = vendor.read_regular(root / "build/install-aws-lc.sh", vendor.MAX_MANIFEST)
    if (
        re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is None
        or source.identifier != "aws-lc-" + version
        or source.url != f"https://github.com/aws/aws-lc/archive/refs/tags/v{version}.tar.gz"
        or source.format != "tar.gz"
        or generator.identifier != "bindgen-cli-" + generator_version
        or re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", generator_version) is None
        or generator.url
        != f"https://static.crates.io/crates/bindgen-cli/bindgen-cli-{generator_version}.crate"
        or generator.format != "tar.gz"
        or vendor.sha256(installer) != vendor.digest(system["installer_sha256"])
        or system["symbol_prefix"] != OPTIONS["BORINGSSL_PREFIX"]
        or object_value(system["configure_options"]) != OPTIONS
    ):
        raise ValueError("AWS-LC external source or installer identity differs")
    for name, value in {
        "awslc_version": version,
        "awslc_sha256": source.sha256,
        "awslc_prefix": string_value(system["symbol_prefix"]),
        "bindgen_version": generator_version,
        "bindgen_sha256": generator.sha256,
    }.items():
        if f"{name}='{value}'\n".encode() not in installer:
            raise ValueError("AWS-LC installer pin differs from reviewed metadata")


def reserve_member(member: tarfile.TarInfo, seen: set[str], files: dict[str, bytes]) -> str:
    """Reject unsupported entries, name collisions and excessive member budgets."""
    if not (member.isfile() or member.isdir()) or member.sparse is not None:
        raise ValueError("AWS-LC unsupported archive member")
    key = vendor.relative_path(member.name.removesuffix("/"))
    if key in seen or len(seen) >= vendor.MAX_FILES:
        raise ValueError("AWS-LC duplicate or excessive archive members")
    if any(str(parent) in files for parent in Path(key).parents) or (
        member.isfile() and any(name.startswith(key + "/") for name in seen)
    ):
        raise ValueError("AWS-LC archive file/directory collision")
    seen.add(key)
    if member.size < 0 or member.size > MAX_MEMBER:
        raise ValueError("AWS-LC archive member exceeds its budget")
    return key


def release_files(source: vendor.Source, data: bytes) -> dict[str, bytes]:
    """Authenticate the larger release archive and bound every parsed member.

    Upstream includes compressed test vectors. Retain header/license bytes and
    every member name, without retaining hundreds of MiB of unused test data.
    """
    if len(data) > MAX_ARCHIVE or vendor.sha256(data) != source.sha256:
        raise ValueError("AWS-LC source SHA-256 or size differs")
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
        expanded = compressed.read(MAX_EXPANDED + 1)
    if len(expanded) > MAX_EXPANDED:
        raise ValueError("AWS-LC archive expansion budget exceeded")
    files: dict[str, bytes] = {}
    seen: set[str] = set()
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r|") as archive:
        for member in archive:
            key = reserve_member(member, seen, files)
            if member.isdir():
                if member.size:
                    raise ValueError("AWS-LC nonempty archive directory")
                continue
            stream = archive.extractfile(member)
            if stream is None:
                raise ValueError("AWS-LC unreadable archive member")
            with stream:
                body = stream.read(MAX_MEMBER + 1)
            if len(body) != member.size:
                raise ValueError("AWS-LC archive member size differs")
            files[key] = body if key.endswith(("/include/openssl/base.h", "/LICENSE")) else b""
    return files


def source_files(
    source: vendor.Source, archive: bytes, *, native_release: bool = False
) -> dict[str, bytes]:
    """Read complete checksum-authenticated source members under their exact release root."""
    files = (
        release_files(source, archive) if native_release else vendor.archive_files(source, archive)
    )
    prefix = source.identifier + "/"
    if any(not name.startswith(prefix) for name in files):
        raise ValueError("AWS-LC source archive prefix differs")
    return {name.removeprefix(prefix): body for name, body in files.items()}


def verify_options(cache: bytes) -> None:
    """Read the actual configured values instead of trusting only receipt assertions."""
    options: dict[str, str] = {}
    for line in cache.decode().splitlines():
        if line and not line.startswith(("#", "//")) and "=" in line and ":" in line:
            key, value = line.split("=", 1)
            options[key.split(":", 1)[0]] = value
    if {key: options.get(key) for key in OPTIONS} != OPTIONS:
        raise ValueError("AWS-LC actual CMake build options differ")


def evidence(
    prefix: Path,
    component: JsonObject,
    archives: Sequence[JsonValue],
    generated_bindings: Sequence[Path],
) -> JsonObject:
    """Bind authenticated source and generated bindings to the actual linked static archive."""
    system = object_value(component["system"])
    receipt = vendor.fields(
        decode_json(
            vendor.read_regular(prefix / "share/simplestchat/build.json", vendor.MAX_MANIFEST)
        ),
        {"configure_options", "installer_sha256", "bindgen_sha256", "files"},
    )
    hashes = object_value(receipt["files"])
    if (
        set(hashes) != set(FILES)
        or receipt["configure_options"] != system["configure_options"]
        or receipt["installer_sha256"] != system["installer_sha256"]
    ):
        raise ValueError("AWS-LC installed build receipt differs")
    _ = vendor.digest(receipt["bindgen_sha256"])
    contents = {name: vendor.read_regular(prefix / name, MAX_ARCHIVE) for name in FILES}
    for name, body in contents.items():
        if vendor.sha256(body) != vendor.digest(hashes[name]):
            raise ValueError("AWS-LC installed artifact differs from its build receipt")
    source = vendor.parse_source(system["source"])
    files = source_files(
        source, contents["share/simplestchat/aws-lc-source.tar.gz"], native_release=True
    )
    if "LICENSE" not in files or vendor.sha256(files["LICENSE"]) != system["source_license_sha256"]:
        raise ValueError("AWS-LC native source license differs")
    generator = vendor.parse_source(system["bindgen_source"])
    generator_files = source_files(generator, contents["share/simplestchat/bindgen-cli.crate"])
    if "Cargo.lock" not in generator_files:
        raise ValueError("AWS-LC binding generator lacks an authenticated lockfile")
    header = contents["include/openssl/base.h"]
    if (
        files.get("include/openssl/base.h") != header
        or f'#define AWSLC_VERSION_NUMBER_STRING "{component["version"]}"\n'.encode() not in header
        or b"#define OPENSSL_IS_AWSLC" not in header
    ):
        raise ValueError("AWS-LC installed version header differs from authenticated source")
    symbol_prefix = string_value(system["symbol_prefix"])
    prefix_header = contents["include/openssl/boringssl_prefix_symbols.h"]
    if f"#define BORINGSSL_PREFIX {symbol_prefix}\n".encode() not in prefix_header:
        raise ValueError("AWS-LC installed symbol namespace differs")
    verify_options(contents["share/simplestchat/CMakeCache.txt"])
    bindings = contents["share/rust/aws_lc_bindings.rs"]
    if (
        len(generated_bindings) != 1
        or vendor.read_regular(generated_bindings[0], vendor.MAX_DOWNLOAD) != bindings
        or f"{symbol_prefix}_EVP_Digest".encode() not in bindings
        or f"{symbol_prefix}_OpenSSL_version".encode() not in bindings
    ):
        raise ValueError("Cargo AWS-LC bindings differ from the installed namespace")
    libraries = [
        object_value(item)
        for item in archives
        if object_value(item).get("provider") == "aws-lc-sys"
    ]
    if (
        len(libraries) != 1
        or libraries[0]["library"] != "crypto-awslc"
        or libraries[0]["sha256"] != hashes["lib/libcrypto-awslc.a"]
        or not contents["lib/libcrypto-awslc.a"].startswith(b"!<arch>\n")
    ):
        raise ValueError("Cargo AWS-LC archive differs from the installed static library")
    return {
        "native_source": system["source"],
        "native_verified_files": len(files),
        "native_license_sha256": system["source_license_sha256"],
        "bindgen_source": system["bindgen_source"],
        "build": receipt,
        "bindings_sha256": hashes["share/rust/aws_lc_bindings.rs"],
    }
