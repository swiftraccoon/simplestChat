"""Assess complete image reports without equating successful scanner exit with coverage.

The operating-system allowlist and license expressions are reviewed repository
policy. Per-finding waivers use the shared exact-scope, expiring exception list.
Unknown licenses, missing inventories and stale databases fail closed.
"""

from __future__ import annotations

import hashlib
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import parse_qs, quote, urlencode, urlsplit

import security_tools
from security_policy import permitted

# isort: split
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

    from security_policy import ExceptionRecord

MAX_REPORT = 64 * 1024 * 1024
MAX_PACKAGES = 20000
MAX_MATCHES = 100000
MAX_EXPRESSION = 4096
MAX_LICENSE_DEPTH = 32
RPM_FIELDS = 3
MIN_DATABASE_HOURS = 1
MAX_DATABASE_HOURS = 120
TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+:-]*|[()]")


def require(condition: object, reason: str) -> None:
    """Use fixed diagnostics rather than untrusted scanner content."""
    security_tools.require(condition, reason)


def report(path: Path, *, limit: int = MAX_REPORT) -> JsonValue:
    """Read one bounded non-symlink JSON artifact with duplicate-key rejection."""
    return decode_json(security_tools.bounded_file(path, limit).decode())


def load_policy(path: Path) -> JsonObject:
    """Validate every current policy field; misspelled restrictions cannot be ignored."""
    policy = object_value(report(path, limit=65536))
    require(
        set(policy)
        == {
            "scannerBase",
            "distribution",
            "maximumDatabaseAgeHours",
            "blockedSeverities",
            "allowedLicenses",
            "firstParty",
        },
        "image_policy_fields",
    )
    require(
        re.fullmatch(
            r"docker.io/library/fedora:44@sha256:[a-f0-9]{64}", string_value(policy["scannerBase"])
        ),
        "image_scanner_base",
    )
    require(policy["distribution"] == {"id": "fedora", "version": "44"}, "image_os_policy")
    hours = policy["maximumDatabaseAgeHours"]
    require(
        type(hours) is int and MIN_DATABASE_HOURS <= hours <= MAX_DATABASE_HOURS,
        "image_database_age_policy",
    )
    require(policy["blockedSeverities"] == ["High", "Critical"], "image_severity_policy")
    require(
        policy["firstParty"]
        == {
            "name": "simplestChat",
            "source": "local",
            "license": "NOASSERTION",
            "cargoPublish": False,
            "policy": "unpublished-application",
        },
        "image_first_party_policy",
    )
    licenses = array_value(policy["allowedLicenses"])
    require(
        bool(licenses) and len(licenses) == len({string_value(x) for x in licenses}),
        "image_license_policy",
    )
    return policy


