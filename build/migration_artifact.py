"""Validate an explicitly unsigned existing release before migration staging.

The clean migration-tool checkout and the selected ancestor release are separate
identities. No image is built, no signed evidence is invented, and authorization
always selects staging without deployment. The controller verifies published
main before calling this module.
"""

import json
import os
import re
import tempfile
from pathlib import Path

import force_release
import release_build as build
import release_deploy as deploy
from release_artifact import sha256_file
from release_json import JsonObject, string_value

MAX_SOURCE_ARCHIVE = 16 * 1024 * 1024


def require(condition: object, code: str) -> None:
    """Reject mismatched selectors with a fixed operational diagnostic."""
    if not condition:
        raise build.BuildError(code)


def source_identity(
    root: Path, tool_revision: str, revision: str, repository: str, directory: Path
) -> tuple[JsonObject, dict[str, str]]:
    """Hash exact ancestor blobs through the existing bounded Git archive reader."""
    require(
        re.fullmatch(r"[a-f0-9]{40}", tool_revision) and re.fullmatch(r"[a-f0-9]{40}", revision),
        "invalid_migration_artifact_revision",
    )
    runner = build.Runner()
    require(
        deploy.checkout_identity(runner, root, repository) == tool_revision,
        "migration_tool_checkout_changed",
    )
    _, branch = runner.run(["git", "symbolic-ref", "--short", "HEAD"], cwd=root)
    require(branch == "main", "migration_main_checkout_required")
    status, _ = runner.run(
        ["git", "merge-base", "--is-ancestor", revision, tool_revision],
        cwd=root,
        allow_failure=True,
    )
    require(status == 0, "migration_release_not_tool_ancestor")
    with tempfile.TemporaryDirectory(prefix="source-inputs.", dir=directory) as temporary:
        parent = Path(temporary)
        archive = parent / "source.tar"
        _ = runner.run(
            [
                "git",
                "archive",
                "--format=tar",
                "--output=" + str(archive),
                revision,
                "--",
                *force_release.SOURCE_INPUTS,
                "migrations",
            ],
            cwd=root,
            timeout=60,
        )
        require(0 < archive.stat().st_size <= MAX_SOURCE_ARCHIVE, "migration_source_too_large")
        source = parent / "source"
        build.unpack_source(archive, source)
        inputs: JsonObject = {
            name: sha256_file(source / name) for name in force_release.SOURCE_INPUTS
        }
        migrations = build.migration_checksums(source)
    require(
        deploy.checkout_identity(runner, root, repository) == tool_revision,
        "migration_tool_checkout_changed",
    )
    return inputs, migrations


def artifact_path(directory: Path) -> Path:
    """Accept one canonical existing directory without following path aliases."""
    require(
        directory.is_absolute()
        and not directory.is_symlink()
        and directory.is_dir()
        and directory.resolve() == directory,
        "invalid_migration_artifact_directory",
    )
    return directory


def prepare(  # noqa: PLR0913, PLR0917 -- Keep both explicit revisions beside their artifact and repository.
    directory: Path,
    root: Path,
    tool_revision: str,
    revision: str,
    artifact_directory: Path,
    repository: str,
) -> tuple[Path, JsonObject]:
    """Validate the retained release and write a private, stage-only force request."""
    artifact = artifact_path(artifact_directory)
    inputs, migrations = source_identity(root, tool_revision, revision, repository, directory)
    request: JsonObject = {
        "schemaVersion": 1,
        "operation": "force-release",
        "forced": True,
        "githubAttested": False,
        "ciVerification": "skipped-explicit-force",
        "repository": repository,
        "revision": revision,
        "deploy": False,
        "quietSeconds": 0,
        "fileDigests": force_release.file_hashes(artifact),
        "sourceInputsSha256": inputs,
    }
    manifest = force_release.validate_artifact(artifact, request)
    require(manifest["migrations"] == migrations, "migration_artifact_migrations_differ")
    descriptor = os.open(directory / "force.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(request, stream, indent=2, sort_keys=True)
        _ = stream.write("\n")
    return artifact, request


def verify(artifact: Path, request_path: Path, root: Path, tool_revision: str) -> None:
    """Recheck staging authorization, exact release bytes and clean tooling before transfer."""
    _ = artifact_path(artifact)
    request = force_release.read_object(request_path)
    force_release.validate_request(request)
    require(
        request["deploy"] is False and request["quietSeconds"] == 0,
        "migration_artifact_must_only_stage",
    )
    inputs, migrations = source_identity(
        root,
        tool_revision,
        string_value(request["revision"]),
        string_value(request["repository"]),
        request_path.parent,
    )
    require(request["sourceInputsSha256"] == inputs, "migration_artifact_source_changed")
    manifest = force_release.validate_artifact(artifact, request)
    require(manifest["migrations"] == migrations, "migration_artifact_migrations_differ")
