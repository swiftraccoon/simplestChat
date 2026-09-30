"""Offline PostgreSQL logging remediation guards; never contacts a database or daemon."""

from __future__ import annotations

import io
import json
import os
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Unpack, final
from unittest.mock import patch

from test_support import ROOT, yaml_value

# isort: split

import database_logging as logging
import release_public as release
from release_json import JsonObject, JsonValue, decode_json, object_value

if TYPE_CHECKING:
    from release_public import CommandOptions

DATABASE = "a" * 64


def snapshot(*, safe: bool = False, source: str | None = None) -> JsonObject:
    """Model the exact fixed catalog projection, never application data."""
    return {
        name: {
            "name": name,
            "setting": "0" if safe or name.endswith("on_error") else "-1",
            "source": source or ("configuration file" if safe else "default"),
            "context": context,
            "pending_restart": False,
        }
        for name, context in logging.SETTINGS.items()
    }


@final
@dataclass
class DatabaseFixture:
    """Capture exact IDs, per-session SQL and identity checks without external effects."""

    snapshots: list[JsonObject] = field(default_factory=lambda: [snapshot(), snapshot(safe=True)])
    statements: list[str] = field(default_factory=list)
    commands: list[tuple[tuple[str, ...], CommandOptions]] = field(default_factory=list)
    inspections: int = 0
    replacement_at: int | None = None
    reload_result: bytes = b"t\n"
    fail_statement: str | None = None

    def container(self, service: str) -> JsonObject:
        """Expose only the current public database and a controlled replacement race."""
        if service != "postgres":
            raise AssertionError(service)
        self.inspections += 1
        return {
            "id": "b" * 64 if self.inspections == self.replacement_at else DATABASE,
            "image": "sha256:" + "c" * 64,
            "state": {"StartedAt": "fixture-start"},
            "restarts": 0,
        }

    def docker(self, *args: str, **kwargs: Unpack[CommandOptions]) -> bytes:
        """Each call models a fresh psql session with a single fixed statement."""
        self.commands.append((args, kwargs))
        statement = args[-1]
        self.statements.append(statement)
        if statement == self.fail_statement:
            raise release.ReleaseError("Fixture command failed")  # noqa: EM101, TRY003 -- fixed fixture.
        if statement == logging.QUERY:
            value = self.snapshots.pop(0) if len(self.snapshots) > 1 else self.snapshots[0]
            return json.dumps(list(value.values())).encode()
        if statement == logging.RELOAD:
            return self.reload_result
        if statement in logging.ALTER:
            return b"ALTER SYSTEM\n"
        raise AssertionError(statement)


