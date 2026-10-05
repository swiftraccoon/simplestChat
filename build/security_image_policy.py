"""Assess complete image reports without equating successful scanner exit with coverage.

The operating-system allowlist and license expressions are reviewed repository
policy. Per-finding waivers use the shared exact-scope, expiring exception list.
Unknown licenses, missing inventories and stale databases fail closed.
"""

from __future__ import annotations

import hashlib
import json
import re
import stat
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING
from urllib.parse import parse_qs, quote, urlencode, urlsplit

import security_elf
import security_image_advisories
import security_secret_spans
import security_tools
from security_license_identity import raw_license_identity
from security_policy import permitted
from security_secret_projection import FORMAT as SECRET_PROJECTION_FORMAT

# isort: split
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Sequence

    from security_policy import ExceptionRecord

MAX_REPORT = 64 * 1024 * 1024
MAX_PACKAGES = 20000
MAX_NATIVE_INSTANCES = 20000
MAX_MATCHES = 100000
MAX_SECRET_PATH = 4096
MAX_SECRET_IDENTITY = 32768
MAX_EXPRESSION = 4096
MAX_LICENSE_DEPTH = 32
RPM_FIELDS = 3
OPENSSL_LIBRARIES = ("ssl", "crypto")
OPENSSL_PROVIDERS = frozenset({"mediasoup-sys", "openssl-sys"})
MIN_DATABASE_HOURS = 1
MAX_DATABASE_HOURS = 120
TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+:-]*|[()]")
# Exact declarations observed in authenticated, currently locked registry archives.
# Keep SPDX parsing strict; this is an adapter for the identified Cargo input fields.
CARGO_LICENSE_ALTERNATIVES = {
    "MIT/Apache-2.0": "MIT OR Apache-2.0",
    "Apache-2.0/MIT": "Apache-2.0 OR MIT",
    "Apache-2.0 / MIT": "Apache-2.0 OR MIT",
    "Unlicense/MIT": "Unlicense OR MIT",
}


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
        descriptor.get("name") == "syft"
        and descriptor.get("version") == security_tools.load_lock()[0]["syft"].version,
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


