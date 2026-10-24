"""Check the finite complete original stage driver without an engine or network."""

from __future__ import annotations

import json
import os
import stat
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

import release_stage_oracle as oracle
from release_json import decode_json
from test_support import obj

# isort: split
if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

DIRECTORY_MODE = 0o700


class WholeStageOracleTests(unittest.TestCase):
    """Assert real original effects and child settlement, not scripted stage verdicts."""

    def test_finite_complete_scenarios_settle_and_keep_original_sources(self) -> None:
        """Every row executes actual main, retaining exact original names and the full census."""
        self.assertEqual(len(oracle.SCENARIOS), len(set(oracle.SCENARIOS)))
        for scenario in oracle.SCENARIOS:
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                observed = obj(result, "observation")
                statuses = cast("list[JsonValue]", observed["exitCodes"])
                self.assertEqual(all(code == 0 for code in statuses), scenario in oracle.POSITIVE)
                self.assertEqual(
                    observed["workerSettled"],
                    {"status": 0, "pipesClosed": True, "reaped": True, "stderrBytes": 0},
                )
                self.assertTrue(observed["lockReleased"])
                self.assertTrue(observed["configUnchanged"])
                self.assertTrue(observed["inputUnchanged"])
                self.assertTrue(observed["sourceUnchanged"])
                self.assertTrue(observed["originalPythonUnchanged"])
                self.assertTrue(observed["originalBaselinePythonUnchanged"])
                baseline = obj(observed, "originalBaselinePythonPins")
                self.assertEqual(set(baseline), set(oracle.BASELINE_PYTHON_PATHS))
                self.assertEqual(observed["originalBaselinePythonCount"], len(baseline))
                self.assertGreaterEqual(cast("int", observed["originalPythonCount"]), len(baseline))
                for child in cast("list[JsonValue]", observed["children"]):
                    facts = obj(child)
                    self.assertIs(type(facts["status"]), int)
                    self.assertTrue(facts["pipesClosed"])
                    self.assertTrue(facts["logsClosed"])

    def test_success_keeps_fixed_daemon_order_and_outcome_after_real_unlock(self) -> None:
        """The real nonblocking flock probe distinguishes final outcome from body journals."""
        observed = obj(oracle.run("success"), "observation")
        calls = cast("list[list[str]]", obj(observed, "daemon")["calls"])
        self.assertEqual(
            [call[2] + (":" + call[3] if call[2] == "image" else "") for call in calls],
            ["image:load", "image:inspect", "create", "start", "wait", "logs", "rm"],
        )
        for call in calls:
            self.assertEqual(call[:2], ["--host", "unix:///var/run/docker.sock"])
        writes = [obj(value) for value in cast("list[JsonValue]", observed["writes"])]
        self.assertTrue(all(value["lockHeld"] is True for value in writes[:-1]))
        self.assertFalse(writes[-1]["lockHeld"])
        self.assertEqual(writes[-1]["path"], "$PUBLIC/results/$ATTEMPT/outcome.json")
        self.assertEqual(obj(observed, "releaseState")["phase"], "staged")
        self.assertIsNone(obj(observed, "daemon")["activeValidator"])
        directories = obj(observed, "directoryModes")
        self.assertTrue(directories)
        self.assertTrue(all(mode == DIRECTORY_MODE for mode in directories.values()))
        self.assertIn("$PUBLIC/results/$ATTEMPT", directories)

    def test_received_pair_keeps_real_existing_hardlinks(self) -> None:
        """Stage the normal received pair while both original aliases stay retained."""
        observed = obj(oracle.run("received-hardlink-pair"), "observation")
        self.assertEqual(observed["exitCodes"], [0])
        self.assertEqual(observed["inputLinkCountsBefore"], {"image.tar": 2, "release.json": 2})
        self.assertEqual(observed["inputLinkCountsAfter"], observed["inputLinkCountsBefore"])
        self.assertTrue(observed["inputUnchanged"])

    def test_repeat_rechecks_daemon_without_second_import_or_validator(self) -> None:
        """A saved staged record never substitutes for archive and live daemon checks."""
        observed = obj(oracle.run("repeat"), "observation")
        self.assertEqual(observed["exitCodes"], [0, 0])
        calls = cast("list[list[str]]", obj(observed, "daemon")["calls"])
        self.assertEqual(len(calls), 8)
        self.assertEqual(calls[-1][2:4], ["image", "inspect"])
        self.assertEqual(sum(call[2:4] == ["image", "load"] for call in calls), 1)
        self.assertEqual(len(cast("list[JsonValue]", observed["attempts"])), 2)

    def test_exact_disk_boundary_refuses_before_import_journal(self) -> None:
        """The reference uses strictly greater than twice the TAR plus one GiB."""
        for name in ("disk-below", "disk-exact"):
            with self.subTest(scenario=name):
                observed = obj(oracle.run(name), "observation")
                self.assertEqual(obj(observed, "daemon")["calls"], [])
                self.assertIsNone(observed["releaseState"])
                self.assertEqual(observed["exitCodes"], [1])
        self.assertEqual(obj(oracle.run("disk-plus"), "observation")["exitCodes"], [0])

    def test_lost_create_identity_remains_unknown_and_is_not_name_cleaned(self) -> None:
        """The original leaves its unresolved validator journal for explicit recovery."""
        observed = obj(oracle.run("create-invalid"), "observation")
        self.assertEqual(obj(observed, "daemon")["activeValidator"], "unknown")
        self.assertEqual(obj(observed, "releaseState")["phase"], "validate_image")
        self.assertFalse(obj(observed, "releaseState")["finalized"])
        calls = cast("list[list[str]]", obj(observed, "daemon")["calls"])
        self.assertFalse(any(call[2] == "rm" for call in calls))

    def test_known_validator_failure_preserves_original_effect_for_native_profile(self) -> None:
        """Native cleanup differences must be explicit, never repaired inside the reference."""
        observed = obj(oracle.run("wait-nonzero"), "observation")
        self.assertEqual(observed["exitCodes"], [1])
        self.assertEqual(obj(observed, "daemon")["activeValidator"], "known")
        self.assertFalse(obj(observed, "releaseState")["finalized"])

    def test_daemon_archive_binding_is_an_explicit_reference_positive_profile(self) -> None:
        """The original accepts another valid same-revision ID; native deliberately refuses it."""
        result = oracle.run("daemon-outside-graph")
        self.assertEqual(obj(result, "input")["nativeProfile"], "archive-graph-image-binding")
        self.assertEqual(obj(result, "observation")["exitCodes"], [0])

    def test_atomic_sync_prefix_and_earlier_staged_effect_remain_observable(self) -> None:
        """File-sync keeps a temp prefix; directory-sync has already replaced the target."""
        file_sync = obj(oracle.run("staged-file-sync"), "observation")
        directory_sync = obj(oracle.run("staged-directory-sync"), "observation")
        self.assertNotIn("staged.json", obj(file_sync, "releaseFiles"))
        self.assertTrue(
            any(name.startswith(".release-") for name in obj(file_sync, "releaseFiles"))
        )
        self.assertIn("staged.json", obj(directory_sync, "releaseFiles"))
        for observed in (file_sync, directory_sync):
            self.assertEqual(observed["exitCodes"], [1])
            self.assertTrue(observed["lockReleased"])
            reports = cast("list[JsonValue]", observed["reports"])
            self.assertTrue(reports)
            self.assertTrue(all(obj(report)["passed"] is False for report in reports))

    def test_outcome_failure_cannot_convert_prior_body_success_into_status_zero(self) -> None:
        """Final status remains negative after a complete outcome and failed parent sync."""
        observed = obj(oracle.run("outcome-directory-sync"), "observation")
        self.assertEqual(observed["exitCodes"], [1])
        self.assertEqual(observed["reports"], [])
        attempts = cast("list[JsonValue]", observed["attempts"])
        self.assertEqual(len(attempts), 1)
        outcome = obj(attempts[0], "outcome.json", "value")
        self.assertTrue(outcome["passed"])
        self.assertTrue(observed["lockReleased"])

    def test_explicit_original_names_cannot_be_hidden_by_extra_maintained_sources(self) -> None:
        """A count-preserving replacement still loses a required original name."""
        current = oracle.python_pins()
        before = oracle.baseline_pins(current)
        original = oracle.BASELINE_PYTHON_PATHS[0]
        changed = dict(current)
        del changed[original]
        changed["build/new-inert-source.py"] = before[original]
        self.assertGreaterEqual(len(changed), len(oracle.BASELINE_PYTHON_PATHS))
        with self.assertRaises(ValueError):
            _ = oracle.baseline_pins(changed)
        changed = dict(current)
        changed["build/new-inert-source.py"] = before[original]
        self.assertEqual(oracle.baseline_pins(changed), before)
        changed[original] = {"sha256": "0" * 64, "bytes": 1, "mode": 0o600}
        self.assertNotEqual(oracle.baseline_pins(changed), before)

    def test_complete_create_close_refuses_without_discarding_observed_daemon_effect(self) -> None:
        """Retain the original effect; native cleanup is an explicit stronger profile."""
        for scenario in ("create-close", "create-close-remove-failure"):
            with self.subTest(scenario=scenario):
                result = oracle.run(scenario)
                observed = obj(result, "observation")
                self.assertEqual(obj(result, "input")["nativeProfile"], "known-create-completion")
                self.assertEqual(observed["exitCodes"], [1])
                reports = cast("list[JsonValue]", observed["reports"])
                self.assertEqual(obj(reports[0])["failure"], "OSError")
                self.assertEqual(obj(observed, "daemon")["activeValidator"], "known")
                children = cast("list[JsonValue]", observed["children"])
                self.assertEqual(len(children), 3)
                self.assertTrue(all(obj(child)["status"] == 0 for child in children))
                self.assertTrue(all(obj(child)["pipesClosed"] is True for child in children))
                self.assertTrue(all(obj(child)["logsClosed"] is True for child in children))
                self.assertFalse(obj(observed, "releaseState")["finalized"])

    def test_full_known_atomic_temps_retain_independent_raw_pins_and_own_framing(self) -> None:
        """Only the two closed complete records get a bounded semantic view."""
        for role in ("staged", "outcome"):
            with self.subTest(role=role), tempfile.TemporaryDirectory() as name:
                root = Path(name).resolve()
                control, manifest, archive = oracle.fixture(role + "-file-sync")
                oracle.populate(root, control, manifest, archive)
                if role == "staged":
                    parent = root / "public/releases" / oracle.REVISION
                    inspection = obj(decode_json(cast("str", obj(control, "daemon")["inspect"])))
                    value = {
                        "schemaVersion": 1,
                        "revision": oracle.REVISION,
                        "serverImage": inspection["id"],
                        "manifestSha256": oracle.digest(manifest),
                        "stagedAt": oracle.UTC,
                    }
                else:
                    parent = root / "public/results/release.fixture"
                    oracle.directory(parent)
                    value = {
                        "action": "stage",
                        "revision": oracle.REVISION,
                        "startedAt": oracle.UTC,
                        "passed": True,
                        "phase": "complete",
                        "finishedAt": oracle.UTC,
                    }
                data = (json.dumps(value, indent=2) + "\n").encode()
                path = parent / ".release-COMPLETE"
                oracle.write(path, data)
                observed = obj(oracle.inventory(parent, root), path.name)
            self.assertEqual(observed["sha256"], oracle.digest(data))
            self.assertEqual(observed["bytes"], len(data))
            self.assertEqual(observed["mode"], 0o600)
            projection = obj(observed, "atomicTemp")
            self.assertEqual(projection["role"], role)
            self.assertTrue(projection["complete"])
            self.assertTrue(projection["ownSerializationMatches"])

    def test_tool_whitespace_and_wide_versions_keep_the_original_effect_order(self) -> None:
        """Exercise actual str/bytes stripping, splitlines and wide valid int parsing."""
        observations = {
            scenario: obj(oracle.run(scenario), "observation")
            for scenario in ("tool-whitespace", "logs-wide-version")
        }
        whitespace = observations["tool-whitespace"]
        self.assertEqual(whitespace["exitCodes"], [0])
        calls = cast("list[list[str]]", obj(whitespace, "daemon")["calls"])
        self.assertEqual(calls[-1][2:], ["rm", oracle.CONTAINER])
        wide = observations["logs-wide-version"]
        self.assertEqual(wide["exitCodes"], [1])
        calls = cast("list[list[str]]", obj(wide, "daemon")["calls"])
        self.assertEqual(calls[-1][2:], ["rm", oracle.CONTAINER])
        self.assertEqual(len(calls), 7)
        reports = cast("list[JsonValue]", wide["reports"])
        self.assertEqual(obj(reports[0])["phase"], "stage")
        self.assertEqual(
            obj(reports[0])["failure"],
            "Archive migration manifest differs from image",
        )

    def test_partial_invalid_or_unknown_temp_bytes_keep_exact_raw_prefixes(self) -> None:
        """Partial LF, duplicate keys and unknown values cannot gain completion facts."""
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            control, manifest, archive = oracle.fixture("staged-file-sync")
            oracle.populate(root, control, manifest, archive)
            parent = root / "public/releases" / oracle.REVISION
            inspection = obj(decode_json(cast("str", obj(control, "daemon")["inspect"])))
            value = {
                "schemaVersion": 1,
                "revision": oracle.REVISION,
                "serverImage": inspection["id"],
                "manifestSha256": oracle.digest(manifest),
                "stagedAt": oracle.UTC,
            }
            full = (json.dumps(value, indent=2) + "\n").encode()
            path = parent / ".release-INCOMPLETE"
            oracle.write(path, b"")
            wrong = dict(value, extra=True)
            cases = [
                full[:-1],
                b"{",
                b"[]\n",
                b"\xff",
                full.replace(b'"schemaVersion": 1', b'"schemaVersion": 1,"schemaVersion":1'),
                (json.dumps(wrong, indent=2) + "\n").encode(),
                (json.dumps(dict(value, manifestSha256="0" * 64), indent=2) + "\n").encode(),
            ]
            observations: list[tuple[bytes, int, JsonObject]] = []
            for data in cases:
                descriptor = os.open(path, os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
                with os.fdopen(descriptor, "wb") as output:
                    written = output.write(data)
                    output.flush()
                observations.append((data, written, obj(oracle.inventory(parent, root), path.name)))
        for data, written, observed in observations:
            self.assertEqual(written, len(data))
            self.assertNotIn("atomicTemp", observed)
            self.assertNotIn("value", observed)
            self.assertEqual(observed["sha256"], oracle.digest(data))
            self.assertEqual(observed["bytes"], len(data))
            if data != b"\xff":
                self.assertEqual(observed["text"], data.decode())

    def test_fixture_creation_starts_private_before_any_selected_final_mode(self) -> None:
        """Never expose incomplete fixture bytes through a caller-selected creation mask."""
        with tempfile.TemporaryDirectory() as name:
            path = Path(name).resolve() / "fixture"
            real_open = os.open
            modes: list[int] = []

            def opened(
                selected: str | bytes | os.PathLike[str] | os.PathLike[bytes],
                flags: int,
                mode: int = 0o600,
                *,
                dir_fd: int | None = None,
            ) -> int:
                modes.append(mode)
                return real_open(selected, flags, mode, dir_fd=dir_fd)

            with patch.object(os, "open", side_effect=opened):
                oracle.write(path, b"INERT_COMPLETE_BYTES", 0o644)
            final_mode = stat.S_IMODE(path.stat().st_mode)
            data = path.read_bytes()
        self.assertEqual(modes, [0o600])
        self.assertEqual(final_mode, 0o644)
        self.assertEqual(data, b"INERT_COMPLETE_BYTES")


if __name__ == "__main__":
    _ = unittest.main()
