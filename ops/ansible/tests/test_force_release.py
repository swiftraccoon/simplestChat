"""Force admission and ordinary release safeguards with inert offline artifacts."""

import json
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path
from typing import override
from unittest.mock import patch

from test_support import ROOT, yaml_value

# isort: split
import force_deploy
import force_release as force
import release_build as build
import release_public as release
import test_public_release as public_tests
from release_artifact import ArtifactError, Manifest, sha256_file
from release_json import JsonObject, JsonValue, decode_json, json_value, object_value
from test_public_release import NEW_IMAGE, OLD_IMAGE, REVISION
from test_release_build import image_archive, release_manifest


def write_json(path: Path, value: JsonValue) -> None:
    """Write ordinary private test evidence only."""
    _ = path.write_text(json.dumps(value))
    path.chmod(0o600)


def request(directory: Path, inputs: JsonObject) -> JsonObject:
    """Authorize only the inert fixture's exact local artifact bytes."""
    return {
        "schemaVersion": 1,
        "operation": "force-release",
        "forced": True,
        "githubAttested": False,
        "ciVerification": "skipped-explicit-force",
        "repository": "owner/repo",
        "revision": REVISION,
        "deploy": True,
        "quietSeconds": 0,
        "sourceInputsSha256": inputs,
        "fileDigests": force.file_hashes(directory),
    }


class ForceArtifactTests(unittest.TestCase):
    """Validate actual archive and source metadata, without builds, Docker or SSH."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize the fixture's explicitly typed paths and evidence."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.artifact: Path = ROOT
        self.evidence: Path = ROOT
        self.inputs: JsonObject = {}
        self.request: JsonObject = {}
        self.manifest: Manifest = public_tests.fixture_manifest("0" * 64)

    @override
    def setUp(self) -> None:
        self.assertFalse((ROOT / "results").is_symlink())
        (ROOT / "results").mkdir(mode=0o700, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.artifact = self.root / "artifact"
        self.artifact.mkdir(mode=0o700)
        self.evidence = self.root / "evidence"
        self.evidence.mkdir(mode=0o700)
        self.inputs = {}
        for name in force.SOURCE_INPUTS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            _ = path.write_text(name)
            self.inputs[name] = sha256_file(path)
        migrations = self.root / "migrations"
        migrations.mkdir()
        _ = (migrations / "001_fixture.sql").write_text("SELECT 1;\n")
        image_archive(self.artifact / "image.tar")
        self.manifest = release_manifest(self.artifact / "image.tar")
        write_json(self.artifact / "release.json", json_value(self.manifest))
        write_json(
            self.artifact / "source.json", {"revision": REVISION, "inputsSha256": self.inputs}
        )
        write_json(
            self.artifact / "outcome.json",
            {
                "schemaVersion": 1,
                "revision": REVISION,
                "passed": True,
                "error": None,
            },
        )
        self.request = request(self.artifact, self.inputs)

    def test_local_export_is_bound_to_exact_checkout_and_preserved_as_unsigned(self) -> None:
        """Current source hashes and migrations remain mandatory for an explicit force."""
        directory, actual = force_deploy.prepare(
            self.evidence,
            self.root,
            REVISION,
            {
                "artifactDirectory": str(self.artifact),
                "repository": "owner/repo",
                "deploy": True,
                "quietSeconds": 0,
            },
        )
        self.assertEqual(directory, self.artifact)
        self.assertEqual(actual, self.request)
        self.assertEqual(force.validate_artifact(directory, actual), self.manifest)
        self.assertFalse(actual["githubAttested"])

    def test_default_force_builds_off_host_through_the_existing_builder(self) -> None:
        """No local artifact selector means a normal reviewed production build."""
        with patch.object(build, "build_release", return_value=self.artifact) as builder:
            _ = force_deploy.prepare(
                self.evidence,
                self.root,
                REVISION,
                {
                    "artifactDirectory": None,
                    "repository": "owner/repo",
                    "deploy": True,
                    "quietSeconds": 0,
                },
            )
        builder.assert_called_once_with(self.evidence / "artifact", 3600, root=self.root)

    def test_authorization_cannot_claim_signed_or_successful_ci(self) -> None:
        """Only the complete explicit unsigned request is accepted."""
        for mutation in (
            {"forced": False},
            {"githubAttested": True},
            {"ciVerification": "passed"},
            {"revision": "main"},
            {"ciRunId": 123},
            {"quietSeconds": True},
            {"deploy": "true"},
        ):
            with self.subTest(mutation=mutation), self.assertRaises(release.ReleaseError):
                force.validate_request({**self.request, **mutation})

    def test_archive_tampering_wrong_source_and_failed_build_are_rejected(self) -> None:
        """Integrity and source/build checks apply even when GitHub gates are skipped."""
        for name, change in (
            ("image.tar", b"different"),
            (
                "source.json",
                json.dumps({"revision": "b" * 40, "inputsSha256": self.inputs}).encode(),
            ),
            (
                "outcome.json",
                json.dumps(
                    {"schemaVersion": 1, "revision": REVISION, "passed": False, "error": None}
                ).encode(),
            ),
        ):
            original = (self.artifact / name).read_bytes()
            try:
                _ = (self.artifact / name).write_bytes(change)
                mutated = deepcopy(self.request)
                if name != "image.tar":
                    mutated["fileDigests"] = force.file_hashes(self.artifact)
                with self.subTest(name=name), self.assertRaises(release.ReleaseError):
                    _ = force.validate_artifact(self.artifact, mutated)
            finally:
                _ = (self.artifact / name).write_bytes(original)

    def test_source_changes_and_symlinks_fail_before_remote_work(self) -> None:
        """A digest-bearing receipt cannot replace the current checkout identity."""
        _ = (self.root / "Cargo.lock").write_text("changed")
        with self.assertRaises(release.ReleaseError):
            _ = force_deploy.prepare(
                self.evidence,
                self.root,
                REVISION,
                {
                    "artifactDirectory": str(self.artifact),
                    "repository": "owner/repo",
                    "deploy": True,
                    "quietSeconds": 0,
                },
            )
        original = self.artifact / "outcome.json"
        target = self.artifact / "real-outcome.json"
        _ = original.rename(target)
        original.symlink_to(target)
        with self.assertRaises(release.ReleaseError):
            _ = force.file_hashes(self.artifact)

    def test_wrong_runtime_image_is_rejected_even_with_matching_transfer_hashes(self) -> None:
        """Unsigned admission still requires the production image's non-root identity."""
        image_archive(self.artifact / "image.tar", user="0:0")
        manifest = release_manifest(self.artifact / "image.tar")
        write_json(self.artifact / "release.json", json_value(manifest))
        self.request["fileDigests"] = force.file_hashes(self.artifact)
        with self.assertRaises(ArtifactError):
            _ = force.validate_artifact(self.artifact, self.request)

    def test_changed_migrations_or_checkout_cannot_be_admitted(self) -> None:
        """A stale export cannot substitute its source ledger or a previous clean revision."""
        write_json(self.evidence / "force.json", self.request)
        with (
            patch.object(build, "clean_revision", return_value="b" * 40),
            self.assertRaises(build.BuildError),
        ):
            force_deploy.verify(self.artifact, self.evidence / "force.json", self.root, REVISION)
        _ = (self.root / "migrations/001_fixture.sql").write_text("SELECT 2;\n")
        with self.assertRaisesRegex(build.BuildError, "force_migrations_differ_from_source"):
            _ = force_deploy.prepare(
                self.evidence,
                self.root,
                REVISION,
                {
                    "artifactDirectory": str(self.artifact),
                    "repository": "owner/repo",
                    "deploy": True,
                    "quietSeconds": 0,
                },
            )


