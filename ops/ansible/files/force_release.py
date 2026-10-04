"""Admit an explicitly forced local artifact without claiming GitHub verification.

The authenticated controller binds the exact bytes and source inputs. This does
not verify a signature or CI result. Image staging, backup, replacement, readiness
and rollback remain the ordinary release helper's responsibility.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import stat
import sys
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn

import release_public as release
from release_artifact import (
    MAX_ARCHIVE_BYTES,
    MAX_JSON_BYTES,
    Manifest,
    sha256_file,
    validate_manifest,
    verify_archive,
)
from release_json import JsonObject, decode_json, integer_value, object_value, string_value

if TYPE_CHECKING:
    from types import FrameType

FILES = ("image.tar", "release.json", "source.json", "outcome.json")
SOURCE_INPUTS = (
    "Dockerfile",
    ".dockerignore",
    "Cargo.lock",
    "web/package-lock.json",
    "build/pip-constraints.txt",
)
REQUEST_KEYS = {
    "schemaVersion",
    "operation",
    "forced",
    "githubAttested",
    "ciVerification",
    "repository",
    "revision",
    "fileDigests",
    "sourceInputsSha256",
    "deploy",
    "quietSeconds",
}


def read_object(path: Path) -> JsonObject:
    """Read bounded ordinary JSON without following a final symlink."""
    metadata = path.lstat()
    release.require(
        stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= MAX_JSON_BYTES,
        "invalid_force_metadata",
    )
    return object_value(decode_json(path.read_bytes()))


def validate_request(request: JsonObject) -> None:
    """Require explicit unsigned authorization and exact immutable source selectors."""
    release.require(
        set(request) == REQUEST_KEYS
        and type(request.get("schemaVersion")) is int
        and request["schemaVersion"] == 1
        and request.get("operation") == "force-release"
        and request.get("forced") is True
        and request.get("githubAttested") is False
        and request.get("ciVerification") == "skipped-explicit-force"
        and type(request.get("deploy")) is bool
        and type(request.get("quietSeconds")) is int
        and 0 <= integer_value(request["quietSeconds"]) <= release.MAX_QUIET_SECONDS,
        "invalid_force_authorization",
    )
    release.require(
        re.fullmatch(r"[a-f0-9]{40}", string_value(request["revision"])), "invalid_force_revision"
    )
    release.require(
        re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}",
            string_value(request["repository"]),
        ),
        "invalid_force_repository",
    )
    for key, names in (("fileDigests", FILES), ("sourceInputsSha256", SOURCE_INPUTS)):
        values = object_value(request[key])
        release.require(
            set(values) == set(names)
            and all(
                isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value)
                for value in values.values()
            ),
            "invalid_force_digests",
        )


def file_hashes(directory: Path) -> JsonObject:
    """Hash only bounded ordinary artifact files, never links or special devices."""
    result: JsonObject = {}
    for name in FILES:
        path = directory / name
        metadata = path.lstat()
        maximum = MAX_ARCHIVE_BYTES if name == "image.tar" else MAX_JSON_BYTES
        release.require(
            stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= maximum,
            "invalid_force_artifact_file",
        )
        result[name] = sha256_file(path)
    return result


def validate_artifact(directory: Path, request: JsonObject) -> Manifest:
    """Check the unchanged build/export contract without inventing a signed receipt."""
    validate_request(request)
    release.require(
        file_hashes(directory) == request["fileDigests"], "force_artifact_digest_mismatch"
    )
    source = read_object(directory / "source.json")
    release.require(
        source
        == {
            "revision": request["revision"],
            "inputsSha256": request["sourceInputsSha256"],
        },
        "force_source_evidence_mismatch",
    )
    outcome = read_object(directory / "outcome.json")
    release.require(
        type(outcome.get("schemaVersion")) is int
        and outcome["schemaVersion"] == 1
        and outcome.get("revision") == request["revision"]
        and outcome.get("passed") is True
        and "error" in outcome
        and outcome["error"] is None,
        "force_build_not_passed",
    )
    manifest = validate_manifest(directory / "release.json")
    release.require(manifest["revision"] == request["revision"], "force_revision_mismatch")
    _ = verify_archive(directory / "image.tar", manifest)
    return manifest


def publish(directory: Path, revision: str) -> Path:
    """Retain immutable canonical artifacts; refuse every conflicting existing byte."""
    parent = release.ROOT / "releases"
    release.protected(parent, directory=True, modes=(0o700,))
    destination = parent / revision
    destination.mkdir(mode=0o700, exist_ok=True)
    release.protected(destination, directory=True, modes=(0o700,))
    for name in FILES:
        target = destination / name
        if target.exists() or target.is_symlink():
            release.protected(target)
            release.require(
                target.stat().st_size == (directory / name).stat().st_size
                and sha256_file(target) == sha256_file(directory / name),
                "retained_artifact_differs",
            )
    # Authorization belongs to the operation, not the artifact identity. Retain
    # the first admission alongside the artifact; subsequent requests remain in
    # their own private directories (for example stage followed by deploy).
    if (destination / "force.json").exists() or (destination / "force.json").is_symlink():
        release.protected(destination / "force.json")
    for name in (*FILES, "force.json"):
        if not (destination / name).exists():
            os.link(directory / name, destination / name, follow_symlinks=False)
    descriptor = os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    return destination


def execute(directory: Path) -> JsonObject:
    """Use the ordinary locked release transaction, recording unsigned authorization."""
    release.require(
        directory.parent == release.ROOT / "forced"
        and re.fullmatch(r"[a-f0-9]{32}", directory.name),
        "invalid_force_directory",
    )
    for parent in (release.ROOT, directory.parent, directory):
        release.protected(parent, directory=True, modes=(0o700,))
        release.require(parent.resolve() == parent, "force_path_symlink")
    for name in (*FILES, "force.json"):
        release.protected(
            directory / name, limit=MAX_ARCHIVE_BYTES if name == "image.tar" else MAX_JSON_BYTES
        )
    request = read_object(directory / "force.json")
    validate_request(request)
    report: JsonObject = {
        "action": "force-deploy" if request["deploy"] else "force-stage",
        "revision": request["revision"],
        "repository": request["repository"],
        "forced": True,
        "githubAttested": False,
        "ciVerification": "skipped-explicit-force",
        "fileDigests": request["fileDigests"],
        "startedAt": release.timestamp(),
        "passed": False,
        "phase": "validate",
    }
    with release.workload_lock():
        release.protected(release.ROOT / "results", directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix="force.", dir=release.ROOT / "results"))
        report["evidence"] = str(attempt)
        try:
            manifest = validate_artifact(directory, request)
            canonical = publish(directory, manifest["revision"])
            runner = release.Runner(attempt)
            report["phase"] = "stage"
            staged = release.stage(runner, canonical, manifest)
            report["serverImage"] = staged["serverImage"]
            if request["deploy"]:
                report["phase"] = "deploy"
                release.deploy(
                    runner,
                    manifest,
                    staged,
                    report,
                    quiet_seconds=integer_value(request["quietSeconds"]),
                )
            report.update(passed=True, phase="complete")
        except BaseException:
            report["failure"] = "force_release_failed"
            raise
        finally:
            report["finishedAt"] = release.timestamp()
            release.atomic(attempt / "outcome.json", report)
    return report


class Arguments(argparse.Namespace):
    """Expose only the explicit authorization and prepared private request."""

    force: bool = False
    request: str = ""


def main() -> int:
    """Run the explicit force action without emitting private exception contents."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--force", action="store_true", required=True)
    _ = parser.add_argument("--request", required=True)
    args = parser.parse_args(namespace=Arguments())

    def interrupted(_number: int, _frame: FrameType | None) -> NoReturn:
        message = "force_release_interrupted"
        raise release.ReleaseError(message)

    try:
        release.require(os.geteuid() == 0 and args.force, "explicit_root_force_required")
        _ = os.umask(0o077)
        for number in (signal.SIGTERM, signal.SIGINT):
            _ = signal.signal(number, interrupted)
        path = Path(args.request)
        release.require(path.name == "force.json", "invalid_force_request_path")
        report = execute(path.parent)
    except Exception:  # noqa: BLE001 -- never expose archive, configuration or subprocess contents.
        _ = sys.stderr.write('{"passed":false,"failure":"force_release_failed"}\n')
        return 1
    _ = sys.stdout.write(json.dumps(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
