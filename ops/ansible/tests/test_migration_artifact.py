"""Existing unsigned migration artifacts remain bound to exact ancestor sources."""

import os
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import patch

from test_support import ROOT, objects, yaml_value

# isort: split
import force_release
import migration_artifact as migration
import release_build as build
import test_force_release as force_tests
from release_artifact import sha256_file
from release_json import JsonObject, json_value
from test_force_release import write_json
from test_release_build import image_archive, release_manifest


class MigrationArtifactTests(unittest.TestCase):
    """Exercise real Git history and inert archives without Docker, SSH or network."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize typed paths and revision selectors."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.artifact: Path = ROOT
        self.evidence: Path = ROOT
        self.revision: str = ""
        self.tool_revision: str = ""
        self.inputs: JsonObject = {}

    def git(self, *arguments: str) -> str:
        """Run bounded commands in only this disposable Git repository."""
        _, output = build.Runner().run(
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
                *arguments,
            ],
            cwd=self.root,
            env={**os.environ, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull},
        )
        return output

    @override
    def setUp(self) -> None:
        """Create a retained release followed by a tool commit with different source inputs."""
        temporary = tempfile.TemporaryDirectory(dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.artifact = self.root / "artifact"
        self.evidence = self.root / "evidence"
        self.artifact.mkdir(mode=0o700)
        self.evidence.mkdir(mode=0o700)
        _ = self.git("init", "--initial-branch=main")
        _ = self.git("remote", "add", "origin", "https://github.com/owner/repo.git")
        _ = (self.root / ".gitignore").write_text("artifact/\nevidence/\n")
        for name in force_release.SOURCE_INPUTS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text(name + "\n")
            self.inputs[name] = sha256_file(path)
        (self.root / "migrations").mkdir()
        _ = (self.root / "migrations/001_fixture.sql").write_text("SELECT 1;\n")
        _ = self.git("add", ".")
        _ = self.git("commit", "--no-verify", "-m", "fixture: original release")
        self.revision = self.git("rev-parse", "HEAD")
        image_archive(
            self.artifact / "image.tar",
            revision=self.revision,
            tags=["simplestchat-release/production:" + self.revision],
        )
        manifest = release_manifest(self.artifact / "image.tar")
        manifest["revision"] = self.revision
        manifest["imageTag"] = "simplestchat-release/production:" + self.revision
        write_json(self.artifact / "release.json", json_value(manifest))
        write_json(
            self.artifact / "source.json", {"revision": self.revision, "inputsSha256": self.inputs}
        )
        write_json(
            self.artifact / "outcome.json",
            {"schemaVersion": 1, "revision": self.revision, "passed": True, "error": None},
        )
        _ = (self.root / "Dockerfile").write_text("new tool checkout\n")
        _ = (self.root / "migrations/001_fixture.sql").write_text("SELECT 2;\n")
        _ = self.git("add", ".")
        _ = self.git("commit", "--no-verify", "-m", "fixture: newer migration tools")
        self.tool_revision = self.git("rev-parse", "HEAD")

    def prepare(self) -> tuple[Path, JsonObject]:
        """Prepare one explicit retained release using this newer tool checkout."""
        return migration.prepare(
            self.evidence,
            self.root,
            self.tool_revision,
            self.revision,
            self.artifact,
            "owner/repo",
        )

    def test_ancestor_release_uses_its_own_exact_sources_and_never_deploys(self) -> None:
        """Current Dockerfile and SQL changes cannot silently relabel an older artifact."""
        artifact, request = self.prepare()
        self.assertEqual(artifact, self.artifact)
        self.assertEqual(request["sourceInputsSha256"], self.inputs)
        self.assertFalse(request["deploy"])
        self.assertFalse(request["githubAttested"])
        self.assertEqual(request["ciVerification"], "skipped-explicit-force")
        self.assertEqual((self.evidence / "force.json").stat().st_mode & 0o777, 0o600)
        migration.verify(artifact, self.evidence / "force.json", self.root, self.tool_revision)
        self.assertFalse(list(self.evidence.glob("source-inputs.*")))

    def test_source_evidence_from_new_tool_revision_is_rejected(self) -> None:
        """A matching image label cannot substitute current-source hashes for original bytes."""
        write_json(
            self.artifact / "source.json",
            {
                "revision": self.revision,
                "inputsSha256": {
                    name: sha256_file(self.root / name) for name in force_release.SOURCE_INPUTS
                },
            },
        )
        with self.assertRaisesRegex(RuntimeError, "force_source_evidence_mismatch"):
            _ = self.prepare()

    def test_migrations_must_match_selected_commit(self) -> None:
        """Matching artifact digests cannot authorize SQL from a different source revision."""
        manifest = force_release.read_object(self.artifact / "release.json")
        manifest["migrations"] = dict(build.migration_checksums(self.root))
        write_json(self.artifact / "release.json", manifest)
        with self.assertRaisesRegex(build.BuildError, "migration_artifact_migrations_differ"):
            _ = self.prepare()

    def test_changed_artifact_after_preparation_is_rejected(self) -> None:
        """The transfer-time recheck compares the retained artifact bytes again."""
        _ = self.prepare()
        with (self.artifact / "image.tar").open("ab") as stream:
            _ = stream.write(b"changed")
        with self.assertRaisesRegex(RuntimeError, "force_artifact_digest_mismatch"):
            migration.verify(
                self.artifact, self.evidence / "force.json", self.root, self.tool_revision
            )

    def test_transfer_cannot_change_stage_authorization_to_deploy(self) -> None:
        """Migration must not start an application before database transfer and sealing."""
        _, request = self.prepare()
        request["deploy"] = True
        write_json(self.evidence / "force.json", request)
        with self.assertRaisesRegex(build.BuildError, "migration_artifact_must_only_stage"):
            migration.verify(
                self.artifact, self.evidence / "force.json", self.root, self.tool_revision
            )

    def test_dirty_or_changed_tool_checkout_is_rejected(self) -> None:
        """The old artifact never permits stale or uncommitted migration tooling."""
        _ = self.prepare()
        _ = (self.root / "Dockerfile").write_text("uncommitted\n")
        with self.assertRaises(build.BuildError):
            migration.verify(
                self.artifact, self.evidence / "force.json", self.root, self.tool_revision
            )

    def test_nonancestor_existing_commit_is_rejected(self) -> None:
        """A valid object from unrelated history cannot supply the migration release."""
        tree = self.git("rev-parse", "HEAD^{tree}")
        self.revision = self.git("commit-tree", tree, "-m", "unrelated fixture")
        with self.assertRaisesRegex(build.BuildError, "migration_release_not_tool_ancestor"):
            _ = self.prepare()

    def test_repository_mismatch_is_rejected(self) -> None:
        """Artifact admission stays bound to the explicitly selected repository."""
        _ = self.git("remote", "set-url", "origin", "https://github.com/another/repo.git")
        with self.assertRaisesRegex(Exception, "repository_origin_mismatch"):
            _ = self.prepare()


class MigrationArtifactTaskTests(unittest.TestCase):
    """Guard the ordering around the maintained host staging command."""

    def test_force_transfer_rechecks_locally_and_uses_official_stage_primitives(self) -> None:
        """Neither image import nor application startup has a migration-specific bypass."""
        path = ROOT / "ops/ansible/tasks/migration-artifact.yml"
        text = path.read_text()
        tasks = objects({"tasks": yaml_value(text)}, "tasks")
        self.assertIn("scmig_force_release is sameas true", str(tasks[0]))
        self.assertIn("migration_artifact.verify", str(tasks[1]))
        self.assertEqual(tasks[1]["delegate_to"], "localhost")
        self.assertIn("release.workload_lock()", str(tasks[2]))
        self.assertIn("request['deploy'] is False", str(tasks[-2]))
        self.assertIn("force_release.py", str(tasks[-1]))
        self.assertIn("--force", str(tasks[-1]))
        self.assertNotIn("docker", text)
        self.assertNotIn("simplestchat-public-deploy", text)

    def test_official_force_stage_accepts_fresh_host_without_starting_services(self) -> None:
        """A fresh target needs no old app configuration, database or active release."""
        with patch.object(tempfile, "tempdir", str(ROOT / "results")):
            fixture = force_tests.ForceRuntimeTests()
            fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        for path in fixture.fixture.config.iterdir():
            path.unlink()
        fixture.fixture.runner.app_running = False
        fixture.fixture.runner.proxy_running = False
        fixture.fixture.runner.database_healthy = False
        fixture.request["deploy"] = False
        write_json(fixture.directory / "force.json", fixture.request)
        self.assertFalse((fixture.fixture.root / "release-state.json").exists())
        report = force_release.execute(fixture.directory)
        self.assertTrue(report["passed"])
        self.assertEqual(report["action"], "force-stage")
        self.assertFalse(fixture.fixture.runner.app_running)
        self.assertFalse(fixture.fixture.runner.proxy_running)
        self.assertEqual(list(fixture.fixture.config.iterdir()), [])
        self.assertFalse(any(kind == "compose" for kind, _, _ in fixture.fixture.runner.calls))


if __name__ == "__main__":
    _ = unittest.main()