class LicenseExpression:
    """Evaluate SPDX AND/OR/WITH with bounded grammar and explicit reviewed atoms."""

    def __init__(self, expression: str, allowed: set[str]) -> None:
        """Tokenize a bounded SPDX expression without evaluating arbitrary text."""
        require(0 < len(expression) <= MAX_EXPRESSION, "image_license_expression")
        self.tokens: list[str] = TOKEN.findall(expression)
        require("".join(self.tokens) == re.sub(r"\s+", "", expression), "image_license_grammar")
        self.position: int = 0
        self.allowed: set[str] = {self.identifier(item) for item in allowed}

    @staticmethod
    def identifier(value: str) -> str:
        """Match SPDX license/exception IDs case-insensitively; preserve custom references."""
        return value if value.startswith(("LicenseRef-", "DocumentRef-")) else value.casefold()

    def take(self, token: str) -> bool:
        """Consume one exact grammar token."""
        if self.position < len(self.tokens) and self.tokens[self.position] == token:
            self.position += 1
            return True
        return False

    def atom(self, depth: int) -> bool:
        """Require parentheses and WITH terms to remain well formed."""
        require(
            depth < MAX_LICENSE_DEPTH and self.position < len(self.tokens), "image_license_depth"
        )
        if self.take("("):
            result = self.choice(depth + 1)
            require(self.take(")"), "image_license_parenthesis")
            return result
        name = self.tokens[self.position]
        require(name not in {"AND", "OR", "WITH", ")"}, "image_license_atom")
        self.position += 1
        if self.take("WITH"):
            require(self.position < len(self.tokens), "image_license_exception")
            name += " WITH " + self.tokens[self.position]
            self.position += 1
        return self.identifier(name) in self.allowed

    def conjunction(self, depth: int) -> bool:
        """Every conjunct requires an allowed license; do not short circuit parsing."""
        result = self.atom(depth)
        while self.take("AND"):
            next_result = self.atom(depth)
            result = result and next_result
        return result

    def choice(self, depth: int = 0) -> bool:
        """Accept a permitted alternative in a well-formed SPDX choice."""
        result = self.conjunction(depth)
        while self.take("OR"):
            next_result = self.conjunction(depth)
            result = result or next_result
        return result

    def allowed_expression(self) -> bool:
        """Reject trailing tokens even when an earlier alternative was allowed."""
        result = self.choice()
        require(self.position == len(self.tokens), "image_license_trailing_tokens")
        return result


def database_status(value: JsonObject, policy: JsonObject, *, now: datetime | None = None) -> None:
    """Require current authenticated Grype database evidence, including a recent build."""
    require(value.get("valid") is True, "image_database_invalid")
    require(
        re.fullmatch(r"v6\.[0-9]{1,3}\.[0-9]{1,3}", string_value(value["schemaVersion"])),
        "image_database_schema",
    )
    source = urlsplit(string_value(value["from"]))
    checksum = parse_qs(source.query).get("checksum", [])
    require(
        source.scheme == "https"
        and source.hostname == "grype.anchore.io"
        and source.path.startswith("/databases/v6/")
        and len(checksum) == 1
        and re.fullmatch(r"sha256:[a-f0-9]{64}", checksum[0]),
        "image_database_source",
    )
    built = datetime.fromisoformat(string_value(value["built"]))
    require(built.tzinfo is not None, "image_database_timezone")
    age = ((now or datetime.now(UTC)) - built).total_seconds()
    hours = policy["maximumDatabaseAgeHours"]
    require(type(hours) is int and 0 <= age <= hours * 3600, "image_database_stale")


def inventory(value: JsonObject, policy: JsonObject) -> list[JsonObject]:
    """Require Fedora RPM and auditable Rust coverage, not just an empty successful report."""
    distro = object_value(value["distro"])
    require(
        {"id": distro.get("id"), "version": distro.get("versionID")} == policy["distribution"],
        "image_distribution_unsupported",
    )
    descriptor = object_value(value["descriptor"])
    require(
        descriptor.get("name") == "syft" and descriptor.get("version") == "1.52.0",
        "image_sbom_tool_identity",
    )
    packages = [object_value(item) for item in array_value(value["artifacts"])]
    require(0 < len(packages) <= MAX_PACKAGES, "image_inventory_empty")
    require(any(item.get("type") == "rpm" for item in packages), "image_rpm_inventory_missing")
    rust = [item for item in packages if item.get("type") == "rust-crate"]
    require(
        any(item.get("name") == "simplestChat" for item in rust), "image_rust_inventory_missing"
    )
    require(
        all(
            string_value(item["name"])
            and string_value(item["version"])
            and string_value(item["id"])
            for item in packages
        ),
        "image_package_identity",
    )
    require(
        len({string_value(item["id"]) for item in packages}) == len(packages),
        "image_package_duplicate",
    )
    return packages


