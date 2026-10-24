"""Verify whole reference flows without supplied per-action successful receipts."""

from __future__ import annotations

import io
import json
import math
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from typing import cast

from test_support import ROOT

# isort: split
import vps_capacity_oracle as oracle
import vps_capacity_remote as remote


def observation(scenario: str) -> dict[str, object]:
    """Read actual driver output through the existing checked object helper."""
    value = oracle.run_scenario(scenario)
    remote.require(
        value["schemaVersion"] == 1 and value["scenario"] == scenario, "fixture_oracle_wrapper"
    )
    return remote.obj(value["observation"])


class WholeOracleTests(unittest.TestCase):
    """Retain actual request/unit/evidence/recovery policy while external tools are inert."""

    def test_success_is_deterministic_and_runs_the_complete_reference_transaction(self) -> None:
        """Actual preparation/build/worker/collection/archive calls determine this result."""
        result = observation("success")
        self.assertEqual(result, observation("success"))
        self.assertEqual(
            result["actions"],
            [
                "preflight",
                "reserve",
                "ansible",
                "release",
                "build",
                "status",
                "start",
                "status",
                "collect",
                "archive",
            ],
        )
        run = remote.obj(remote.array(result["runs"])[0])
        self.assertTrue(run["passed"] is True)
        self.assertTrue(run["cleanupClean"] is True)
        self.assertTrue(result["childPipesClosed"] is True)
        self.assertEqual(run["remoteOutcome"], "collected")
        self.assertEqual(run["legacyTransportAction"], "start")
        request = remote.obj(run["request"])
        self.assertNotIn("action", request)
        self.assertEqual(request["runtimeSeconds"], 120)
        self.assertEqual(request["speakers"], 30)
        self.assertTrue(request["audioOnly"] is True)
        self.assertEqual(result["innerRun"], oracle.INNER_RUN)
        self.assertNotEqual(request["run"], result["innerRun"])
        outcome = remote.obj(cast("object", json.loads(remote.text(run["privateOutcomeJson"]))))
        passing = remote.obj(remote.array(outcome["passingSteps"])[0])
        self.assertTrue(math.isinf(cast("float", passing["privateObservation"])))
        self.assertNotIn("PRIVATE_DO_NOT_PROJECT", json.dumps(result))
        self.assertIn("workload/calibration.json", remote.array(result["retainedFiles"]))
        self.assertIn("--property=RuntimeMaxSec=120", remote.array(result["systemdProperties"]))
        self.assertIn("--property=TimeoutStopSec=600", remote.array(result["systemdProperties"]))

    def test_failed_quality_is_not_masked_by_child_success_or_clean_collection(self) -> None:
        """The external child exits0 while actual assessment rejects its failed step."""
        result = observation("quality-failure")
        run = remote.obj(remote.array(result["runs"])[0])
        self.assertTrue(run["passed"] is False)
        self.assertTrue(run["cleanupClean"] is True)
        outcome = remote.obj(cast("object", json.loads(remote.text(run["privateOutcomeJson"]))))
        self.assertEqual(outcome["exitStatus"], 0)
        self.assertEqual(len(remote.array(outcome["failedSteps"])), 1)
        self.assertEqual(remote.obj(run["unit"])["ExecMainStatus"], "1")

    def test_lost_start_ack_retains_actual_run_without_retry(
        self,
    ) -> None:
        """The real worker runs before the inert transport drops its acknowledgement."""
        result = observation("lost-start-ack")
        actions = remote.array(result["actions"])
        self.assertEqual(actions.count("start"), 1)
        self.assertEqual(actions[-1], "start")
        run = remote.obj(remote.array(result["runs"])[0])
        self.assertTrue(run["passed"] is False)
        self.assertEqual(run["remoteOutcome"], "start_requested")
        self.assertIn("outcome.json", remote.array(result["retainedFiles"]))
        self.assertIn("request.json", remote.array(result["retainedFiles"]))

    def test_clean_recovery_keeps_the_old_failed_measurement_failed(self) -> None:
        """Original recovery proves cleanup but never rewrites the worker's failed outcome."""
        result = observation("recover-clean")
        runs = [remote.obj(value) for value in remote.array(result["runs"])]
        self.assertEqual(len(runs), 2)
        self.assertTrue(all(run["passed"] is False for run in runs))
        self.assertTrue(runs[-1]["recoveryFinalized"] is True)
        self.assertTrue(runs[-1]["outcomePassed"] is False)
        self.assertEqual(result["containers"], [])
        self.assertEqual(
            result["destructive"],
            [["stop", "--time", "20", oracle.OWNED], ["rm", "--volumes", oracle.OWNED]],
        )
        current = remote.obj(result["current"])
        self.assertTrue(current["finalized"] is True)
        self.assertTrue(current["passed"] is False)

    def test_unknown_residual_is_retained_and_cannot_finalize_the_canonical_journal(self) -> None:
        """Real residual ownership and reinspection select only the known generator ID."""
        result = observation("recover-unknown")
        run = remote.obj(remote.array(result["runs"])[-1])
        self.assertTrue(run["passed"] is False)
        self.assertTrue(run["recoveryFinalized"] is False)
        self.assertEqual(result["containers"], [oracle.UNKNOWN])
        self.assertEqual(run["recoveryRemoved"], [oracle.OWNED])
        self.assertTrue(remote.obj(result["current"])["finalized"] is False)
        self.assertNotIn(oracle.UNKNOWN, json.dumps(result["destructive"]))
        self.assertEqual(remote.array(result["actions"]).count("start"), 1)

    def test_missing_outcome_is_failed_without_finalizing_an_unfinished_journal(self) -> None:
        """An empty residual snapshot cannot repair a missing actual worker record."""
        result = observation("missing-outcome")
        run = remote.obj(remote.array(result["runs"])[0])
        self.assertTrue(run["passed"] is False)
        self.assertTrue(run["cleanupClean"] is True)
        self.assertTrue(remote.obj(result["current"])["finalized"] is False)
        outcome = remote.obj(cast("object", json.loads(remote.text(run["privateOutcomeJson"]))))
        self.assertEqual(outcome["failureClass"], "worker_outcome_missing")

    def test_saved_terminal_unit_survives_actual_collection_then_simulated_gc(self) -> None:
        """Fresh collection uses original saved Unit/request facts after the stop boundary."""
        result = observation("unit-gc")
        runs = [remote.obj(value) for value in remote.array(result["runs"])]
        self.assertTrue(all(run["passed"] is True for run in runs))
        self.assertTrue(remote.obj(runs[-1]["unit"])["Retained"] is True)
        self.assertEqual(remote.array(result["actions"]).count("start"), 1)
        self.assertEqual(remote.array(result["actions"]).count("collect"), 2)
        self.assertEqual(remote.obj(runs[-1]["request"])["runtimeSeconds"], 120)
        # This records the reference ambiguity, rather than modifying its oracle:
        # the new Rust lookup-before-wait contract intentionally uses saved120.
        self.assertEqual(
            remote.obj(remote.array(result["legacyStatusRequests"])[-1])["runtimeSeconds"], 7200
        )

    def test_failed_build_cannot_start_the_worker(self) -> None:
        """The original wait/build-result policy stops the transaction at its failed unit."""
        result = observation("build-failure")
        self.assertNotIn("start", remote.array(result["actions"]))
        self.assertEqual(result["capacityArguments"], [])
        self.assertIsNone(result["current"])
        run = remote.obj(remote.array(result["runs"])[0])
        self.assertEqual(run["failureClass"], "image_build_failed")
        self.assertTrue(run["passed"] is False)

    def test_closed_scenario_and_cli_emit_one_bounded_ordinary_json_record(self) -> None:
        """Private Infinity stays inside a string and never changes closed control JSON."""
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(oracle.main(["--scenario", "success"]), 0)
        raw = output.getvalue()
        self.assertEqual(raw.count("\n"), 1)
        self.assertLessEqual(len(raw.encode()), oracle.MAX_OUTPUT)
        value = remote.obj(cast("object", json.loads(raw)))
        self.assertEqual(value["schemaVersion"], 1)
        with self.assertRaises(ValueError):
            _ = oracle.run_scenario("not-a-scenario")

    def test_unknown_fixture_command_never_reaches_a_real_tool(self) -> None:
        """The external adapter refuses an unmodeled process instead of executing it."""
        with tempfile.TemporaryDirectory() as directory:
            host = oracle.Host(Path(directory).resolve(), "success")
            with self.assertRaises(AssertionError):
                _ = host.command(["unexpected-fixture-tool"])
        self.assertTrue(ROOT.is_dir())


if __name__ == "__main__":
    _ = unittest.main()