class ForceRuntimeTests(unittest.TestCase):
    """Exercise real locked stage/deploy/rollback logic through its owned command fixture."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize the ordinary release fixture without starting any external services."""
        super().__init__(method_name)
        self.fixture: public_tests.PublicReleaseTests = public_tests.PublicReleaseTests()
        self.directory: Path = ROOT
        self.request: JsonObject = {}

    @override
    def setUp(self) -> None:
        self.fixture = public_tests.PublicReleaseTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        root = self.fixture.root
        self.directory = root / "forced" / ("f" * 32)
        self.directory.parent.mkdir(mode=0o700)
        _ = self.fixture.directory.rename(self.directory)
        inputs: JsonObject = dict.fromkeys(force.SOURCE_INPUTS, "a" * 64)
        write_json(self.directory / "source.json", {"revision": REVISION, "inputsSha256": inputs})
        write_json(
            self.directory / "outcome.json",
            {
                "schemaVersion": 1,
                "revision": REVISION,
                "passed": True,
                "error": None,
            },
        )
        self.request = request(self.directory, inputs)
        write_json(self.directory / "force.json", self.request)

    def outcome(self) -> JsonObject:
        """Read the force operation's one private final report."""
        paths = list((self.fixture.root / "results").glob("force.*/outcome.json"))
        self.assertEqual(len(paths), 1)
        return object_value(decode_json(paths[0].read_bytes()))

    def test_force_uses_checked_backup_and_changes_only_the_application(self) -> None:
        """Unsigned admission retains the ordinary backup, readiness and continuity checks."""
        result = force.execute(self.directory)
        self.assertTrue(result["passed"])
        self.assertFalse(result["githubAttested"])
        self.assertIn("backupSha256", result)
        self.assertEqual(self.fixture.runner.app_image, NEW_IMAGE)
        self.assertTrue(self.fixture.runner.database_healthy and self.fixture.runner.proxy_running)
        self.assertEqual(self.outcome(), result)
        self.assertTrue((self.fixture.directory / "force.json").is_file())

    def test_candidate_failure_rolls_back_once_and_remains_failed(self) -> None:
        """Force never bypasses candidate health or turns successful rollback into success."""
        self.fixture.runner.candidate_unready = True
        with self.assertRaises(release.ReleaseError):
            _ = force.execute(self.directory)
        self.assertEqual(self.fixture.runner.app_image, OLD_IMAGE)
        self.assertEqual(self.fixture.runner.ups, 2)
        result = self.outcome()
        self.assertFalse(result["passed"])
        self.assertTrue(result["rollbackAttempted"] and result["rollbackPassed"])

    def test_existing_journal_and_artifact_collisions_cannot_be_forced(self) -> None:
        """Force is authorization for unsigned input, never permission to override ownership."""
        write_json(
            self.fixture.root / "release-state.json",
            {
                "schemaVersion": 1,
                "finalized": False,
                "phase": "unfinished",
            },
        )
        with self.assertRaises(release.ReleaseError):
            _ = force.execute(self.directory)
        self.assertEqual(self.fixture.runner.calls, [])
        (self.fixture.root / "release-state.json").unlink()
        self.fixture.directory.mkdir(mode=0o700)
        write_json(self.fixture.directory / "release.json", {"different": True})
        before = (self.fixture.directory / "release.json").read_bytes()
        with self.assertRaisesRegex(release.ReleaseError, "retained_artifact_differs"):
            _ = force.execute(self.directory)
        self.assertEqual((self.fixture.directory / "release.json").read_bytes(), before)
        self.assertEqual(self.fixture.runner.calls, [])

    def test_explicit_stage_preserves_running_application_for_maintenance(self) -> None:
        """Only the separate maintenance command may apply new schema or configuration."""
        self.request["deploy"] = False
        write_json(self.directory / "force.json", self.request)
        result = force.execute(self.directory)
        self.assertTrue(result["passed"])
        self.assertEqual(result["action"], "force-stage")
        self.assertEqual(self.fixture.runner.app_image, OLD_IMAGE)
        self.assertEqual(self.fixture.app_mutations(), [])

    def test_schema_change_still_refuses_app_only_force_before_any_stop(self) -> None:
        """Force cannot bypass the live ledger equality that requires explicit maintenance."""
        self.fixture.runner.database_migrations = {"1": "d" * 96}
        with self.assertRaisesRegex(release.ReleaseError, "Schema changes require"):
            _ = force.execute(self.directory)
        self.assertEqual(self.fixture.app_mutations(), [])
        self.assertFalse(self.outcome()["passed"])

    def test_identical_staged_artifact_accepts_a_new_deployment_authorization(self) -> None:
        """Changing stage/quiet options must not collide with immutable image bytes."""
        self.request["deploy"] = False
        write_json(self.directory / "force.json", self.request)
        _ = force.execute(self.directory)
        first = (self.fixture.directory / "force.json").read_bytes()
        next_directory = self.directory.parent / ("e" * 32)
        next_directory.mkdir(mode=0o700)
        for name in force.FILES:
            target = next_directory / name
            _ = target.write_bytes((self.directory / name).read_bytes())
            target.chmod(0o600)
        self.request["deploy"] = True
        write_json(next_directory / "force.json", self.request)
        result = force.execute(next_directory)
        self.assertTrue(result["passed"])
        self.assertEqual(self.fixture.runner.app_image, NEW_IMAGE)
        self.assertEqual((self.fixture.directory / "force.json").read_bytes(), first)


class ForcePlaybookTests(unittest.TestCase):
    """Check the maintained playbook's ordering and explicit authorization boundary."""

    def test_force_revalidates_before_remote_preflight_and_never_calls_signed_receiver(
        self,
    ) -> None:
        """Unsigned input has a distinct admission path with the shared runtime checks."""
        value = yaml_value((ROOT / "ops/ansible/force-release.yml").read_text())
        play = object_value(value[0]) if isinstance(value, list) else {}
        source = json.dumps(play)
        self.assertIn("scpub_force_release is sameas true", source)
        self.assertLess(source.index("force_deploy.verify"), source.index("release_preflight.py"))
        self.assertIn("force_release.py", source)
        self.assertIn("--force", source)
        self.assertNotIn("fetch-release.py", source)
        self.assertNotIn("public-smoke", source)
        self.assertNotIn("prune", source)


if __name__ == "__main__":
    _ = unittest.main()
