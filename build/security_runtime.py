"""Prove the managed server profile before issuing one exact-artifact OpenSSL VEX.

The installed RPM remains affected and visible. This is an execution-path
assessment for the shipped server, not a general exception for the RPM, other
programs in the image, or arbitrary operator-selected commands. No image code
is executed, no external VEX is trusted, and source reviews never renew themselves.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from datetime import UTC, date, datetime
from pathlib import Path

import bounded_process
import runtime_profile
import security_image_advisories as advisories
import security_image_policy
import security_tools
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

REVIEW = "security/runtime-source-review.json"
NSS = "security/runtime/nsswitch.conf"
PROFILE = "ops/ansible/files/runtime_profile.py"
SOURCE_TREES = (
    "src",
    "vendor/mediasoup-0.29.0/src",
    "vendor/mediasoup-sys-0.19.0/src",
    "vendor/mediasoup-sys-0.19.0/include",
    "vendor/mediasoup-sys-0.19.0/deps",
)
SOURCE_FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "vendor/integrity.json",
    "vendor/native-components.json",
)
MAX_REVIEW = 65536
MAX_SOURCE = 1024 * 1024
MAX_ENTRIES = 200000
MAX_DEPTH = 128
MAX_ENVIRONMENT = 256
MAX_ENVIRONMENT_BYTES = 65536
MAX_REVIEW_DAYS = 60
CVE = "CVE-2026-84782"
REPOSITORY = "https://github.com/swiftraccoon/simplestChat"


def require(condition: object, reason: str) -> None:
    """Reject incomplete evidence with a fixed public failure code."""
    security_tools.require(condition, reason)


def sha256(data: bytes) -> str:
    """Return an exact byte identity."""
    return hashlib.sha256(data).hexdigest()


def encoded(value: JsonValue) -> bytes:
    """Use one deterministic representation for generated evidence bindings."""
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def git_bytes(root: Path, arguments: list[str], limit: int) -> bytes:
    """Read only bounded local Git objects; no remote or image process is involved."""
    try:
        status, output, _ = bounded_process.run(
            ["git", *arguments],
            cwd=root,
            limits=bounded_process.Limits(timeout=15, stdout=limit, stderr=65536),
        )
    except bounded_process.ProcessError:
        reason = "runtime_source_git"
        raise security_tools.ToolError(reason) from None
    require(status == 0, "runtime_source_git")
    return output


def source_review(root: Path, revision: str) -> JsonObject:
    """Check an explicit, expiring review against the actual release's source objects."""
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "runtime_source_revision")
    body = security_tools.bounded_file(root / REVIEW, MAX_REVIEW)
    review = object_value(decode_json(body.decode()))
    require(
        set(review)
        == {
            "schemaVersion",
            "reviewedOn",
            "expires",
            "scope",
            "assessment",
            "trees",
            "files",
            "profileFiles",
        }
        and type(review["schemaVersion"]) is int
        and review["schemaVersion"] == 1
        and review["scope"] == "managed-default-server-dtls"
        and bool(string_value(review["assessment"])),
        "runtime_source_review_schema",
    )
    require(
        all(
            re.fullmatch(r"\d{4}-\d{2}-\d{2}", string_value(review[key]))
            for key in ("reviewedOn", "expires")
        ),
        "runtime_source_review_date",
    )
    start, end = (
        date.fromisoformat(string_value(review["reviewedOn"])),
        date.fromisoformat(string_value(review["expires"])),
    )
    require(
        start <= datetime.now(UTC).date() < end and (end - start).days <= MAX_REVIEW_DAYS,
        "runtime_source_review_expired",
    )
    trees, files = object_value(review["trees"]), object_value(review["files"])
    profiles = object_value(review["profileFiles"])
    require(
        set(trees) == set(SOURCE_TREES)
        and set(files) == set(SOURCE_FILES)
        and set(profiles) == {PROFILE, NSS},
        "runtime_source_review_scope",
    )
    expected_trees = [string_value(trees[path]) for path in SOURCE_TREES]
    require(
        all(re.fullmatch(r"[a-f0-9]{40}", item) for item in expected_trees),
        "runtime_source_tree_identity",
    )
    actual_trees = (
        git_bytes(root, ["rev-parse", *[f"{revision}:{path}" for path in SOURCE_TREES]], 4096)
        .decode()
        .splitlines()
    )
    require(actual_trees == expected_trees, "runtime_source_tree_changed")
    for path, digest in {**files, **profiles}.items():
        expected = string_value(digest)
        require(re.fullmatch(r"[a-f0-9]{64}", expected), "runtime_source_file_identity")
        actual = git_bytes(root, ["show", f"{revision}:{path}"], MAX_SOURCE)
        require(sha256(actual) == expected, "runtime_source_file_changed")
        if path in profiles:
            require(
                sha256(security_tools.bounded_file(root / path, MAX_SOURCE)) == expected,
                "runtime_profile_changed",
            )
    return {
        "sha256": sha256(body),
        "expires": review["expires"],
        "scope": review["scope"],
        "trees": trees,
        "files": files,
        "profileFiles": profiles,
    }


