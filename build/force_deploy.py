"""Prepare explicit unsigned deployments from the current clean checkout only."""

from pathlib import Path

import force_release
import release_build as build
from release_artifact import sha256_file
from release_json import JsonObject


def prepare(
    directory: Path, root: Path, revision: str, options: JsonObject
) -> tuple[Path, JsonObject]:
    """Build off-host or validate the selected exact-HEAD export before remote access."""
    selected = options.get("artifactDirectory")
    if selected is None:
        artifact = build.build_release(directory / "artifact", 3600, root=root)
    else:
        if not isinstance(selected, str):
            message = "invalid_force_artifact_directory"
            raise build.BuildError(message)
        artifact = Path(selected)
        if artifact.is_symlink() or not artifact.is_dir() or artifact.resolve() != artifact:
            message = "invalid_force_artifact_directory"
            raise build.BuildError(message)
    request: JsonObject = {
        "schemaVersion": 1,
        "operation": "force-release",
        "forced": True,
        "githubAttested": False,
        "ciVerification": "skipped-explicit-force",
        "repository": options["repository"],
        "revision": revision,
        "deploy": options["deploy"],
        "quietSeconds": options["quietSeconds"],
        "fileDigests": force_release.file_hashes(artifact),
        "sourceInputsSha256": {
            name: sha256_file(root / name) for name in force_release.SOURCE_INPUTS
        },
    }
    manifest = force_release.validate_artifact(artifact, request)
    if manifest["migrations"] != build.migration_checksums(root):
        message = "force_migrations_differ_from_source"
        raise build.BuildError(message)
    build.write_json(directory / "force.json", request)
    return artifact, request


def verify(directory: Path, request_path: Path, root: Path, revision: str) -> None:
    """Recheck controller bytes and clean source before the playbook contacts its host."""
    request = force_release.read_object(request_path)
    manifest = force_release.validate_artifact(directory, request)
    if (
        manifest["revision"] != revision
        or build.clean_revision(build.Runner(), root) != revision
        or request["sourceInputsSha256"]
        != {name: sha256_file(root / name) for name in force_release.SOURCE_INPUTS}
        or manifest["migrations"] != build.migration_checksums(root)
    ):
        message = "force_checkout_changed"
        raise build.BuildError(message)