def vulnerability_verdict(value: JsonObject, exceptions: Sequence[ExceptionRecord]) -> JsonObject:
    """Include unfixed high/critical matches and fail unknown severity or ignored results."""
    descriptor = object_value(value["descriptor"])
    require(
        descriptor.get("name") == "grype" and descriptor.get("version") == "0.119.0",
        "image_vulnerability_tool_identity",
    )
    require(not array_value(value.get("ignoredMatches", [])), "image_ignored_vulnerabilities")
    matches = array_value(value["matches"])
    require(len(matches) <= MAX_MATCHES, "image_vulnerability_count")
    blocked: list[JsonValue] = []
    waived: list[JsonValue] = []
    for item in matches:
        match = object_value(item)
        vulnerability, package = (
            object_value(match["vulnerability"]),
            object_value(match["artifact"]),
        )
        severity = string_value(vulnerability["severity"])
        require(
            severity in {"Negligible", "Low", "Medium", "High", "Critical", "Unknown"},
            "image_vulnerability_severity",
        )
        if severity not in {"High", "Critical", "Unknown"}:
            continue
        scope = string_value(package["purl"])
        fingerprint = string_value(vulnerability["id"])
        finding: JsonObject = {
            "id": fingerprint,
            "scope": scope,
            "severity": severity,
            "fixState": object_value(vulnerability["fix"]).get("state"),
        }
        (waived if permitted(exceptions, "grype", fingerprint, scope) else blocked).append(finding)
    return {"passed": not blocked, "matches": len(matches), "blocked": blocked, "waived": waived}


def secret_verdict(
    value: JsonValue, exceptions: Sequence[ExceptionRecord], paths: JsonObject
) -> JsonObject:
    """Never copy candidate secret text or scanner Match/Secret fields into public evidence."""
    findings = array_value(value)
    require(len(findings) <= MAX_MATCHES, "image_secret_count")
    blocked: list[JsonValue] = []
    waived: list[JsonValue] = []
    for item in findings:
        finding = object_value(item)
        entry = string_value(finding["File"]).removeprefix("/layers/")
        require(entry in paths, "image_secret_file_unknown")
        identity = object_value(paths[entry])
        path = string_value(identity["path"])
        content_hash = string_value(identity["sha256"])
        require(re.fullmatch(r"[a-f0-9]{64}", content_hash), "image_secret_file_digest")
        require(not path.startswith("/") and ".." not in Path(path).parts, "image_secret_path")
        rule = string_value(finding["RuleID"])
        line = finding["StartLine"]
        require(type(line) is int and line > 0, "image_secret_line")
        fingerprint = f"{rule}:{path}:{line}:{content_hash}"
        record: JsonObject = {"rule": rule, "path": path, "line": line, "fingerprint": fingerprint}
        (waived if permitted(exceptions, "gitleaks", fingerprint, path) else blocked).append(record)
    return {
        "passed": not blocked,
        "blocked": blocked,
        "waived": waived,
        "scope": "every regular layer file, including deleted files, and complete image config",
    }


def license_verdict(
    packages: list[JsonObject], policy: JsonObject, exceptions: Sequence[ExceptionRecord]
) -> JsonObject:
    """Unknown and unreviewed license expressions require a scoped explicit review."""
    allowed = {string_value(item) for item in array_value(policy["allowedLicenses"])}
    blocked: list[JsonValue] = []
    waived: list[JsonValue] = []
    for package in packages:
        expressions = [
            string_value(object_value(item).get("spdxExpression", ""))
            for item in array_value(package["licenses"])
        ]
        expression = " AND ".join("(" + item + ")" for item in expressions if item)
        approved = bool(expression) and LicenseExpression(expression, allowed).allowed_expression()
        if package.get("name") == "simplestChat" and package.get("type") == "rust-crate":
            approved = expressions == ["NOASSERTION"]
        if approved:
            continue
        scope = string_value(
            package.get("purl")
            or (string_value(package["name"]) + "@" + string_value(package["version"]))
        )
        fingerprint = "license:" + hashlib.sha256(expression.encode()).hexdigest()
        finding: JsonObject = {
            "scope": scope,
            "expression": expression or "UNKNOWN",
            "fingerprint": fingerprint,
        }
        (waived if permitted(exceptions, "image-license", fingerprint, scope) else blocked).append(
            finding
        )
    return {"passed": not blocked, "packages": len(packages), "blocked": blocked, "waived": waived}