def filesystem_profile(root: Path, expected_nss: bytes) -> JsonObject:
    """Check hook absence without following host paths or enumerating an unbounded tree."""
    require(root.is_dir() and not root.is_symlink(), "runtime_root")
    for name in ("/etc/ld.so.cache", "/etc/ld.so.preload"):
        # image_path must also reject dangling symlinks: lstat the known parent.
        parent = root / "etc"
        require(parent.is_dir() and not parent.is_symlink(), "runtime_etc_directory")
        require(not os.path.lexists(parent / name.rsplit("/", 1)[1]), "runtime_loader_hook")
    pending = [(root, 0)]
    count = 0
    while pending:
        directory, depth = pending.pop()
        require(depth <= MAX_DEPTH, "runtime_directory_depth")
        with os.scandir(directory) as entries:
            for entry in entries:
                count += 1
                require(count <= MAX_ENTRIES, "runtime_entry_limit")
                require(entry.name != "glibc-hwcaps", "runtime_hwcaps")
                if entry.is_dir(follow_symlinks=False):
                    pending.append((Path(entry.path), depth + 1))
    path = root / "etc/nsswitch.conf"
    require(stat.S_ISREG(path.lstat().st_mode), "runtime_nss_kind")
    content = security_tools.bounded_file(path, 8192)
    require(content == expected_nss, "runtime_nss_profile")
    return {
        "cacheAbsent": True,
        "preloadAbsent": True,
        "hwcapsAbsent": True,
        "nssSha256": sha256(content),
        "entriesInspected": count,
    }


def report_digest(path: Path, expected: JsonObject) -> str:
    """Bind the exact published JSON bytes and their independently validated value."""
    body = security_tools.bounded_file(path, 8 * MAX_SOURCE)
    require(object_value(decode_json(body.decode())) == expected, "runtime_report_changed")
    return sha256(body)


