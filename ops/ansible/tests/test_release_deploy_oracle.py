"""Exercise the closed whole unchanged deploy oracle without an engine or network."""

from __future__ import annotations

import hashlib
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import ClassVar, override

import release_deploy_oracle as oracle
from test_support import array, obj, objects, string

# isort: split
from release_json import JsonObject, integer_value

# Literal expected observation counts and fixture assertions are the contract.

POSITIVE = {
    "success",
    "prestaged",
    "received-hardlinks",
    "env-no-final-lf",
    "compose-crlf",
    "caddy-public-0644",
    "restore-without-read",
    "quiet-no-token",
    "quiet-unavailable",
    "quiet-malformed",
    "quiet-active-zero",
    "quiet-active-deadline",
    "rooms-arrive-after-quiet",
    "dump-above-inspection",
}


class DeployOracleTests(unittest.TestCase):
    """Keep actual whole-reference effects and independently measured owned settlement."""

    temporary: ClassVar[tempfile.TemporaryDirectory[str]]
    root: ClassVar[Path]
    results: ClassVar[dict[str, JsonObject]]

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Run every finite case once with at most three independently owned original workers."""
        cls.temporary = tempfile.TemporaryDirectory(prefix="deploy-oracle-suite.")
        cls.root = Path(cls.temporary.name).resolve()

        def run_scenario(scenario: str) -> tuple[str, JsonObject]:
            """Keep each complete reference process and fixture owned independently."""
            path = cls.root / scenario
            path.mkdir(mode=0o700)
            return scenario, oracle.run(scenario, path)

        # Only the read-only parent and disjoint fixtures run concurrently. Each
        # original main, patched globals and deadline stay in its own subprocess.
        # Context exit joins every admitted task even when one case refuses.
        with ThreadPoolExecutor(max_workers=3) as workers:
            cls.results = dict(workers.map(run_scenario, oracle.SCENARIOS))

    @classmethod
    @override
    def tearDownClass(cls) -> None:
        """Remove only the suite's own closed temporary fixture trees."""
        cls.temporary.cleanup()

    def observation(self, scenario: str) -> JsonObject:
        """Select the complete measured observation for one finite case."""
        return obj(self.results[scenario], "observation")

    def roles(self, scenario: str) -> list[str]:
        """Read chronological external roles, not inferred report permissions."""
        return [
            string(obj(value), "role")
            for value in objects(obj(self.observation(scenario), "daemon"), "calls")
        ]

    def test_all_closed_cases_keep_sources_and_owned_peers_settled(self) -> None:
        """Every success or refusal retains its original inputs and closes actual children."""
        self.assertEqual(set(self.results), set(oracle.SCENARIOS))
        self.assertEqual(len(set(oracle.SCENARIOS)), len(oracle.SCENARIOS))
        for scenario in oracle.SCENARIOS:
            with self.subTest(scenario=scenario):
                value = self.observation(scenario)
                self.assertEqual(value["exitCodes"], [0 if scenario in POSITIVE else 1])
                for key in (
                    "sourceUnchanged",
                    "originalPythonUnchanged",
                    "originalBaselinePythonUnchanged",
                    "lockReleased",
                ):
                    self.assertTrue(value[key] is True)
                self.assertEqual(value["inputPinsBefore"], value["inputPinsAfter"])
                self.assertEqual(value["inputIdentityBefore"], value["inputIdentityAfter"])
                self.assertEqual(
                    set(obj(value, "originalBaselinePythonPins")), set(oracle.BASELINE_PYTHON_PATHS)
                )
                settled = obj(value, "workerSettled")
                self.assertEqual(
                    settled, {"status": 0, "reaped": True, "pipesClosed": True, "stderrBytes": 0}
                )
                for child in objects(value, "children"):
                    self.assertTrue(obj(child)["pipesClosed"] is True)
                    self.assertTrue(obj(child)["logsClosed"] is True)
                    self.assertIsInstance(obj(child)["status"], int)

    def test_success_contains_backup_assets_and_only_one_app_replacement(self) -> None:
        """Full main stages, saves a real dump/receipt, then replaces only the application."""
        value = self.observation("success")
        report = obj(array(value, "reports")[0])
        self.assertTrue(report["passed"] is True)
        self.assertEqual(report["phase"], "complete")
        roles = self.roles("success")
        self.assertLess(roles.index("image-load"), roles.index("db-ledger"))
        self.assertLess(roles.index("restore-list"), roles.index("app-stop"))
        self.assertEqual(roles.count("app-stop"), 1)
        self.assertEqual(roles.count("candidate-up"), 1)
        self.assertNotIn("rollback-up", roles)
        self.assertTrue(value["assetFiles"])
        attempt = obj(objects(value, "attempts")[0])
        self.assertIn("database-before.dump", attempt)
        self.assertIn("database-before.receipt.json", attempt)
        self.assertEqual(report["backupSha256"], obj(attempt, "database-before.dump")["sha256"])
        self.assertTrue(
            all(
                obj(w)["lockHeld"]
                for w in objects(value, "writes")
                if not string(obj(w), "path").endswith("outcome.json")
            )
        )
        self.assertTrue(obj(objects(value, "writes")[-1])["lockHeld"] is False)

    def test_rollback_is_once_and_never_promotes_candidate_failure(self) -> None:
        """Failed candidate returns failure after exactly one bounded old-image rollback."""
        for scenario in (
            "candidate-wrong-image",
            "candidate-up",
            "candidate-loop-unready",
            "candidate-public-unready",
            "database-restarted",
            "proxy-restarted",
        ):
            with self.subTest(scenario=scenario):
                value = self.observation(scenario)
                report = obj(array(value, "reports")[0])
                self.assertTrue(report["passed"] is False)
                self.assertTrue(report["rollbackAttempted"] is True)
                self.assertEqual(self.roles(scenario).count("rollback-up"), 1)
                self.assertEqual(self.roles(scenario).count("rollback-stop"), 1)
                self.assertEqual(obj(value, "daemon")["ups"], 2)

    def test_schema_refusals_happen_before_backup_and_replacement(self) -> None:
        """Exact ledger equality and packaged checks cannot become maintenance admission."""
        for scenario in (
            "ledger-missing",
            "ledger-extra",
            "ledger-checksum",
            "ledger-failed",
            "ledger-duplicate",
            "ledger-empty",
            "packaged-mismatch",
        ):
            with self.subTest(scenario=scenario):
                roles = self.roles(scenario)
                self.assertNotIn("dump", roles)
                self.assertNotIn("app-stop", roles)
                self.assertNotIn("rollback-up", roles)

    def test_backup_failures_keep_prefixes_and_never_stop_application(self) -> None:
        """Failed dump/list/durability retains private bytes without replacement authority."""
        for scenario in (
            "dump-empty",
            "dump-nonzero",
            "dump-partial",
            "list-nonzero",
            "backup-file-sync",
            "backup-rename",
            "backup-directory-sync",
            "backup-receipt-write",
        ):
            with self.subTest(scenario=scenario):
                self.assertNotIn("app-stop", self.roles(scenario))
                value = self.observation(scenario)
                self.assertEqual(obj(value, "daemon")["ups"], 0)
        partial = obj(objects(self.observation("dump-partial"), "attempts")[0])
        self.assertEqual(obj(partial, "database-before.dump.partial")["bytes"], 3)
        self.assertEqual(
            obj(partial, "database-before.dump.partial")["sha256"],
            hashlib.sha256(b"PGD").hexdigest(),
        )

    def test_room_timestamp_precedes_rollback_eligibility(self) -> None:
        """Actual timestamp refusal before replaced=True must neither stop nor roll back."""
        value = self.observation("replacement-observation-time")
        report = obj(array(value, "reports")[0])
        self.assertTrue(report["rollbackAttempted"] is False)
        self.assertNotIn("app-stop", self.roles("replacement-observation-time"))
        self.assertNotIn("rollback-up", self.roles("replacement-observation-time"))
        self.assertEqual(value["configBefore"], value["configAfter"])

    def test_quiet_missing_or_active_metrics_are_observations_not_admission(self) -> None:
        """Unknown and deadline-active metrics continue with their exact report facts."""
        for scenario in (
            "quiet-no-token",
            "quiet-unavailable",
            "quiet-malformed",
            "quiet-active-zero",
            "quiet-active-deadline",
            "rooms-arrive-after-quiet",
        ):
            with self.subTest(scenario=scenario):
                report = obj(array(self.observation(scenario), "reports")[0])
                self.assertTrue(report["passed"] is True)
                self.assertIn("quietWaitSeconds", report)
                self.assertIn("roomsActiveBeforeStop", report)
        report = obj(array(self.observation("quiet-active-deadline"), "reports")[0])
        self.assertEqual(report["roomsActiveAtQuietCheck"], 2)
        report = obj(array(self.observation("rooms-arrive-after-quiet"), "reports")[0])
        self.assertEqual(report["roomsActiveBeforeStop"], 3)

    def test_compose_crlf_normalizes_candidate_but_preserves_raw_rollback_copy(self) -> None:
        """Real read_text normalizes the candidate; shutil keeps original rollback bytes."""
        root = self.root / "compose-crlf"
        native, reference = root / "native", root / "reference"
        initial = (native / "config/compose.public.yml").read_bytes()
        attempts = list((reference / "public/results").glob("release.*"))
        self.assertEqual(len(attempts), 1)
        before = (attempts[0] / "before-compose.public.yml").read_bytes()
        self.assertIn(b"\r\n", initial)
        self.assertNotIn(b"\n", initial.replace(b"\r\n", b""))
        self.assertEqual(
            initial.replace(str(native).encode(), b"$ROOT"),
            before.replace(str(reference).encode(), b"$ROOT"),
        )
        candidate = (attempts[0] / "candidate-compose.yml").read_bytes()
        selected = (reference / "config/compose.public.yml").read_bytes()
        expected = before.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
        new_image = string(obj(self.results["compose-crlf"], "input"), "newImage")
        expected = expected.replace(oracle.OLD_IMAGE.encode(), new_image.encode())
        self.assertEqual(candidate, expected)
        self.assertEqual(selected, candidate)
        self.assertNotIn(b"\r", selected)
        value = self.observation("compose-crlf")
        self.assertEqual(value["exitCodes"], [0])
        self.assertEqual(self.roles("compose-crlf").count("candidate-up"), 1)
        self.assertTrue(value["lockReleased"] is True)

    def test_operational_public_caddyfile_keeps_exact_0644_and_receipt(self) -> None:
        """The nonsecret public Caddyfile retains its initial bytes and operational mode."""
        value = self.observation("caddy-public-0644")
        self.assertEqual(value["exitCodes"], [0])
        self.assertTrue(obj(array(value, "reports")[0])["passed"] is True)
        before, after = obj(value, "configBefore"), obj(value, "configAfter")
        self.assertEqual(before["Caddyfile"], after["Caddyfile"])
        self.assertEqual(obj(before, "Caddyfile")["mode"], 0o644)
        self.assertEqual(
            before["browser-assets-installed.json"], after["browser-assets-installed.json"]
        )

    def test_restore_without_full_input_read_is_a_reference_positive(self) -> None:
        """A successful list subprocess need not acknowledge reading the full supplied FD."""
        calls = objects(obj(self.observation("restore-without-read"), "daemon"), "calls")
        call = next(obj(c) for c in calls if string(obj(c), "role") == "restore-list")
        self.assertEqual(call["status"], 0)
        self.assertEqual(call["inputBytes"], 0)

    def test_bulk_dump_above_inspection_cap_uses_original_binary_sink(self) -> None:
        """Actual Runner streams a multi-megabyte dump and supplies original input to list."""
        value = self.observation("dump-above-inspection")
        dump = obj(obj(objects(value, "attempts")[0]), "database-before.dump")
        self.assertGreater(integer_value(dump["bytes"]), 2 * 1024**2)
        roles = objects(obj(value, "daemon"), "calls")
        listed = next(obj(c) for c in roles if string(obj(c), "role") == "restore-list")
        self.assertEqual(listed["inputBytes"], dump["bytes"])
        self.assertEqual(listed["inputSha256"], dump["sha256"])

    def test_outcome_fault_invalidates_body_success(self) -> None:
        """Post-lock outcome persistence failures cannot emit a successful final stdout."""
        for scenario in ("outcome-write", "outcome-file-sync", "outcome-directory-sync"):
            with self.subTest(scenario=scenario):
                value = self.observation(scenario)
                self.assertEqual(value["exitCodes"], [1])
                self.assertEqual(value["reports"], [])
                self.assertEqual(obj(value, "daemon")["ups"], 1)
                self.assertEqual(obj(value, "releaseState")["phase"], "complete")

    def test_completed_candidate_peer_failure_keeps_original_failure_and_one_rollback(self) -> None:
        """Synthetic completion refusal follows actual raw0/EOF and never becomes success."""
        for scenario in (
            "candidate-up-close",
            "candidate-up-close-rollback-up",
            "candidate-up-late-result",
        ):
            with self.subTest(scenario=scenario):
                value = self.observation(scenario)
                report = objects(value, "reports")[0]
                self.assertTrue(report["rollbackAttempted"] is True)
                self.assertEqual(self.roles(scenario).count("rollback-up"), 1)
                candidate = next(
                    call
                    for call in objects(obj(value, "daemon"), "calls")
                    if call["role"] == "candidate-up"
                )
                self.assertEqual(candidate["status"], 0)
                self.assertEqual(
                    report["failure"],
                    "TimeoutExpired" if scenario.endswith("late-result") else "OSError",
                )
                self.assertTrue(value["faultObserved"] is True)
                self.assertTrue(
                    report["rollbackPassed"] is (scenario != "candidate-up-close-rollback-up")
                )

    def test_normalization_preserves_asset_retention_and_unknown_fields(self) -> None:
        """Only known wall-clock string entropy is normalized; retention numbers stay exact."""
        root = self.root / "success/reference"
        value: JsonObject = {
            "createdAt": 2524608000,
            "expiresAt": 2525212800,
            "unknownAt": 17,
            "completedAt": "2050-01-01T00:00:00Z",
            "startedAt": False,
        }
        expected = {**value, "completedAt": "$UTC"}
        self.assertEqual(oracle.normalized(value, root), expected)

    def test_command_entry_snapshots_bind_publication_before_app_up(self) -> None:
        """Captured replies cannot hide moving config/assets/backup publication after app start."""
        value = self.observation("success")
        commands = objects(value, "commands")
        daemon = objects(obj(value, "daemon"), "calls")
        at = next(i for i, call in enumerate(daemon) if call["role"] == "candidate-up")
        snapshot = obj(commands[at], "stateAtStart")
        selected = obj(snapshot, "selection", "images.json", "value")
        self.assertEqual(selected["revision"], oracle.REVISION)
        self.assertEqual(snapshot["assetVersion"], {"revision": oracle.REVISION})
        self.assertTrue(snapshot["lockHeld"] is True)
        self.assertEqual(
            obj(snapshot, "backup")["sha256"], obj(snapshot, "backupReceipt")["sha256"]
        )
        self.assertEqual(obj(snapshot, "journal")["phase"], "replace_application")
        before = next(i for i, call in enumerate(daemon) if call["role"] == "app-stop")
        old = obj(commands[before], "stateAtStart", "selection", "images.json", "value")
        self.assertEqual(old["revision"], oracle.OLD_REVISION)

    def test_failed_outcome_sync_retains_the_writers_complete_intended_frame(self) -> None:
        """A known full temporary has its own byte intent; unknown prefixes stay raw pins."""
        value = self.observation("outcome-file-sync")
        expected = next(
            item
            for item in objects(value, "atomicIntended")
            if string(item, "path").endswith("outcome.json")
        )
        attempts = objects(value, "attempts")
        retained = [obj(item) for name, item in attempts[0].items() if name.startswith(".release-")]
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0]["sha256"], expected["sha256"])
        self.assertEqual(retained[0]["bytes"], expected["bytes"])
        self.assertTrue(obj(expected, "value")["passed"] is True)
        self.assertEqual(value["reports"], [])

    def test_closed_selection_and_fixture_roots_refuse_foreign_requests(self) -> None:
        """No arbitrary scenario text or nonfresh export location is interpreted."""
        with self.assertRaises(ValueError):
            _ = oracle.run("arbitrary-command")
        with tempfile.TemporaryDirectory(prefix="deploy-nonfresh.") as name:
            root = Path(name).resolve()
            _ = (root / "foreign").write_text("INERT")
            with self.assertRaises(ValueError):
                _ = oracle.run("success", root)


if __name__ == "__main__":
    _ = unittest.main()
