"""Current release attestation shape and exact byte bindings.

Cryptographic verification runs on the authenticated controller before SSH.
The receiver accepts its result only through that trusted SSH stdin channel,
then independently matches every downloaded byte. An artifact-provided receipt
is never a substitute for controller verification.
"""

from __future__ import annotations

import re
from datetime import UTC, date, datetime
from typing import TYPE_CHECKING

from release_artifact import sha256_file
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from pathlib import Path

PREDICATE_TYPE = "https://github.com/swiftraccoon/simplestChat/attestations/release/v1"
WORKFLOW = ".github/workflows/ci.yml"
REF = "refs/heads/main"
ISSUER = "https://token.actions.githubusercontent.com"
BOUND_FILES = (
    "image.tar",
    "release.json",
    "outcome.json",
    "source.json",
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
)
SUBJECTS = ("image.tar", "sbom.spdx.json", "runtime-proof.json", "vex.openvex.json")
BUNDLE = "release-attestation.jsonl"
PREDICATE = "release-predicate.json"
MAX_METADATA = 64 * 1024**2
MAX_IMAGE = 2 * 1024**3


class TrustError(ValueError):
    """A fixed release trust failure, without downloaded content or credentials."""


def require(condition: object, reason: str) -> None:
    """Reject incomplete or unbound release evidence."""
    if not condition:
        raise TrustError(reason)


def digest(value: JsonValue) -> bool:
    """Recognize only complete lowercase SHA-256 values."""
    return isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value) is not None


def read(path: Path) -> JsonObject:
    """Bound and decode one regular metadata file with duplicate-key rejection."""
    require(not path.is_symlink() and path.is_file(), "attestation_metadata_file")
    require(0 < path.stat().st_size <= MAX_METADATA, "attestation_metadata_size")
    return object_value(decode_json(path.read_bytes()))


def predicate(value: JsonValue, envelope: JsonObject) -> JsonObject:
    """Require one exact current main-push release claim, never an older shape."""
    result = object_value(value)
    require(
        set(result)
        == {
            "schemaVersion",
            "repository",
            "revision",
            "runId",
            "runAttempt",
            "workflow",
            "ref",
            "event",
            "platform",
            "imageId",
            "securityPassed",
            "fileDigests",
        },
        "attestation_predicate_shape",
    )
    require(
        type(result["schemaVersion"]) is int and result["schemaVersion"] == 1, "attestation_schema"
    )
    require(
        result["repository"] == envelope["repository"]
        and result["revision"] == envelope["revision"]
        and type(result["runId"]) is int
        and result["runId"] == envelope["ciRunId"]
        and type(result["runAttempt"]) is int
        and result["runAttempt"] == envelope["runAttempt"]
        and envelope["buildRunId"] == envelope["ciRunId"],
        "attestation_source_identity",
    )
    require(
        result["workflow"] == WORKFLOW
        and result["ref"] == REF
        and result["event"] == "push"
        and result["platform"] == "linux/amd64"
        and result["securityPassed"] is True,
        "attestation_release_policy",
    )
    image = result["imageId"]
    require(
        isinstance(image, str) and re.fullmatch(r"sha256:[a-f0-9]{64}", image),
        "attestation_image_identity",
    )
    files = object_value(result["fileDigests"])
    require(
        set(files) == set(BOUND_FILES) and all(digest(item) for item in files.values()),
        "attestation_file_digests",
    )
    return result


def proof(value: JsonValue, envelope: JsonObject) -> JsonObject:
    """Validate the controller's SSH-only verification projection before remote work."""
    result = object_value(value)
    require(
        set(result) == {"predicate", "bundleSha256", "verificationSha256"},
        "attestation_proof_shape",
    )
    _ = predicate(result["predicate"], envelope)
    hashes = object_value(result["verificationSha256"])
    require(
        digest(result["bundleSha256"])
        and set(hashes) == set(SUBJECTS)
        and all(digest(item) for item in hashes.values()),
        "attestation_proof_digests",
    )
    return result


