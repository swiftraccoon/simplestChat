"""Verify finite whole-reference traces without executing any external tool."""

from __future__ import annotations

import hashlib
import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from typing import ClassVar, override
from unittest.mock import patch

import release_build_oracle as ORACLE  # noqa: N812 -- Keep the reference-driver alias explicit.

# isort: split
from release_json import JsonObject, array_value, decode_json, object_value, string_value


class ReleaseBuildOracleTests(unittest.TestCase):
    """Check actual reference verdicts, effect ordering and deterministic records."""

    observations: ClassVar[dict[str, JsonObject]] = {}

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Execute every fixed case with an explicit refusal of subprocess creation."""
        with patch.object(subprocess, "Popen", side_effect=AssertionError("external tool")):
            cls.observations = {name: ORACLE.run_scenario(name) for name in ORACLE.SCENARIOS}

    def records(self, scenario: str) -> list[JsonObject]:
        """Read only checked case objects from a whole-driver observation."""
        return [object_value(value) for value in array_value(self.observations[scenario]["cases"])]

    def test_all_finite_cases_have_expected_verdict_and_completed_temp_cleanup(self) -> None:
        """Failures cannot be mistaken for complete builder or publication success."""
        successful = {
            ("build-success", "production"),
            ("export-missing-tag", "missing"),
            ("export-existing-tag", "matching"),
            ("custom-builder", "export-accepted"),
            ("receiver-boundary", "build-unsigned"),
            ("receiver-boundary", "export-unsigned"),
            *(("checked-env", value) for value in ("1.48", "1.49", "1.100", "2.0")),
        }
        count = 0
        self.assertEqual(len(ORACLE.SCENARIOS), 26)
        for scenario in ORACLE.SCENARIOS:
            value = self.observations[scenario]
            self.assertEqual(value["schemaVersion"], 1)
            self.assertEqual(value["scenario"], scenario)
            for record in self.records(scenario):
                count += 1
                with self.subTest(scenario=scenario, case=record["name"]):
                    expected = (scenario, record["name"]) in successful
                    self.assertEqual(record["passed"], expected)
                    self.assertEqual(record["published"], expected)
                    self.assertIs(record["temporaryClean"], True)  # noqa: FBT003
                    if expected:
                        self.assertIsNone(record["error"])
                    else:
                        self.assertIsInstance(record["error"], str)
                    outcome = record["outcome"]
                    if outcome is not None:
                        self.assertEqual(object_value(outcome)["passed"], expected)
        self.assertEqual(count, 112)

    def test_selected_build_uses_the_original_finite_production_effect_order(self) -> None:
        """Real source extraction and verification surround exactly one build and save."""
        record = self.records("build-success")[0]
        self.assertEqual(
            record["actions"],
            [
                "git-check-ignore",
                "git-head",
                "git-status",
                "docker-context",
                "docker-save-help",
                "docker-api",
                "docker-public",
                "docker-benchmark",
                "docker-builder",
                "git-archive",
                "docker-build",
                "docker-save",
                "git-head",
                "git-status",
            ],
        )
        self.assertEqual(
            record["retainedFiles"],
            [
                "image.tar",
                "outcome.json",
                "release.candidate.json",
                "release.json",
                "source.json",
            ],
        )
        manifest = object_value(record["manifest"])
        self.assertEqual(manifest["platform"], "linux/amd64")
        self.assertEqual(set(object_value(manifest["migrations"])), {"1"})

    def test_export_checks_selection_twice_and_never_builds_or_pulls(self) -> None:
        """Missing and retained tags follow distinct original no-fallback paths."""
        for scenario, tags in (("export-missing-tag", 1), ("export-existing-tag", 0)):
            record = self.records(scenario)[0]
            actions = array_value(record["actions"])
            with self.subTest(scenario=scenario):
                self.assertEqual(actions.count("docker-tag"), tags)
                self.assertEqual(actions.count("docker-inspect-tag"), 2)
                self.assertNotIn("docker-build", actions)
                self.assertNotIn("docker-builder", actions)
                self.assertLess(actions.index("docker-inspect-image"), actions.index("docker-save"))
                self.assertLess(actions.index("docker-save"), len(actions) - 2)
        for record in self.records("export-fails"):
            if object_value(record["fixture"])["export"] is True:
                self.assertNotIn("docker-build", array_value(record["actions"]))
            self.assertNotIn("release.json", array_value(record["retainedFiles"]))

    def test_rejected_selectors_and_output_have_no_docker_or_created_attempt(self) -> None:
        """Pre-admission refusal preserves the operator's existing namespace."""
        for scenario in ("invalid-selector", "invalid-output", "dirty-or-invalid-head"):
            for record in self.records(scenario):
                with self.subTest(scenario=scenario, case=record["name"]):
                    self.assertIs(record["outputCreated"], False)  # noqa: FBT003
                    self.assertIsNone(record["outcome"])
                    self.assertTrue(
                        all(
                            string_value(role).startswith("git-")
                            for role in array_value(record["actions"])
                        )
                    )

    def test_guards_and_capability_failures_precede_dangerous_effects(self) -> None:
        """Actual lock/state and probe policy blocks source archive/build/export."""
        for record in self.records("guard-blocked"):
            self.assertTrue(
                all(
                    string_value(role).startswith("git-") for role in array_value(record["actions"])
                )
            )
            self.assertIs(record["outputCreated"], True)  # noqa: FBT003 -- Exact JSON boolean.
        for scenario in ("export-capability", "probe-failure"):
            for record in self.records(scenario):
                self.assertTrue(
                    {"git-archive", "docker-build", "docker-save"}.isdisjoint(
                        array_value(record["actions"])
                    )
                )

    def test_checked_environment_retains_only_the_unrelated_original_value(self) -> None:
        """Every selected Docker command receives the scrubbed original environment."""
        for record in self.records("checked-env"):
            for value in array_value(record["commands"]):
                command = object_value(value)
                argv = array_value(command["argv"])
                if argv[:2] == ["docker", "--host"]:
                    self.assertEqual(argv[2], "unix:///var/run/docker.sock")
                    self.assertEqual(command["envOverrides"], {"FIXTURE_ENV": "preserved"})

    def test_invalid_real_archives_and_migrations_cannot_publish(self) -> None:
        """Malformed fixture bytes reach real source and server archive validation."""
        for scenario in ("source-invalid", "migrations-invalid"):
            for record in self.records(scenario):
                actions = array_value(record["actions"])
                self.assertIn("git-archive", actions)
                self.assertNotIn("docker-build", actions)
                self.assertIsNone(record["manifest"])
        for record in self.records("archive-invalid"):
            self.assertIn("docker-save", array_value(record["actions"]))
            self.assertIsNone(record["manifest"])
            self.assertIn("release.candidate.json", array_value(record["retainedFiles"]))

    def test_image_identity_failures_precede_retag_and_export(self) -> None:
        """The original image checks retain conflicting selections without overwriting."""
        for scenario in ("image-identity", "layer-metadata", "conflicting-tag"):
            for record in self.records(scenario):
                self.assertTrue(
                    {"docker-tag", "docker-build", "docker-save"}.isdisjoint(
                        array_value(record["actions"])
                    )
                )

    def test_unsigned_receiver_and_closed_source_contract_remain_separate(self) -> None:
        """Unsigned success supplies no receiver authorization or new source fields."""
        for record in self.records("receiver-boundary"):
            self.assertIs(record["receiverRejected"], True)  # noqa: FBT003 -- Exact JSON boolean.
            self.assertEqual(set(object_value(record["source"])), {"revision", "inputsSha256"})
            self.assertEqual(
                set(object_value(record["outcome"])),
                {
                    "schemaVersion",
                    "revision",
                    "startedAt",
                    "finishedAt",
                    "passed",
                    "error",
                    *(("exportedImageId",) if record["name"] == "export-unsigned" else ()),
                },
            )

    def test_original_active_deadline_prevents_save_and_retains_failure(self) -> None:
        """The real reference rejects exhausted remaining time before dispatch."""
        record = self.records("deadline-before-save")[0]
        self.assertIn("docker-build", array_value(record["actions"]))
        self.assertNotIn("docker-save", array_value(record["actions"]))
        self.assertEqual(record["error"], "Build/export deadline expired before image export")

    def test_normalized_results_are_deterministic_and_bounded_ordinary_json(self) -> None:
        """Only temporary tokens/timestamps change; exact command data remains comparable."""
        for scenario in (
            "build-success",
            "export-missing-tag",
            "source-invalid",
            "archive-invalid",
        ):
            self.assertEqual(ORACLE.run_scenario(scenario), self.observations[scenario])
        for value in self.observations.values():
            encoded = json.dumps(value, allow_nan=False)
            self.assertLess(len(encoded.encode()), ORACLE.MAX_OUTPUT)
            self.assertEqual(decode_json(encoded), value)

    def test_closed_cli_returns_one_json_record_and_refuses_unknown_before_effects(self) -> None:
        """The oracle exposes one fixed finite selector and no external execution hook."""
        stream = io.StringIO()
        with redirect_stdout(stream):
            self.assertEqual(ORACLE.main(["--scenario", "build-success"]), 0)
        self.assertEqual(decode_json(stream.getvalue()), self.observations["build-success"])
        with (
            patch.object(ORACLE, "run_scenario", side_effect=AssertionError("effect")),
            redirect_stderr(io.StringIO()),
            self.assertRaises(SystemExit) as failure,
        ):
            _ = ORACLE.main(["--scenario", "unknown"])
        self.assertEqual(failure.exception.code, 2)

    def test_exported_original_inputs_match_pins_and_successful_archive_bytes(self) -> None:
        """Candidate adapters receive independent real bytes, not inferred verdict fixtures."""
        with tempfile.TemporaryDirectory(prefix="release-oracle-export.") as value:
            base = Path(value).resolve()
            for scenario in ORACLE.SCENARIOS:
                destination = base / scenario
                observed = ORACLE.run_scenario(scenario, destination)
                self.assertEqual(destination.stat().st_mode & 0o777, 0o700)
                for item in array_value(observed["cases"]):
                    record = object_value(item)
                    pins = object_value(record["fixtureFiles"])
                    for kind in ("source", "image"):
                        pin = object_value(pins[kind])
                        path = destination / string_value(pin["path"])
                        data = path.read_bytes()
                        self.assertEqual(len(data), pin["bytes"])
                        self.assertEqual(hashlib.sha256(data).hexdigest(), pin["sha256"])
                        self.assertLessEqual(len(data), ORACLE.MAX_FIXTURE_FILE)
                    if record["passed"] is True:
                        self.assertEqual(
                            object_value(record["manifest"])["archiveSha256"],
                            object_value(pins["image"])["sha256"],
                        )

    def test_fixture_export_refuses_existing_or_aliased_output_without_overwrite(self) -> None:
        """The finite fixture writer only creates a new private destination."""
        with tempfile.TemporaryDirectory(prefix="release-oracle-export.") as value:
            base = Path(value).resolve()
            occupied = base / "occupied"
            occupied.mkdir()
            marker = occupied / "operator.txt"
            _ = marker.write_bytes(b"operator bytes")
            alias = base / "alias"
            alias.symlink_to(base, target_is_directory=True)
            for destination in (occupied, alias / "new"):
                with self.assertRaisesRegex(ValueError, "directory_rejected"):
                    _ = ORACLE.export_fixtures("build-success", destination)
            self.assertEqual(marker.read_bytes(), b"operator bytes")
            self.assertFalse((base / "new").exists())


if __name__ == "__main__":
    _ = unittest.main()