def proof(  # noqa: PLR0913 -- Every independently authenticated artifact is explicit.
    *,
    tree: Path,
    elf: JsonObject,
    native: JsonObject,
    manifest: JsonObject,
    root: Path,
    sbom_sha256: str,
) -> JsonObject:
    """Bind the exact artifact, current native facts, loader profile and fixed source review."""
    archive = object_value(
        decode_json(security_tools.bounded_file(tree / "report.json", MAX_SOURCE).decode())
    )
    config_bytes = security_tools.bounded_file(tree / "layers/image-config.json", MAX_SOURCE)
    config = object_value(decode_json(config_bytes.decode()))
    require(
        archive.get("passed") is True
        and archive["configSha256"] == sha256(config_bytes)
        and archive["imageId"] == "sha256:" + sha256(config_bytes)
        and archive["archiveSha256"] == manifest["archiveSha256"]
        and archive["revision"] == manifest["revision"]
        and archive["platform"] == manifest["platform"],
        "runtime_archive_binding",
    )
    runtime = object_value(config["config"])
    environment = array_value(runtime["Env"])
    require(
        len(environment) <= MAX_ENVIRONMENT and len(encoded(environment)) <= MAX_ENVIRONMENT_BYTES,
        "runtime_environment_bounds",
    )
    runtime_profile.validate_image(runtime)
    require(
        config["os"] == "linux"
        and "linux/" + string_value(config["architecture"]) == manifest["platform"]
        and elf["platform"] == manifest["platform"]
        and object_value(runtime["Labels"])["org.opencontainers.image.revision"]
        == manifest["revision"],
        "runtime_platform_revision",
    )
    require(
        elf.get("passed") is True
        and elf.get("standardDirectoryCandidatesEqual") is True
        and elf.get("noLibraryLoaderHooks") is True
        and object_value(elf["checks"]).get("noLoaderHooks") is True,
        "runtime_elf_profile",
    )
    require(
        object_value(native["binary"])["sha256"] == elf["binarySha256"], "runtime_native_binary"
    )
    openssl = object_value(native["openssl"])
    require(
        openssl["version"] == "3.5.9"
        and object_value(openssl["build"])["disabled_options"]
        == ["shared", "dso", "module", "engine"],
        "runtime_static_openssl",
    )
    expected_native = object_value(
        decode_json(
            security_tools.bounded_file(root / "vendor/native-components.json", MAX_SOURCE).decode()
        )
    )
    security_image_policy.openssl_build_binding(native, object_value(expected_native["openssl"]))
    require(
        native["native_manifest_sha256"]
        == sha256(security_tools.bounded_file(root / "vendor/native-components.json", MAX_SOURCE))
        and native["cargo_lock_sha256"]
        == sha256(security_tools.bounded_file(root / "Cargo.lock", MAX_SOURCE)),
        "runtime_native_source",
    )
    review = source_review(root, string_value(manifest["revision"]))
    nss = security_tools.bounded_file(root / NSS, 8192)
    filesystem = filesystem_profile(tree / "rootfs", nss)
    return {
        "schemaVersion": 1,
        "required": True,
        "passed": True,
        "scope": "managed-default-server-dtls",
        "archiveSha256": manifest["archiveSha256"],
        "imageId": archive["imageId"],
        "revision": manifest["revision"],
        "platform": manifest["platform"],
        "binarySha256": elf["binarySha256"],
        "nativeSha256": report_digest(tree.parent / "native.json", native),
        "elfSha256": report_digest(tree.parent / "elf.json", elf),
        "sbomSha256": sbom_sha256,
        "imageConfigSha256": sha256(config_bytes),
        "environmentSha256": sha256(encoded(environment)),
        "filesystem": filesystem,
        "sourceReview": review,
        "advisoryPolicySha256": advisories.reviewed_policy(advisories.POLICY_PATH)[1],
    }


def affected_scopes(verdict: JsonObject) -> set[str]:
    """Select only the reviewed advisory's exact, observed affected RPM identities."""
    reviewed = object_value(verdict["reviewedAdvisories"])
    _, policy_digest = advisories.reviewed_policy(advisories.POLICY_PATH)
    require(reviewed["policySha256"] == policy_digest, "runtime_advisory_identity")
    scopes: set[str] = set()
    for raw in array_value(reviewed["blocked"]):
        finding = object_value(raw)
        require(
            finding["id"] == CVE and finding["source"] == "reviewed-openssl-advisory",
            "runtime_advisory_finding",
        )
        scopes.add(string_value(finding["scope"]))
    return scopes


