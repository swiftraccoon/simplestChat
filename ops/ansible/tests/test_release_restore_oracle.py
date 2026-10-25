"""Check complete unchanged restore CLI cases without a database, engine or network."""

from __future__ import annotations

import concurrent.futures
import hashlib
import json
import tempfile
import unittest
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, cast, override
from unittest.mock import patch

import release_restore_oracle as oracle
from test_support import ROOT, obj

# isort: split
import release_public as public
import restore_verify as restore
from release_json import decode_json

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

ROLES = [
    "image",
    "prior",
    "create",
    "start",
    "ready",
    "roles",
    "restore",
    "restored-ledger",
    "verify",
    "amcheck",
    "cleanup-select",
    "cleanup-inspect",
    "cleanup-remove",
    "cleanup-confirm",
]


class SourceCensusTests(unittest.TestCase):
    """Keep original and new maintained sources outside the installed controller environment."""

    def test_controller_environment_is_excluded_while_original_and_new_sources_remain(self) -> None:
        """An ignored environment cannot hide adjacent maintained files or a missing original."""
        with tempfile.TemporaryDirectory(prefix="restore-source-census-") as temporary:
            root = Path(temporary)
            maintained = {"ops/ansible/venv.py", "build/new-maintained.py"}
            ignored = "ops/ansible/.venv/lib/provider.py"
            for name in {*oracle.BASELINE_PYTHON_PATHS, *maintained, ignored}:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                _ = path.write_bytes(b"original\n")
            with patch.object(oracle, "ROOT", root):
                before = oracle.python_pins()
                self.assertEqual(set(before), set(oracle.BASELINE_PYTHON_PATHS) | maintained)
                _ = (root / ignored).write_bytes(b"installed dependency changed\n")
                self.assertEqual(oracle.python_pins(), before)
                _ = (root / "ops/ansible/venv.py").write_bytes(b"maintained source changed\n")
                self.assertNotEqual(oracle.python_pins(), before)
                (root / oracle.BASELINE_PYTHON_PATHS[0]).unlink()
                with self.assertRaises(FileNotFoundError):
                    _ = oracle.python_pins()