class DatabaseLoggingTests(unittest.TestCase):
    """Only safe reloadable policy changes may produce a successful private receipt."""

    def test_separate_alters_reload_and_fresh_sessions_verify_both_limits(self) -> None:
        """A successful reload signal alone is insufficient until new sessions see the policy."""
        runner = DatabaseFixture(snapshots=[snapshot(), snapshot(), snapshot(safe=True)])
        report: JsonObject = {}
        with patch.object(time, "sleep"):
            logging.apply(runner, report)
        self.assertEqual(
            runner.statements,
            [logging.QUERY, *logging.ALTER, logging.RELOAD, logging.QUERY, logging.QUERY],
        )
        self.assertTrue(report["changed"])
        self.assertEqual(report["after"], snapshot(safe=True))
        for argv, options in runner.commands:
            self.assertEqual(argv[0], "exec")
            self.assertIn(DATABASE, argv)
            self.assertIn("--no-psqlrc", argv)
            self.assertIn("--no-password", argv)
            self.assertIn("PGOPTIONS=-c statement_timeout=5000 -c lock_timeout=3000", argv)
            self.assertEqual(argv.count("--command"), 1)
            self.assertEqual(options.get("timeout"), 10)

    def test_explicit_safe_configuration_is_idempotent(self) -> None:
        """Safe file or command-line settings require no ALTER or reload."""
        for source in ("configuration file", "command line"):
            with self.subTest(source=source):
                runner = DatabaseFixture(snapshots=[snapshot(safe=True, source=source)])
                report: JsonObject = {}
                logging.apply(runner, report)
                self.assertEqual(runner.statements, [logging.QUERY])
                self.assertFalse(report["changed"])

    def test_overrides_pending_restart_and_unexpected_context_fail_before_mutation(self) -> None:
        """Do not claim ALTER SYSTEM can override stronger policy or avoid a required restart."""
        for column, value in (
            ("source", "command line"),
            ("source", "database"),
            ("source", "user"),
            ("source", "session"),
            ("pending_restart", True),
            ("context", "postmaster"),
        ):
            with self.subTest(column=column, value=value):
                before = snapshot()
                object_value(before["log_parameter_max_length"])[column] = value
                runner = DatabaseFixture(snapshots=[before])
                with self.assertRaises(release.ReleaseError):
                    logging.apply(runner, {})
                self.assertEqual(runner.statements, [logging.QUERY])

    def test_missing_duplicate_unknown_and_invalid_catalog_values_are_rejected(self) -> None:
        """The complete two-row schema is mandatory even when the values appear safe."""
        rows = list(snapshot(safe=True).values())
        bad: list[JsonValue] = [[], rows[:1], [rows[0], rows[0]], {"items": rows}]
        for column, value in (("name", "other"), ("setting", "invalid"), ("pending_restart", "f")):
            changed = object_value(rows[0]).copy()
            changed[column] = value
            bad.append([changed, rows[1]])
        for value in bad:
            with self.subTest(value=value), self.assertRaises((release.ReleaseError, ValueError)):
                _ = logging.settings(json.dumps(value).encode())

    def test_full_id_and_fixed_sql_are_required(self) -> None:
        """No caller-supplied host, partial ID, profile query or arbitrary SQL is accepted."""
        runner = DatabaseFixture()
        for identity, statement in (("a" * 12, logging.QUERY), (DATABASE, "SELECT 1")):
            with self.subTest(identity=identity), self.assertRaises(release.ReleaseError):
                _ = logging.query(runner, identity, statement)
        self.assertEqual(runner.commands, [])

    def test_identity_changes_before_mutation_or_after_reload_are_failures(self) -> None:
        """A replacement cannot inherit the original container's successful check."""
        before_mutation = 2
        for replacement in (before_mutation, 5):
            runner = DatabaseFixture(replacement_at=replacement)
            with (
                self.subTest(replacement=replacement),
                self.assertRaisesRegex(release.ReleaseError, "container changed"),
            ):
                logging.apply(runner, {})
            if replacement == before_mutation:
                self.assertEqual(runner.statements, [logging.QUERY])

    def test_reload_refusal_and_stale_effective_values_never_pass(self) -> None:
        """Verification has a finite deadline and does not accept defaults as persisted policy."""
        with self.assertRaisesRegex(release.ReleaseError, "reload was refused"):
            logging.apply(DatabaseFixture(reload_result=b"f\n"), {})
        runner = DatabaseFixture(snapshots=[snapshot(), snapshot(safe=True, source="default")])
        with (
            patch.object(time, "monotonic", side_effect=[0, 16]),
            self.assertRaisesRegex(release.ReleaseError, "reload was not effective"),
        ):
            logging.apply(runner, {})

    def test_locked_execution_retains_private_success_and_partial_failure_evidence(self) -> None:
        """The real shared lock and private filesystem retain an outcome even after partial SQL."""
        previous_umask = os.umask(0o077)
        self.addCleanup(os.umask, previous_umask)
        for failure in (None, logging.ALTER[1]):
            with tempfile.TemporaryDirectory() as directory, self.subTest(failure=failure):
                root = Path(directory)
                config, results = root / "config", root / "results"
                config.mkdir(mode=0o700)
                results.mkdir(mode=0o700)
                runner = DatabaseFixture(fail_statement=failure)
                with (
                    patch.object(release, "ROOT", root),
                    patch.object(release, "CONFIG", config),
                    patch.object(release, "WORK", root / "work"),
                    patch.object(release, "ROOT_UID", os.getuid()),
                    patch.object(os, "geteuid", return_value=0),
                    patch.object(release, "Runner", return_value=runner),
                    patch.object(release, "workload_lock", wraps=release.workload_lock) as lock,
                    redirect_stdout(io.StringIO()),
                ):
                    if failure is None:
                        self.assertTrue(logging.execute()["passed"])
                    else:
                        with self.assertRaises(release.ReleaseError):
                            _ = logging.execute()
                    lock.assert_called_once_with()
                attempts = list(results.iterdir())
                self.assertEqual(len(attempts), 1)
                outcome = attempts[0] / "outcome.json"
                self.assertEqual(attempts[0].stat().st_mode & 0o777, 0o700)
                self.assertEqual(outcome.stat().st_mode & 0o777, 0o600)
                saved = object_value(decode_json(outcome.read_bytes()))
                self.assertEqual(saved["passed"], failure is None)
                self.assertTrue(saved["changed"])
                provenance = object_value(saved["sourceSha256"])
                self.assertEqual(
                    set(provenance),
                    {"database_logging.py", "release_public.py", "bounded_process.py"},
                )
                for digest in provenance.values():
                    self.assertIsInstance(digest, str)
                    self.assertRegex(str(digest), r"^[a-f0-9]{64}$")

    def test_playbook_installs_shared_dependencies_and_runs_only_fixed_apply(self) -> None:
        """The maintained replay command cannot take SQL or restart any service."""
        text = (ROOT / "ops/ansible/database-logging.yml").read_text()
        self.assertIsInstance(yaml_value(text), list)
        for expected in (
            "scpub_enabled | bool",
            "bounded_process.py",
            "release_public.py",
            "database_logging.py",
            "when: not ansible_check_mode",
            "- apply",
        ):
            self.assertIn(expected, text)
        self.assertNotIn("systemd_service", text)
        self.assertNotIn("restart:", text)


if __name__ == "__main__":
    _ = unittest.main()
