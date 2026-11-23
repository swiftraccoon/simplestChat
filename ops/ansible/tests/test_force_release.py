"""Force admission and ordinary release safeguards with inert offline artifacts."""

import json
import unittest
from pathlib import Path
from typing import override

from test_support import ROOT

# isort: split
import force_release as force
import release_public as release
import test_public_release as public_tests
from release_json import JsonObject, JsonValue, decode_json, object_value
from test_public_release import NEW_IMAGE, OLD_IMAGE, REVISION


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


if __name__ == "__main__":
    _ = unittest.main()