def vulnerability_verdict(
    value: JsonObject, exceptions: Sequence[ExceptionRecord], packages: Sequence[JsonObject]
) -> JsonObject:
    """Retain scanner matches and independently block reviewed known-advisory gaps."""
    descriptor = object_value(value["descriptor"])
    require(
        descriptor.get("name") == "grype"
        and descriptor.get("version") == security_tools.load_lock()[0]["grype"].version,
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
    advisories = security_image_advisories.verdict(packages)
    blocked.extend(array_value(advisories["blocked"]))
    return {
        "passed": not blocked,
        "matches": len(matches),
        "blocked": blocked,
        "waived": waived,
        "reviewedAdvisories": advisories,
    }


def secret_fingerprint(span: JsonObject) -> str:
    """Identify only an exact, uniquely resolved current-format match, never a captured group."""
    require(span.get("status") == "resolved", "image_secret_span_unresolved")
    size, checksum = span["spanBytes"], string_value(span["spanSha256"])
    require(
        type(size) is int and 0 < size <= security_secret_spans.MAX_FILE,
        "image_secret_span_size",
    )
    require(re.fullmatch(r"[a-f0-9]{64}", checksum), "image_secret_span_digest")
    identity: JsonObject = {
        "format": security_secret_spans.FORMAT,
        "projectionFormat": SECRET_PROJECTION_FORMAT,
        "rule": span["rule"],
        "path": span["path"],
        "spanBytes": size,
        "spanSha256": checksum,
    }
    encoded = json.dumps(
        identity, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode()
    require(len(encoded) <= MAX_SECRET_IDENTITY, "image_secret_identity_size")
    return (
        "image-match:" + hashlib.sha256(b"simplestchat-image-secret-match\0" + encoded).hexdigest()
    )


def secret_span_binding(finding: JsonObject, paths: JsonObject, span: JsonObject) -> JsonObject:
    """Bind each computed span to the corresponding scanner row and complete current path map."""
    entry = string_value(finding["File"]).removeprefix("/layers/")
    require(
        re.fullmatch(r"content-[0-9]{6}", entry) and entry in paths, "image_secret_file_unknown"
    )
    identity = object_value(paths[entry])
    path, rule = string_value(identity["path"]), string_value(finding["RuleID"])
    require(
        0 < len(path.encode()) <= MAX_SECRET_PATH
        and not path.startswith("/")
        and PurePosixPath(path).as_posix() == path
        and ".." not in PurePosixPath(path).parts
        and not re.search(r"[\x00-\x1f\x7f]", path),
        "image_secret_path",
    )
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", rule), "image_secret_rule")
    require(identity["projectionFormat"] == SECRET_PROJECTION_FORMAT, "image_secret_projection")
    expected: JsonObject = {
        "rule": rule,
        "path": path,
        "fileSha256": identity["sha256"],
        "projectionSha256": identity["projectionSha256"],
    }
    for field in ("fileSha256", "projectionSha256"):
        require(
            re.fullmatch(r"[a-f0-9]{64}", string_value(expected[field])), "image_secret_file_digest"
        )
    for field in security_secret_spans.COORDINATES:
        value = finding[field]
        require(
            type(value) is int and 0 <= value <= security_secret_spans.MAX_FILE,
            "image_secret_coordinates",
        )
        expected[field[0].lower() + field[1:]] = value
    require(
        all(
            type(span.get(key)) is type(value) and span.get(key) == value
            for key, value in expected.items()
        ),
        "image_secret_span_binding",
    )
    status = span.get("status")
    require(status in {"resolved", "ambiguous", "unresolved"}, "image_secret_span_status")
    fields = set(expected) | {"status"}
    if status == "resolved":
        fields |= {"spanBytes", "spanSha256"}
    require(set(span) == fields, "image_secret_span_fields")
    return {**expected, "line": finding["StartLine"], "status": status}


def secret_verdict(
    value: JsonValue, exceptions: Sequence[ExceptionRecord], paths: JsonObject, spans: JsonObject
) -> JsonObject:
    """Review exact public match bytes; keep changed surrounding bytes as fresh evidence."""
    findings, regions = array_value(value), array_value(spans["findings"])
    require(
        set(spans) == {"format", "findings"}
        and spans["format"] == security_secret_spans.FORMAT
        and len(findings) == len(regions)
        and len(findings) <= security_secret_spans.MAX_FINDINGS,
        "image_secret_span_report",
    )
    blocked: list[JsonValue] = []
    waived: list[JsonValue] = []
    for item, raw_span in zip(findings, regions, strict=True):
        span = object_value(raw_span)
        record = secret_span_binding(object_value(item), paths, span)
        approved = False
        if record["status"] == "resolved":
            fingerprint = secret_fingerprint(span)
            record.update(
                fingerprint=fingerprint, spanBytes=span["spanBytes"], spanSha256=span["spanSha256"]
            )
            approved = permitted(exceptions, "gitleaks", fingerprint, string_value(record["path"]))
        (waived if approved else blocked).append(record)
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
        records = array_value(package["licenses"])
        expressions = [
            string_value(object_value(item).get("spdxExpression", "")) for item in records
        ]
        expression = " AND ".join("(" + item + ")" for item in expressions if item)
        complete = bool(expressions) and all(expressions)
        approved = complete and LicenseExpression(expression, allowed).allowed_expression()
        if package.get("name") == "simplestChat" and package.get("type") == "rust-crate":
            approved = expressions == ["NOASSERTION"]
        if approved:
            continue
        scope = string_value(
            package.get("purl")
            or (string_value(package["name"]) + "@" + string_value(package["version"]))
        )
        # Raw identity preserves every declaration and evidence path. Only a
        # verified image-layer digest is separated from semantic review identity.
        raw_identity = raw_license_identity(records) if not complete else None
        fingerprint = (
            "license:" + hashlib.sha256(expression.encode()).hexdigest()
            if raw_identity is None
            else raw_identity.fingerprint
        )
        finding: JsonObject = {
            "scope": scope,
            "expression": expression or "UNKNOWN",
            "fingerprint": fingerprint,
        }
        if raw_identity is not None:
            finding["rawLicenseRecords"] = records
            finding["rawLicenseRecordsSha256"] = raw_identity.records_sha256
        (waived if permitted(exceptions, "image-license", fingerprint, scope) else blocked).append(
            finding
        )
    return {"passed": not blocked, "packages": len(packages), "blocked": blocked, "waived": waived}


def rust_license_join(packages: list[JsonObject], native: JsonObject, elf: JsonObject) -> None:
    """Join binary-observed Rust identities to authenticated build-source license records."""
    metadata = object_value(elf["auditable"])
    declared = object_value(native["rust_dependency_metadata"])
    require(
        set(declared) == {"format", "sha256", "compressedSha256", "packageCount"}
        and all(
            type(declared[key]) is type(metadata[key]) and declared[key] == metadata[key]
            for key in declared
        ),
        "image_rust_metadata_binding",
    )
    graph = [object_value(item) for item in array_value(metadata["packages"])]
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
        coverage = object_value(license_record["evidence"])
        require(
            set(coverage) == {"compilerArtifact", "embeddedMetadata"}
            and type(coverage["compilerArtifact"]) is bool
            and coverage["embeddedMetadata"] is True,
            "image_rust_license_coverage",
        )
        expression = object_value(license_record["license"])["expression"]
        declared_expression = expression
        if expression is not None:
            expression = string_value(expression)
            if source == "crates.io":
                expression = CARGO_LICENSE_ALTERNATIVES.get(expression, expression)
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
            declared_expression = expression
        package["licenses"] = (
            []
            if expression is None
            else [
                {
                    "value": declared_expression,
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


def openssl_build_binding(native: JsonObject, expected: JsonObject) -> None:
    """Bind both configured archives to every distinct Cargo provider build instance."""
    component = object_value(native["openssl"])
    require(
        set(component) == set(expected) | {"build"}
        and all(component[key] == expected[key] for key in expected)
        and component["configure_options"]
        == ["no-shared", "no-dso", "no-module", "no-engine", "no-tests"],
        "image_openssl_source_binding",
    )
    build = object_value(component["build"])
    require(
        set(build)
        == {
            "version_header",
            "configuration_header",
            "disabled_options_record",
            "disabled_options",
            "static_libraries",
        }
        and build["disabled_options"] == ["shared", "dso", "module", "engine"],
        "image_openssl_configuration",
    )
    records = [
        object_value(build[key])
        for key in ("version_header", "configuration_header", "disabled_options_record")
    ]
    libraries = [object_value(value) for value in array_value(build["static_libraries"])]
    require(
        len(libraries) == len(OPENSSL_LIBRARIES)
        and [item.get("library") for item in libraries] == list(OPENSSL_LIBRARIES),
        "image_openssl_archives",
    )
    for item in [*records, *libraries]:
        path = string_value(item["path"])
        require(
            set(item)
            == (
                {"path", "sha256", "size", "library"}
                if item in libraries
                else {"path", "sha256", "size"}
            )
            and PurePosixPath(path).is_absolute()
            and str(PurePosixPath(path)) == path
            and ".." not in PurePosixPath(path).parts
            and re.fullmatch(r"[a-f0-9]{64}", string_value(item["sha256"]))
            and type(item["size"]) is int
            and 0 < item["size"] <= 512 * 1024 * 1024,
            "image_openssl_artifact_identity",
        )
    suffix = "/include/openssl/opensslv.h"
    prefix = string_value(records[0]["path"]).removesuffix(suffix)
    require(
        records[0]["path"] == prefix + suffix
        and records[1]["path"] == prefix + "/include/openssl/configuration.h"
        and records[2]["path"] == prefix + "/share/simplestchat/openssl-disabled.txt"
        and records[2]["sha256"] == hashlib.sha256(b"shared\ndso\nmodule\nengine\n").hexdigest()
        and records[2]["size"] == len(b"shared\ndso\nmodule\nengine\n"),
        "image_openssl_configuration_identity",
    )
    linked = [object_value(item) for item in array_value(native["static_archives"])]
    previous_instances: set[tuple[str, str]] | None = None
    for library in libraries:
        name = string_value(library["library"])
        matches = [item for item in linked if item["library"] == name]
        instances = {
            (string_value(item["provider"]), string_value(item["out_dir"])) for item in matches
        }
        require(
            library["path"] == f"{prefix}/lib/lib{name}.a"
            and 0 < len(matches) <= MAX_NATIVE_INSTANCES
            and len(instances) == len(matches)
            and frozenset(provider for provider, _ in instances) == OPENSSL_PROVIDERS
            and (previous_instances is None or instances == previous_instances)
            and all(
                PurePosixPath(out_dir).is_absolute()
                and str(PurePosixPath(out_dir)) == out_dir
                and ".." not in PurePosixPath(out_dir).parts
                for _, out_dir in instances
            )
            and all(
                type(item["size"]) is int
                and all(item[key] == library[key] for key in ("path", "sha256", "size"))
                for item in matches
            ),
            "image_openssl_archive_binding",
        )
        previous_instances = instances


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


def runtime_file_path(rootfs: Path, name: str) -> Path:
    """Resolve one canonical RPM/ELF candidate inside the authenticated image only."""
    require(
        PurePosixPath(name).is_absolute()
        and str(PurePosixPath(name)) == name
        and ".." not in PurePosixPath(name).parts,
        "image_runtime_rpm_path",
    )
    try:
        return security_elf.image_path(rootfs, name)
    except (OSError, ValueError) as error:
        reason = "image_runtime_rpm_path"
        raise security_tools.ToolError(reason) from error


def runtime_rpm_bindings(
    packages: list[JsonObject], elf: JsonObject, rootfs: Path
) -> list[JsonValue]:
    """Bind resolved runtime bytes to exactly one regular RPM file through image symlinks."""
    require(rootfs.is_dir() and not rootfs.is_symlink(), "image_runtime_root")
    result: list[JsonValue] = []
    for value in array_value(elf["libraries"]):
        library = object_value(value)
        name = string_value(library["path"])
        resolved = runtime_file_path(rootfs, name)
        content = security_elf.read_binary(resolved)
        require(
            "/" + resolved.relative_to(rootfs).as_posix() == name
            and type(library["bytes"]) is int
            and len(content) == library["bytes"]
            and hashlib.sha256(content).hexdigest() == library["sha256"],
            "image_runtime_library_identity",
        )
        owners: list[JsonObject] = []
        for package in packages:
            if package["type"] != "rpm":
                continue
            metadata = object_value(package["metadata"])
            for item in array_value(metadata["files"]):
                file = object_value(item)
                recorded_name = string_value(file["path"])
                # Only potential regular-file owners need resolving. Unrelated
                # RPM config/ghost entries may legitimately be absent in the image.
                if PurePosixPath(recorded_name).name != resolved.name:
                    continue
                if runtime_file_path(rootfs, recorded_name) != resolved:
                    continue
                recorded = object_value(file["digest"])
                require(
                    type(file["mode"]) is int
                    and stat.S_ISREG(file["mode"])
                    and type(file["size"]) is int
                    and file["size"] == library["bytes"]
                    and recorded["algorithm"] == "sha256"
                    and recorded["value"] == library["sha256"],
                    "image_runtime_rpm_digest",
                )
                owners.append(
                    {
                        "path": recorded_name,
                        "resolvedPath": name,
                        "bytes": library["bytes"],
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