def apply(
    verdict: JsonObject, evidence: JsonObject, proof_path: Path
) -> tuple[JsonObject, JsonObject]:
    """Keep affected records while separately disposing only this proven execution path."""
    require(
        evidence.get("passed") is True
        and evidence.get("required") is True
        and evidence.get("scope") == "managed-default-server-dtls",
        "runtime_proof_required",
    )
    scopes = affected_scopes(verdict)
    require(bool(scopes), "runtime_no_affected_rpm")
    reviewed = object_value(verdict["reviewedAdvisories"])
    require(evidence["advisoryPolicySha256"] == reviewed["policySha256"], "runtime_proof_advisory")
    binding: JsonObject = {
        "proofSha256": report_digest(proof_path, evidence),
        "advisoryPolicySha256": reviewed["policySha256"],
        "affectedScopes": list[JsonValue](sorted(scopes)),
    }
    subject = "urn:simplestchat:managed-server:sha256:" + sha256(encoded(binding))
    issued = datetime.now(UTC).isoformat()
    document: JsonObject = {
        "@context": "https://openvex.dev/ns/v0.2.0",
        "@id": "urn:simplestchat:vex:sha256:"
        + sha256(encoded({"binding": binding, "issued": issued})),
        "author": REPOSITORY,
        "role": "Project security automation",
        "timestamp": issued,
        "version": 1,
        "tooling": "build/security_runtime.py; explicit source review and exact artifact proof",
        "statements": [
            {
                "vulnerability": {"name": CVE},
                "products": [
                    {
                        "@id": subject,
                        "hashes": {"sha-256": evidence["archiveSha256"]},
                        "subcomponents": [
                            {"@id": scope, "identifiers": {"purl": scope}}
                            for scope in sorted(scopes)
                        ],
                    }
                ],
                "status": "not_affected",
                "justification": "vulnerable_code_not_in_execute_path",
                "impact_statement": (
                    "Applies only to this exact image's shipped server under the enforced managed "
                    "runtime profile. DTLS uses authenticated static OpenSSL 3.5.9 with external "
                    "DSO/module/engine loading disabled. The installed Fedora RPM remains "
                    "affected; other programs and operator-selected execution profiles are "
                    "outside this statement."
                ),
                "status_notes": "Proof SHA-256: "
                + string_value(binding["proofSha256"])
                + "; source review expires "
                + string_value(object_value(evidence["sourceReview"])["expires"]),
            }
        ],
    }
    original = array_value(verdict["blocked"])
    remaining: list[JsonValue] = []
    disposed: list[JsonValue] = []
    for raw in original:
        finding = object_value(raw)
        target = finding.get("id") == CVE and finding.get("scope") in scopes
        (disposed if target else remaining).append(finding)
    require(bool(disposed), "runtime_finding_missing")
    result = dict(verdict)
    result.update(
        {
            "passed": not remaining,
            "blocked": remaining,
            "affectedFindings": original,
            "notAffectedForManagedServer": disposed,
            "runtimeDisposition": {**binding, "subject": subject},
        }
    )
    return result, document


def not_required(
    manifest: JsonObject, image_id: str, verdict: JsonObject
) -> tuple[JsonObject, JsonObject]:
    """Publish an explicit absence of a VEX claim when the reviewed RPM is not affected."""
    require(not affected_scopes(verdict), "runtime_affected_rpm_requires_proof")
    evidence: JsonObject = {
        "schemaVersion": 1,
        "required": False,
        "passed": True,
        "reason": "no_affected_reviewed_rpm",
        "archiveSha256": manifest["archiveSha256"],
        "imageId": image_id,
        "revision": manifest["revision"],
        "platform": manifest["platform"],
        "advisoryPolicySha256": object_value(verdict["reviewedAdvisories"])["policySha256"],
    }
    issued = datetime.now(UTC).isoformat()
    document: JsonObject = {
        "@context": "https://openvex.dev/ns/v0.2.0",
        "@id": "urn:simplestchat:vex:sha256:"
        + sha256(encoded({"evidence": evidence, "issued": issued})),
        "author": REPOSITORY,
        "role": "Project security automation",
        "timestamp": issued,
        "version": 1,
        "tooling": "build/security_runtime.py; no affected reviewed RPM, no VEX claim",
        "statements": [],
    }
    return evidence, document