class WholeRestoreOracleTests(unittest.TestCase):
    """Use separate owned original processes and disjoint files for every finite scenario."""

    results: ClassVar[dict[str, JsonObject]] = {}

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Join every admitted inert original before exposing results or assertions."""
        super().setUpClass()
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as workers:
            tasks = {name: workers.submit(oracle.run, name) for name in oracle.SCENARIOS}
            cls.results = {name: task.result() for name, task in tasks.items()}

    def observed(self, name: str) -> JsonObject:
        """Select only a completed closed scenario observation."""
        return obj(self.results[name], "observation")

    def calls(self, name: str) -> list[JsonObject]:
        """Return the actual lower command journal, never a scripted phase verdict."""
        return [
            obj(value)
            for value in cast("list[JsonValue]", obj(self.observed(name), "daemon")["calls"])
        ]

    def files(self, name: str) -> JsonObject:
        """Keep all physical product leaves, including failed atomic prefixes."""
        return obj(self.observed(name), "files")

    def records(self, name: str, filename: str) -> list[JsonObject]:
        """Read semantic facts only from complete original JSON files at fixed leaf names."""
        return [
            obj(value, "json")
            for path, value in self.files(name).items()
            if path.endswith("/" + filename)
        ]

    def test_all_finite_originals_settle_and_preserve_every_named_source(self) -> None:
        """Every enumerated row executes the whole CLI and real file/child owners."""
        self.assertEqual(len(oracle.SCENARIOS), len(set(oracle.SCENARIOS)))
        for name, result in self.results.items():
            with self.subTest(scenario=name):
                observed = obj(result, "observation")
                codes = cast("list[JsonValue]", observed["exitCodes"])
                self.assertEqual(all(code == 0 for code in codes), name in oracle.POSITIVE)
                self.assertEqual(
                    observed["workerSettled"],
                    {"status": 0, "reaped": True, "pipesClosed": True, "stderrBytes": 0},
                )
                self.assertTrue(observed["lockReleased"])
                self.assertTrue(observed["snapshotOriginalsClosed"])
                self.assertEqual(observed["faultObserved"], name in oracle.FAULT_CASES)
                self.assertTrue(observed["inputUnchanged"])
                self.assertTrue(observed["sourceUnchanged"])
                self.assertTrue(observed["originalPythonUnchanged"])
                self.assertTrue(observed["originalBaselinePythonUnchanged"])
                baseline = obj(observed, "originalBaselinePythonPins")
                self.assertEqual(set(baseline), set(oracle.BASELINE_PYTHON_PATHS))
                self.assertEqual(observed["originalBaselinePythonCount"], len(baseline))
                self.assertGreaterEqual(cast("int", observed["originalPythonCount"]), len(baseline))
                children = cast("list[JsonValue]", observed["children"])
                self.assertEqual(len(children), len(self.calls(name)))
                for child in children:
                    facts = obj(child)
                    self.assertIs(type(facts["status"]), int)
                    self.assertTrue(facts["pipesClosed"])
                    self.assertTrue(facts["logsClosed"])

    def test_success_has_real_restore_cleanup_and_freshness_order_under_lock(self) -> None:
        """The original publishes success only after exact cleanup and snapshot retirement."""
        observed = self.observed("success")
        self.assertEqual(observed["exitCodes"], [0])
        self.assertEqual(observed["stdout"], oracle.SUCCESS)
        self.assertEqual(observed["stderr"], "")
        self.assertEqual([call["role"] for call in self.calls("success")], ROLES)
        writes = [obj(value) for value in cast("list[JsonValue]", observed["writes"])]
        self.assertTrue(all(value["lockHeld"] is True for value in writes))
        self.assertTrue(all(value["completed"] is True for value in writes))
        self.assertTrue(cast("str", writes[-1]["path"]).endswith("/restore-verified.timestamp"))
        self.assertTrue(cast("str", writes[-2]["path"]).endswith("/outcome.json"))
        self.assertFalse(any(path.endswith("snapshot.dump") for path in self.files("success")))
        self.assertEqual(observed["snapshotUnlinkAttempts"], 1)
        self.assertIs(obj(observed, "daemon")["present"], expr2=False)
        record = self.records("success", "restore.json")[0]
        self.assertIs(record["verified"], expr2=True)
        self.assertIs(record["cleanupPassed"], expr2=True)
        self.assertEqual(record["counts"], oracle.COUNTS)

    def test_failed_release_subset_and_original_hardlink_are_reference_positive(self) -> None:
        """Pre-upgrade evidence is bound to its own ledger, independent from release success."""
        for name in (
            "failed-release-backup",
            "subset-ledger",
            "source-hardlink",
            "verify-sql-hardlink",
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["exitCodes"], [0])
                self.assertTrue(self.records(name, "restore.json")[0]["verified"])
        pins = obj(self.observed("source-hardlink"), "inputPinsBefore")
        self.assertEqual(
            obj(next(value for key, value in pins.items() if key.endswith(".dump")))["links"], 2
        )

    def test_readiness_profile_keeps_exact_reference_overshoot_without_real_sleep(self) -> None:
        """A final reference retry at30 may succeed; the reviewed native30 ceiling is separate."""
        for name, elapsed, expected in (
            ("ready-retry", 1.0, 0),
            ("ready-exact-boundary", 30.0, 0),
            ("ready-deadline", 30.0, 1),
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["virtualElapsed"], elapsed)
                self.assertEqual(self.observed(name)["exitCodes"], [expected])
        self.assertEqual(
            obj(self.results["ready-exact-boundary"], "input")["nativeProfile"],
            "strict-readiness30",
        )

    def test_private_sql_is_an_explicit_fixed_native_profile_not_a_reference_refusal(self) -> None:
        """The original reads private0600 SQL; the installed native profile requires0644."""
        self.assertEqual(self.observed("verify-sql-private0600")["exitCodes"], [0])
        self.assertEqual(
            obj(self.results["verify-sql-private0600"], "input")["nativeProfile"],
            "fixed-verifier-sql644",
        )

    def test_real_stdin_bytes_bind_roles_archive_and_verifier_sql(self) -> None:
        """All stdin-bearing daemon children consume actual owned bytes through the real Runner."""
        calls = {cast("str", value["role"]): value for value in self.calls("success")}
        for role, data in (
            ("roles", restore.ROLE_SQL),
            ("restore", oracle.DUMP),
            ("restored-ledger", public.LEDGER_QUERY.encode()),
            ("verify", (ROOT / oracle.REFERENCE_PATHS[1]).read_bytes()),
        ):
            with self.subTest(role=role):
                self.assertEqual(calls[role]["inputBytes"], len(data))
                self.assertEqual(calls[role]["inputSha256"], hashlib.sha256(data).hexdigest())

    def test_unknown_or_timedout_create_uses_exact_generated_ownership_join(self) -> None:
        """Unreturned identities still get the original name/label/image join, never broad rm."""
        for name in ("create-unknown", "create-timeout-after-effect", "create-command"):
            with self.subTest(scenario=name):
                calls = self.calls(name)
                self.assertEqual(self.observed(name)["exitCodes"], [1])
                self.assertIn("cleanup-inspect", [call["role"] for call in calls])
                removed = next(call for call in calls if call["role"] == "cleanup-remove")
                self.assertEqual(cast("list[JsonValue]", removed["arguments"])[-1], oracle.CID)
                self.assertFalse(obj(self.observed(name), "daemon")["present"])

    def test_prior_or_ambiguous_foreign_identity_never_grants_removal(self) -> None:
        """A prior role or failed exact join remains negative and retained for inspection."""
        for name in (
            "prior-container",
            "cleanup-ambiguous",
            "cleanup-id-diff",
            "cleanup-name-diff",
            "cleanup-image-diff",
            "cleanup-owner-diff",
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["exitCodes"], [1])
                self.assertNotIn("cleanup-remove", [call["role"] for call in self.calls(name)])

    def test_snapshot_io_failures_close_originals_and_precede_daemon_effects(self) -> None:
        """No first read/write/flush/sync refusal reaches actual PostgreSQL creation."""
        for name in (
            "snapshot-read",
            "snapshot-write",
            "snapshot-flush",
            "snapshot-fsync",
            "backup-empty",
            "backup-oversized",
            "backup-digest",
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["exitCodes"], [1])
                self.assertEqual(self.calls(name), [])
                self.assertFalse(any(path.endswith("snapshot.dump") for path in self.files(name)))

    def test_body_failure_keeps_real_cleanup_and_previous_freshness(self) -> None:
        """Restore/ledger/schema/amcheck failures remove their owned database without success."""
        previous = hashlib.sha256(b"UNCHANGED_PREVIOUS_FRESHNESS\n").hexdigest()
        for name in (
            "restore-command",
            "restored-ledger-diff",
            "verify-command",
            "verify-invalid-json",
            "amcheck-command",
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["exitCodes"], [1])
                record = self.records(name, "restore.json")[0]
                self.assertFalse(record["verified"])
                self.assertTrue(record["cleanupPassed"])
                self.assertEqual(
                    obj(self.files(name)["state/restore-verified.timestamp"])["sha256"], previous
                )

    def test_final_atomic_faults_follow_actual_full_body_and_earlier_records(self) -> None:
        """Every final fault must target its declared write, after all actual lower effects."""
        for prefix in ("restore-record-final", "outcome", "freshness"):
            for boundary in ("write", "file-sync", "directory-sync"):
                name = prefix + "-" + boundary
                with self.subTest(scenario=name):
                    observed = self.observed(name)
                    self.assertEqual([call["role"] for call in self.calls(name)], ROLES)
                    writes = [obj(value) for value in cast("list[JsonValue]", observed["writes"])]
                    restored = [
                        row for row in writes if cast("str", row["path"]).endswith("/restore.json")
                    ]
                    self.assertEqual(len(restored), 3)
                    self.assertEqual(
                        [row["completed"] for row in restored],
                        [True, True, prefix != "restore-record-final"],
                    )
                    self.assertEqual(obj(restored[0], "data")["containerId"], "")
                    self.assertEqual(obj(restored[1], "data")["containerId"], oracle.CID)
                    self.assertIs(obj(restored[2], "data")["verified"], expr2=True)
                    self.assertIs(obj(restored[2], "data")["cleanupPassed"], expr2=True)
                    outcomes = [
                        row for row in writes if cast("str", row["path"]).endswith("/outcome.json")
                    ]
                    fresh = [
                        row
                        for row in writes
                        if cast("str", row["path"]).endswith("/restore-verified.timestamp")
                    ]
                    self.assertEqual(len(outcomes), int(prefix != "restore-record-final"))
                    self.assertEqual(len(fresh), int(prefix == "freshness"))
                    if outcomes:
                        self.assertIs(outcomes[0]["completed"], prefix == "freshness")
                    if fresh:
                        self.assertIs(fresh[0]["completed"], expr2=False)
                    self.assertEqual(observed["faultEvents"], [name])
                    self.assertEqual(observed["snapshotUnlinkAttempts"], 1)

    def test_successful_body_cannot_advance_freshness_after_final_local_faults(self) -> None:
        """Snapshot removal and atomic final records retain exact failed prefixes/verdicts."""
        for name in (
            "snapshot-unlink",
            "outcome-write",
            "outcome-file-sync",
            "outcome-directory-sync",
            "freshness-write",
            "freshness-file-sync",
            "freshness-directory-sync",
            "restore-record-final-write",
            "restore-record-final-file-sync",
            "restore-record-final-directory-sync",
        ):
            with self.subTest(scenario=name):
                self.assertEqual(self.observed(name)["exitCodes"], [1])
                self.assertEqual(self.observed(name)["stdout"], "")
                self.assertEqual(self.observed(name)["stderr"], oracle.FAILURE)
        self.assertTrue(any("$ATOMIC-" in path for path in self.files("outcome-file-sync")))
        self.assertTrue(self.records("outcome-directory-sync", "outcome.json"))
        self.assertTrue(self.records("freshness-directory-sync", "restore-verified.timestamp"))
        self.assertEqual(self.observed("snapshot-unlink")["snapshotUnlinkAttempts"], 1)

    def test_observed_original_generated_ids_have_actual_uuid_four_metadata(self) -> None:
        """The fixed uuid4 observation must retain its actual version and variant semantics."""
        for scenario in ("success", "repeat"):
            for call in self.calls(scenario):
                if call["role"] != "cleanup-inspect":
                    continue
                raw = bytes.fromhex(cast("str", call["stdoutHex"]))
                record = obj(decode_json(raw))
                name = cast("str", record["owner"])
                self.assertTrue(name.startswith("scpub-restore-"))
                identifier = uuid.UUID(hex=name.removeprefix("scpub-restore-"))
                self.assertEqual(identifier.version, 4)
                self.assertEqual(identifier.variant, uuid.RFC_4122)

    def test_raw_inspection_and_original_log_pins_match_each_fixed_uuid(self) -> None:
        """Keep raw bytes and original log pins exact across both independent generations."""
        for scenario, count in (("success", 1), ("repeat", 2)):
            generation = 0
            ordinal = 0
            inspections = 0
            for call in self.calls(scenario):
                if call["role"] == "image":
                    generation += 1
                    ordinal = 0
                ordinal += 1
                if call["role"] != "cleanup-inspect":
                    continue
                name = (
                    "scpub-restore-"
                    + uuid.UUID(bytes=b"\xdd" * 15 + bytes([0xDD + generation - 1]), version=4).hex
                )
                expected = (
                    json.dumps(
                        {
                            "id": oracle.CID,
                            "name": "/" + name,
                            "image": oracle.IMAGE,
                            "owner": name,
                        },
                        separators=(",", ":"),
                    )
                    + "\n"
                ).encode()
                raw = bytes.fromhex(cast("str", call["stdoutHex"]))
                self.assertEqual(raw, expected)
                leaf = f"state/restores/$RESTORE-{generation:03}/{ordinal:03}.stdout"
                self.assertEqual(
                    obj(self.files(scenario), leaf),
                    {
                        "bytes": len(raw),
                        "sha256": hashlib.sha256(raw).hexdigest(),
                        "mode": 0o600,
                        "links": 1,
                    },
                )
                inspections += 1
            self.assertEqual(generation, count)
            self.assertEqual(inspections, count)

    def test_repeat_keeps_both_attempts_instead_of_collapsing_generated_names(self) -> None:
        """Both completed original generations and all28 actual child owners remain visible."""
        observed = self.observed("repeat")
        self.assertEqual(observed["exitCodes"], [0, 0])
        self.assertEqual(len(cast("list[JsonValue]", observed["children"])), 2 * len(ROLES))
        names = {
            path.split("/")[2]
            for path in self.files("repeat")
            if path.startswith("state/restores/")
        }
        self.assertEqual(names, {"$RESTORE-001", "$RESTORE-002"})


if __name__ == "__main__":
    _ = unittest.main()