def bind_files(directory: Path, claim: JsonObject) -> None:
    """Check signed hashes and current image-security relationships without executing bytes."""
    files = object_value(claim["fileDigests"])
    for name in BOUND_FILES:
        path = directory / name
        require(not path.is_symlink() and path.is_file(), "attestation_bound_file")
        require(
            0 < path.stat().st_size <= (MAX_IMAGE if name == "image.tar" else MAX_METADATA),
            "attestation_bound_file_size",
        )
        require(sha256_file(path) == files[name], "attestation_file_changed")
    security = read(directory / "image-security.json")
    require(
        security.get("passed") is True
        and security.get("revision") == claim["revision"]
        and security.get("platform") == claim["platform"]
        and security.get("imageId") == claim["imageId"]
        and security.get("archiveSha256") == files["image.tar"]
        and security.get("sbomSha256") == files["sbom.spdx.json"],
        "attestation_security_binding",
    )
    bind_runtime(directory, security, claim)
    outcome = read(directory / "outcome.json")
    require(
        outcome.get("passed") is True
        and outcome.get("revision") == claim["revision"]
        and outcome.get("exportedImageId") == claim["imageId"],
        "attestation_build_binding",
    )
    manifest = read(directory / "release.json")
    require(
        manifest.get("archiveSha256") == files["image.tar"]
        and manifest.get("revision") == claim["revision"]
        and manifest.get("platform") == claim["platform"],
        "attestation_manifest_binding",
    )


def bind_runtime(directory: Path, security: JsonObject, claim: JsonObject) -> None:
    """Bind signed execution evidence and refuse an expired conditional disposition.

    The trusted image gate evaluates reachability. This boundary checks the exact
    authenticated output relationships afresh before signing, verification and
    receipt publication; it does not reinterpret the package's affected status.
    """
    files = object_value(claim["fileDigests"])
    required = security.get("vexRequired")
    require(
        type(required) is bool
        and security.get("runtimeProofSha256") == files["runtime-proof.json"]
        and security.get("vexSha256") == files["vex.openvex.json"],
        "attestation_runtime_hash_binding",
    )
    proof = read(directory / "runtime-proof.json")
    require(
        type(proof.get("schemaVersion")) is int
        and proof.get("schemaVersion") == 1
        and proof.get("required") is required
        and proof.get("passed") is True
        and proof.get("archiveSha256") == files["image.tar"]
        and all(proof.get(key) == claim[key] for key in ("imageId", "revision", "platform")),
        "attestation_runtime_identity",
    )
    statements = array_value(read(directory / "vex.openvex.json").get("statements"))
    require(len(statements) == (1 if required else 0), "attestation_runtime_disposition")
    if required:
        review = object_value(proof.get("sourceReview"))
        expires = string_value(review.get("expires"))
        require(
            re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", expires)
            and datetime.now(UTC).date() < date.fromisoformat(expires),
            "attestation_runtime_review_expired",
        )
        require(
            proof.get("scope") == "managed-default-server-dtls"
            and all(
                digest(proof.get(key)) and proof.get(key) == security.get(key)
                for key in ("binarySha256", "nativeSha256", "elfSha256", "sbomSha256")
            ),
            "attestation_runtime_evidence_binding",
        )


def bind_received(directory: Path, envelope: JsonObject) -> None:
    """Match downloaded bytes to the already verified controller claim and bundle."""
    verified = proof(envelope["attestation"], envelope)
    claim = object_value(verified["predicate"])
    require(read(directory / PREDICATE) == claim, "attestation_predicate_changed")
    bundle = directory / BUNDLE
    require(
        not bundle.is_symlink()
        and bundle.is_file()
        and sha256_file(bundle) == verified["bundleSha256"],
        "attestation_bundle_changed",
    )
    bind_files(directory, claim)