def rust_license_join(packages: list[JsonObject], native: JsonObject, elf: JsonObject) -> None:
    """Join binary-observed Rust identities to authenticated build-source license records."""
    graph = [object_value(item) for item in array_value(object_value(elf["auditable"])["packages"])]
    built = [object_value(item) for item in array_value(native["rust_licenses"])]
    seen: set[tuple[str, str]] = set()
    expected = {
        (string_value(item["name"]), string_value(item["version"]))
        for item in graph
        if item.get("kind", "runtime") == "runtime"
    }
    for package in packages:
        if package.get("type") != "rust-crate":
            continue
        identity = (string_value(package["name"]), string_value(package["version"]))
        matches = [item for item in graph if (item["name"], item["version"]) == identity]
        require(len(matches) == 1, "image_rust_binary_identity")
        source = string_value(matches[0]["source"])
        licenses = [
            item
            for item in built
            if (item["name"], item["version"]) == identity
            and source in {"local", "crates.io"}
            and item["source"] == source
        ]
        require(len(licenses) == 1, "image_rust_license_source")
        license_record = licenses[0]
        expression = object_value(license_record["license"])["expression"]
        if license_record["first_party"] is True:
            require(
                identity[0] == "simplestChat"
                and source == "local"
                and expression is None
                and license_record["cargo_publish"] is False,
                "image_first_party_license_identity",
            )
            # This describes publication policy; it does not grant a license.
            expression = "NOASSERTION"
        package["licenses"] = (
            []
            if expression is None
            else [
                {
                    "value": expression,
                    "spdxExpression": expression,
                    "type": "declared",
                    "urls": [],
                    "locations": [],
                }
            ]
        )
        seen.add(identity)
    require(expected <= seen, "image_rust_runtime_coverage")


def static_rpm(package: JsonObject, owner: list[str]) -> JsonObject:
    """Keep a compiled static RPM's exact Fedora identity usable by RPM advisory matching."""
    epoch, separator, nevra = owner[1].partition(":")
    version_release, _, architecture = nevra.rpartition(".")
    version, dash, release = version_release.rpartition("-")
    require(
        separator
        and dash
        and epoch.isdecimal()
        and version
        and release
        and architecture in {"x86_64", "aarch64"}
        and owner[2].endswith(".src.rpm"),
        "image_static_rpm_nevra",
    )
    package["type"] = "rpm"
    package["version"] = version_release
    package["purl"] = (
        "pkg:rpm/fedora/"
        + quote(owner[0], safe="")
        + "@"
        + quote(version_release, safe="")
        + "?"
        + urlencode(
            {
                "arch": architecture,
                "distro": "fedora-44",
                "upstream": owner[2],
                **({"epoch": epoch} if int(epoch) else {}),
            }
        )
    )
    package["metadataType"] = "rpm-db-entry"
    package["metadata"] = {
        "name": owner[0],
        "version": version,
        "epoch": int(epoch),
        "release": release,
        "architecture": architecture,
        "sourceRpm": owner[2],
        "files": [],
    }
    return package


