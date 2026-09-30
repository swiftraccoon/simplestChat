"""Prepare and verify exact main-push release attestations before remote actions.

GitHub's maintained verifier handles signatures, certificate identity and
transparency evidence. This module adds exact workflow/run/attempt and file
relationships. No decoded, unverified bundle or artifact-provided success bit
can authorize SSH, Ansible, image loading or service operations.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import re
import shutil
import ssl
import sys
import time
import zipfile
from http import HTTPStatus
from pathlib import Path
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import bounded_process
import release_fetch_receiver as receiver
import release_trust as trust
import security_archive
from release_artifact import sha256_file, validate_manifest, verify_archive
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

MAX_VERIFICATION = 8 * 1024**2
VERIFY_SECONDS = 180
DOWNLOAD_SECONDS = 240


def write(path: Path, value: JsonValue) -> None:
    """Create private evidence without replacing a previous result."""
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        _ = output.write("\n")


def selectors(repository: str, revision: str, run_id: int, attempt: int) -> JsonObject:
    """Validate explicit identities before preparing a claim or verifier command."""
    trust.require(
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", repository),
        "attestation_repository",
    )
    trust.require(re.fullmatch(r"[a-f0-9]{40}", revision), "attestation_revision")
    trust.require(
        type(run_id) is int and 0 < run_id < 2**53 and type(attempt) is int and 0 < attempt < 2**31,
        "attestation_run_identity",
    )
    return {
        "repository": repository,
        "revision": revision,
        "ciRunId": run_id,
        "buildRunId": run_id,
        "runAttempt": attempt,
    }


def prepare(artifact: Path, security: Path, selection: JsonObject) -> JsonObject:
    """Bind the one tested archive and successful image evidence for the trusted signer."""
    manifest = validate_manifest(artifact / "release.json")
    _ = verify_archive(artifact / "image.tar", manifest)
    result = trust.read(security / "outcome.json")
    trust.require(
        result.get("passed") is True
        and result.get("secretDetectorSelfTest") is True
        and result.get("revision") == selection["revision"]
        and result.get("archiveSha256") == manifest["archiveSha256"],
        "attestation_security_failed",
    )
    archive_id = security_archive.image_identity(artifact / "image.tar")
    trust.require(
        archive_id == result.get("imageId")
        and object_value(result.get("selectedImage")).get("archiveConfigSha256")
        == archive_id.removeprefix("sha256:"),
        "attestation_config_identity",
    )
    checks = object_value(result.get("checks"))
    trust.require(
        set(checks) == {"vulnerabilities", "licenses", "secrets"}
        and all(object_value(item).get("passed") is True for item in checks.values()),
        "attestation_security_checks",
    )
    for field, name in (
        ("sbomSha256", "spdx/sbom.spdx.json"),
        ("nativeSha256", "native.json"),
        ("elfSha256", "elf.json"),
        ("secretPathMapSha256", "secret-paths.json"),
        ("databaseEvidenceSha256", "database-status.json"),
        ("runtimeProofSha256", "runtime-proof.json"),
        ("vexSha256", "vex.openvex.json"),
        ("runtimeLicenseEvidenceSha256", "runtime-license-evidence.json"),
    ):
        trust.require(
            result.get(field) == sha256_file(security / name), "attestation_security_file"
        )
    for source, name in (
        (security / "spdx/sbom.spdx.json", "sbom.spdx.json"),
        (security / "outcome.json", "image-security.json"),
        (security / "runtime-proof.json", "runtime-proof.json"),
        (security / "vex.openvex.json", "vex.openvex.json"),
    ):
        trust.require(
            not source.is_symlink() and 0 < source.stat().st_size <= trust.MAX_METADATA,
            "attestation_metadata_file",
        )
        with source.open("rb") as incoming, (artifact / name).open("xb") as outgoing:
            shutil.copyfileobj(incoming, outgoing, length=65536)
    claim: JsonObject = {
        "schemaVersion": 1,
        "repository": selection["repository"],
        "revision": selection["revision"],
        "runId": selection["ciRunId"],
        "runAttempt": selection["runAttempt"],
        "workflow": trust.WORKFLOW,
        "ref": trust.REF,
        "event": "push",
        "platform": "linux/amd64",
        "imageId": result.get("imageId"),
        "securityPassed": True,
        "fileDigests": {name: sha256_file(artifact / name) for name in trust.BOUND_FILES},
    }
    _ = trust.predicate(claim, selection)
    trust.bind_files(artifact, claim)
    write(artifact / trust.PREDICATE, claim)
    return claim


def verifier_arguments(path: Path, bundle: Path, selection: JsonObject) -> list[str]:
    """Specify all cryptographic identity restrictions explicitly; unsupported flags fail."""
    repository, revision = (
        string_value(selection["repository"]),
        string_value(selection["revision"]),
    )
    return [
        "gh",
        "attestation",
        "verify",
        str(path),
        "--bundle",
        str(bundle),
        "--repo",
        repository,
        "--hostname",
        "github.com",
        "--predicate-type",
        trust.PREDICATE_TYPE,
        "--cert-identity",
        f"https://github.com/{repository}/{trust.WORKFLOW}@{trust.REF}",
        "--cert-oidc-issuer",
        trust.ISSUER,
        "--signer-digest",
        revision,
        "--source-digest",
        revision,
        "--source-ref",
        trust.REF,
        "--deny-self-hosted-runners",
        "--format",
        "json",
    ]


def verified_statement(value: JsonValue, selection: JsonObject, claim: JsonObject) -> None:
    """Interpret only successful verifier output, binding its certificate run and all subjects."""
    entries = array_value(value)
    trust.require(len(entries) == 1, "attestation_verification_count")
    result = object_value(object_value(entries[0])["verificationResult"])
    certificate = object_value(object_value(result["signature"])["certificate"])
    repository, revision = selection["repository"], selection["revision"]
    expected = {
        "subjectAlternativeName": f"https://github.com/{repository}/{trust.WORKFLOW}@{trust.REF}",
        "issuer": trust.ISSUER,
        "buildSignerDigest": revision,
        "sourceRepositoryURI": f"https://github.com/{repository}",
        "sourceRepositoryDigest": revision,
        "sourceRepositoryRef": trust.REF,
        "runInvocationURI": f"https://github.com/{repository}/actions/runs/{selection['ciRunId']}/attempts/{selection['runAttempt']}",
        "runnerEnvironment": "github-hosted",
        "buildTrigger": "push",
    }
    trust.require(
        all(certificate.get(key) == item for key, item in expected.items()),
        "attestation_certificate_identity",
    )
    trust.require(bool(array_value(result["verifiedTimestamps"])), "attestation_verified_timestamp")
    statement = object_value(result["statement"])
    trust.require(
        statement.get("_type") == "https://in-toto.io/Statement/v1"
        and statement.get("predicateType") == trust.PREDICATE_TYPE
        and statement.get("predicate") == claim,
        "attestation_signed_claim",
    )
    subjects = array_value(statement["subject"])
    files = object_value(claim["fileDigests"])
    expected_subjects: list[JsonValue] = [
        {"name": name, "digest": {"sha256": files[name]}} for name in trust.SUBJECTS
    ]
    trust.require(
        len(subjects) == len(expected_subjects)
        and all(item in subjects for item in expected_subjects),
        "attestation_subjects",
    )


def verify_directory(directory: Path, envelope: JsonObject) -> JsonObject:
    """Cryptographically verify cached exact bytes afresh; never trust a stored success receipt."""
    verify_cached_zip(directory, envelope)
    claim = trust.predicate(trust.read(directory / trust.PREDICATE), envelope)
    trust.bind_files(directory, claim)
    bundle = directory / trust.BUNDLE
    trust.require(
        not bundle.is_symlink()
        and bundle.is_file()
        and 0 < bundle.stat().st_size <= MAX_VERIFICATION,
        "attestation_bundle_file",
    )
    environment = {
        key: os.environ[key]
        for key in (
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "XDG_CONFIG_HOME",
        )
        if key in os.environ
    }
    environment["LC_ALL"] = "C"
    bundle_digest = sha256_file(bundle)
    verification: JsonObject = {}
    for name in trust.SUBJECTS:
        status, output, _ = bounded_process.run(
            verifier_arguments(directory / name, bundle, envelope),
            env=environment,
            limits=bounded_process.Limits(
                timeout=VERIFY_SECONDS, stdout=MAX_VERIFICATION, stderr=65536
            ),
        )
        trust.require(status == 0, "attestation_signature_failed")
        verified_statement(decode_json(output), envelope, claim)
        verification[name] = hashlib.sha256(output).hexdigest()
    trust.require(sha256_file(bundle) == bundle_digest, "attestation_bundle_changed")
    proof: JsonObject = {
        "predicate": claim,
        "bundleSha256": bundle_digest,
        "verificationSha256": verification,
    }
    _ = trust.proof(proof, envelope)
    result: JsonObject = {**envelope, "attestation": proof}
    _ = receiver.validate_envelope(result)
    _ = receiver.validate_build_evidence(directory, result)
    return result


def verify_cached_zip(directory: Path, envelope: JsonObject) -> None:
    """Bind a cached candidate to its exact API-digested ZIP and extracted members."""
    target = directory / "artifact.zip"
    trust.require(
        not target.is_symlink()
        and target.is_file()
        and target.stat().st_size == envelope["artifactZipBytes"]
        and target.stat().st_size <= receiver.MAX_ZIP_BYTES
        and sha256_file(target) == envelope["zipSha256"],
        "attestation_cached_zip_identity",
    )
    receiver.validate_zip_directory(target)
    with zipfile.ZipFile(target) as archive:
        for name, member in receiver.selected_members(archive).items():
            path = directory / name
            trust.require(
                not path.is_symlink()
                and path.is_file()
                and path.stat().st_size == member.file_size,
                "attestation_cached_member",
            )
            checksum, total = hashlib.sha256(), 0
            with archive.open(member) as source:
                while chunk := source.read(min(65536, member.file_size - total + 1)):
                    total += len(chunk)
                    trust.require(total <= member.file_size, "attestation_cached_member")
                    checksum.update(chunk)
            trust.require(
                total == member.file_size and checksum.hexdigest() == sha256_file(path),
                "attestation_cached_member",
            )


def download(url: str, target: Path, envelope: JsonObject) -> None:
    """Stream the API-digested ZIP locally with no credentials, redirects or proxy inheritance."""
    _ = receiver.validate_url({"url": url})
    parsed = urlsplit(url)
    connection = http.client.HTTPSConnection(
        parsed.netloc, timeout=15, context=ssl.create_default_context()
    )
    deadline, total, checksum = time.monotonic() + DOWNLOAD_SECONDS, 0, hashlib.sha256()
    expected = integer_value(envelope["artifactZipBytes"])
    trust.require(
        type(expected) is int and 0 < expected <= receiver.MAX_ZIP_BYTES, "attestation_zip_size"
    )
    try:
        connection.request(
            "GET",
            parsed.path + "?" + parsed.query,
            headers={"Accept": "application/zip", "Accept-Encoding": "identity"},
        )
        response = connection.getresponse()
        trust.require(
            response.status == HTTPStatus.OK
            and response.getheader("Content-Encoding", "identity") == "identity"
            and response.getheader("Content-Length") in (None, str(expected)),
            "attestation_http_response",
        )
        with target.open("xb") as output:
            while chunk := response.read(min(65536, expected - total + 1)):
                total += len(chunk)
                trust.require(
                    total <= expected and time.monotonic() <= deadline, "attestation_download_bound"
                )
                checksum.update(chunk)
                _ = output.write(chunk)
        trust.require(
            total == expected and checksum.hexdigest() == envelope["zipSha256"],
            "attestation_zip_identity",
        )
    finally:
        connection.close()


def fetch_verify(url: str, directory: Path, envelope: JsonObject) -> JsonObject:
    """Download once and retain failed evidence locally before any remote operation."""
    directory.mkdir(mode=0o700)
    trust.require(
        shutil.disk_usage(directory).free
        >= integer_value(envelope["artifactZipBytes"])
        + receiver.MAX_EXTRACTED_BYTES
        + 256 * 1024**2,
        "attestation_insufficient_disk",
    )
    target = directory / "artifact.zip"
    download(url, target, envelope)
    receiver.validate_zip_directory(target)
    with zipfile.ZipFile(target) as archive:
        for name, member in receiver.selected_members(archive).items():
            receiver.extract_member(archive, member, directory / name)
    return verify_directory(directory, envelope)


class Options(argparse.Namespace):
    """One explicit signer preparation or fresh controller verification operation."""

    operation: str = ""
    artifact_dir: Path = Path()
    security_dir: Path = Path()
    repository: str = ""
    revision: str = ""
    run_id: int = 0
    run_attempt: int = 0


def main(argv: list[str] | None = None) -> int:
    """Keep producer and local verification commands free of SSH and engine actions."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    producer = sub.add_parser("prepare")
    for name in ("artifact-dir", "security-dir"):
        _ = producer.add_argument("--" + name, type=Path, required=True)
    for name in ("repository", "revision"):
        _ = producer.add_argument("--" + name, required=True)
    for name in ("run-id", "run-attempt"):
        _ = producer.add_argument("--" + name, type=int, required=True)
    args = parser.parse_args(argv, namespace=Options())
    _ = os.umask(0o077)
    try:
        _ = prepare(
            args.artifact_dir,
            args.security_dir,
            selectors(args.repository, args.revision, args.run_id, args.run_attempt),
        )
    except (ValueError, OSError, KeyError, receiver.FetchError, bounded_process.ProcessError):
        print(json.dumps({"passed": False, "failureClass": "release_attestation_failed"}))  # noqa: T201 -- Fixed CLI failure.
        return 1
    else:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
