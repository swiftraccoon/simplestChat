"""Apply reviewed upstream advisories to exact observed RPM identities.

This supplements scanner results; it does not manufacture Grype matches or
infer application reachability. Unsupported identities and versions fail closed.
"""

from __future__ import annotations

import hashlib
import re
from typing import TYPE_CHECKING
from urllib.parse import parse_qs, urlsplit

import security_tools
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

POLICY_PATH = security_tools.ROOT / "security/advisories/openssl-cve-2026-84782.json"
MAX_POLICY = 16384
MAX_PACKAGES = 20000
MAX_EPOCH = 4294967295
MAX_VERSION = 32
FIXED_PATCH = 9


def require(condition: object, reason: str) -> None:
    """Publish fixed failure codes, never arbitrary package-derived error strings."""
    security_tools.require(condition, reason)


def reviewed_policy(path: Path) -> tuple[JsonObject, str]:
    """Read the single reviewed applicability contract and bind its complete bytes."""
    body = security_tools.bounded_file(path, MAX_POLICY)
    value = object_value(decode_json(body.decode()))
    require(
        set(value)
        == {"schemaVersion", "id", "severity", "reviewedOn", "scope", "sources", "assessment"}
        and type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["id"] == "CVE-2026-84782"
        and value["severity"] == "High"
        and value["reviewedOn"] == "2026-09-30"
        and value["scope"]
        == {
            "distribution": "fedora-44",
            "sourcePackage": "openssl",
            "series": "3.5",
            "affectedFrom": "3.5.0",
            "fixedVersion": "3.5.9",
        }
        and value["sources"]
        == [
            {
                "url": "https://openssl-library.org/news/secjson/cve-2026-84782.json",
                "sha256": "281dcd4742dedb86b29381be85d1e7cfaa0a5af0583fadb0a7dec6e7c903f1ad",
            },
            {
                "url": "https://openssl-library.org/news/secadv/20260929.txt",
                "sha256": "2a6766a213b2dac48b4666aa54c39dbe3af96f73444081a4a25c1e3dc496aeb9",
            },
        ]
        and bool(string_value(value["assessment"])),
        "image_advisory_policy",
    )
    return value, hashlib.sha256(body).hexdigest()


def openssl_candidate(package: JsonObject) -> bool:
    """Inspect name, source package and PURL so an inconsistent identity cannot evade review."""
    if package.get("type") != "rpm":
        return False
    name = string_value(package["name"])
    metadata = object_value(package.get("metadata", {}))
    source = string_value(metadata.get("sourceRpm", ""))
    scope = urlsplit(string_value(package.get("purl", "")))
    return (
        name == "openssl"
        or name.startswith("openssl-")
        or source.startswith("openssl-")
        or scope.path.startswith(("rpm/fedora/openssl@", "rpm/fedora/openssl-"))
    )


def openssl_identity(package: JsonObject) -> tuple[str, int]:
    """Require agreeing Syft NEVRA, source RPM and qualified Fedora PURL fields."""
    metadata = object_value(package["metadata"])
    name = string_value(package["name"])
    version = string_value(metadata["version"])
    release = string_value(metadata["release"])
    epoch = metadata["epoch"]
    architecture = metadata["architecture"]
    source = string_value(metadata["sourceRpm"])
    require(
        package["metadataType"] == "rpm-db-entry"
        and metadata["name"] == name
        and re.fullmatch(r"openssl(?:-[a-z0-9][a-z0-9+-]{0,63})?", name)
        and re.fullmatch(r"[0-9]+(?:\.[0-9]+){2}", version)
        and len(version) <= MAX_VERSION
        and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+~^]{0,127}", release)
        and type(epoch) is int
        and 0 <= epoch <= MAX_EPOCH
        and architecture in {"aarch64", "x86_64", "noarch"}
        and source == f"openssl-{version}-{release}.src.rpm",
        "image_advisory_rpm_identity",
    )
    assert type(epoch) is int  # noqa: S101 -- Narrowing after the strict identity check.
    version_release = f"{version}-{release}"
    require(
        package["version"] == (f"{epoch}:" if epoch else "") + version_release,
        "image_advisory_rpm_version",
    )
    scope = string_value(package["purl"])
    parsed = urlsplit(scope)
    qualifiers = parse_qs(parsed.query, keep_blank_values=True, strict_parsing=True)
    require(
        parsed.scheme == "pkg"
        and not parsed.netloc
        and not parsed.fragment
        and parsed.path == f"rpm/fedora/{name}@{version_release}"
        and qualifiers
        == {
            "arch": [architecture],
            "distro": ["fedora-44"],
            "upstream": [source],
            **({"epoch": [str(epoch)]} if epoch else {}),
        },
        "image_advisory_purl_identity",
    )
    parts = version.split(".")
    require(
        parts[:2] == ["3", "5"] and str(int(parts[2])) == parts[2],
        "image_advisory_series_unreviewed",
    )
    return scope, int(parts[2])


def verdict(packages: Sequence[JsonObject], *, path: Path = POLICY_PATH) -> JsonObject:
    """Block affected installed OpenSSL packages even when Grype has no mapping."""
    require(len(packages) <= MAX_PACKAGES, "image_advisory_inventory_bound")
    policy, policy_digest = reviewed_policy(path)
    blocked: list[JsonValue] = []
    assessed: list[JsonValue] = []
    for package in packages:
        if not openssl_candidate(package):
            continue
        scope, patch = openssl_identity(package)
        affected = patch < FIXED_PATCH
        assessed.append({"scope": scope, "affected": affected})
        if affected:
            blocked.append(
                {
                    "id": policy["id"],
                    "scope": scope,
                    "severity": policy["severity"],
                    "source": "reviewed-openssl-advisory",
                    "upstreamFixedVersion": "3.5.9",
                }
            )
    return {
        "passed": not blocked,
        "policySha256": policy_digest,
        "sources": policy["sources"],
        "scope": policy["scope"],
        "assessed": assessed,
        "blocked": blocked,
    }