def native_packages(native: JsonObject) -> list[JsonObject]:
    """Inventory authenticated native inputs without inventing vulnerability applicability."""
    components = [
        object_value(item)
        for item in array_value(native["wrap_components"])
        if object_value(item)["usage"] == "production"
    ]
    components += [
        object_value(native["adapted_component"]),
        {"name": "OpenSSL", **object_value(native["openssl"])},
        object_value(native["registry_component"]),
    ]
    toolchain = object_value(native["toolchain"])
    owner = string_value(toolchain["static_cxx_owner"]).split("\t")
    require(len(owner) == RPM_FIELDS, "image_static_cxx_identity")
    components.append(
        {"name": owner[0], "version": owner[1], "license": toolchain["static_cxx_license"]}
    )
    result: list[JsonObject] = []
    for component in components:
        name, version = string_value(component["name"]), string_value(component["version"])
        expression = string_value(component["license"])
        identifier = hashlib.sha256((name + "\0" + version).encode()).hexdigest()[:16]
        result.append(
            {
                "id": "native-" + identifier,
                "name": name,
                "version": version,
                "type": "binary",
                "foundBy": "simplestchat-authenticated-native-inputs",
                "locations": [
                    {
                        "path": "/app/simplestChat",
                        "accessPath": "/app/simplestChat",
                        "annotations": {"evidence": "static-build-input"},
                    }
                ],
                "licenses": [
                    {
                        "value": expression,
                        "spdxExpression": expression,
                        "type": "declared",
                        "urls": [],
                        "locations": [],
                    }
                ],
                "language": "c++",
                "cpes": [],
                "purl": "",
                "metadataType": "",
                "metadata": None,
            }
        )
    result[-1] = static_rpm(result[-1], owner)
    return result


def enrich_sbom(
    value: JsonObject, native: JsonObject, elf: JsonObject, image_policy: JsonObject
) -> JsonObject:
    """Preserve scanner observations while adding separately identified build evidence."""
    packages = inventory(value, image_policy)
    rust_license_join(packages, native, elf)
    supplemental = native_packages(native)
    require(
        not (
            {string_value(item["id"]) for item in supplemental}
            & {string_value(item["id"]) for item in packages}
        ),
        "image_native_package_collision",
    )
    value["artifacts"] = list[JsonValue]([*packages, *supplemental])
    # These raw JSON blobs can contain environment/history secrets. Their
    # cryptographic identities remain; publishable SBOMs do not copy the blobs.
    metadata = object_value(object_value(value["source"])["metadata"])
    _ = metadata.pop("config", None)
    _ = metadata.pop("manifest", None)
    return value


def runtime_rpm_bindings(packages: list[JsonObject], elf: JsonObject) -> list[JsonValue]:
    """Bind every resolved runtime shared library to its image RPM and recorded digest."""
    result: list[JsonValue] = []
    for value in array_value(elf["libraries"]):
        library = object_value(value)
        owners: list[JsonObject] = []
        for package in packages:
            if package["type"] != "rpm":
                continue
            metadata = object_value(package["metadata"])
            for item in array_value(metadata["files"]):
                file = object_value(item)
                if file["path"] != library["path"]:
                    continue
                recorded = object_value(file["digest"])
                require(
                    recorded["algorithm"] == "sha256" and recorded["value"] == library["sha256"],
                    "image_runtime_rpm_digest",
                )
                owners.append(
                    {
                        "path": library["path"],
                        "sha256": library["sha256"],
                        "package": package["purl"],
                        "sourceRpm": metadata["sourceRpm"],
                    }
                )
        require(len(owners) == 1, "image_runtime_rpm_owner")
        result.append(owners[0])
    require(bool(result), "image_runtime_rpm_empty")
    return result


def vulnerability_database_binding(value: JsonObject, database: JsonObject) -> None:
    """Require the offline scan to use the validated DB and supported providers without filters."""
    descriptor = object_value(value["descriptor"])
    db = object_value(descriptor["db"])
    status = object_value(db["status"])
    require(
        all(
            status.get(key) == database[key] for key in ("schemaVersion", "from", "built", "valid")
        ),
        "image_vulnerability_database_changed",
    )
    providers = object_value(db["providers"])
    require({"fedora", "github", "nvd"} <= set(providers), "image_vulnerability_providers_missing")
    config = object_value(descriptor["configuration"])
    require(
        config["only-fixed"] is False
        and config["only-notfixed"] is False
        and config["ignore-wontfix"] == "",
        "image_vulnerability_filter",
    )
