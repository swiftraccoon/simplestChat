"""Exercise incident lifecycle SQL only against an explicitly owned disposable database."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split
import monitoring_alerts as alerts
from monitoring_alerts import incident
from release_json import JsonObject, array_value, decode_json, object_value


@unittest.skipUnless(os.environ.get("DISPOSABLE_TEST_DATABASE") == "1", "requires owned PostgreSQL")
class IncidentDatabaseTests(unittest.TestCase):
    """Verify replay, resolve/reopen and private schema boundaries using real PostgreSQL."""

    def sql(self, source: str, payload: JsonObject | None = None) -> str:
        """Send fixed SQL and a psql-quoted JSON value to the isolated test database."""
        executable = shutil.which("psql")
        self.assertIsNotNone(executable)
        if executable is None:
            self.fail("psql is required")
        command = [
            executable,
            "-X",
            "-q",
            "-t",
            "-A",
            os.environ["TEST_DATABASE_URL"],
            "--set",
            "ON_ERROR_STOP=1",
        ]
        if payload is not None:
            command += ["--set", "payload=" + json.dumps(payload)]
        return subprocess.run(  # noqa: S603 - explicitly marked disposable database and reviewed fixed SQL.
            command,
            input=source,
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        ).stdout.strip()

    def test_ambiguous_committed_response_retains_database_outage_after_recovery(self) -> None:
        """A timeout after COMMIT must still produce and then resolve a separate DB incident."""
        _ = self.sql((ROOT / "ops/ansible/files/monitoring-schema.sql").read_text())
        _ = self.sql("TRUNCATE operations.alerts, operations.alert_cursor;")
        executable = shutil.which("psql")
        if executable is None:
            self.fail("psql is required")
        run = subprocess.run
        attempts = 0

        def commit_then_timeout(
            arguments: list[str], **_options: object
        ) -> subprocess.CompletedProcess[bytes]:
            nonlocal attempts
            payload = next(item for item in arguments if item.startswith("payload="))
            result = run(
                [executable, "-X", "-q", os.environ["TEST_DATABASE_URL"], "--set", payload],
                input=(ROOT / "ops/ansible/files/monitoring-alerts.sql").read_bytes(),
                capture_output=True,
                check=True,
                timeout=10,
            )
            attempts += 1
            if attempts == 1:
                failed_command = "committed fixture"
                raise subprocess.TimeoutExpired(failed_command, 1)
            return result

        def healthy_snapshot(_revision: str | None) -> JsonObject:
            return {
                "observedAt": datetime.now(UTC).isoformat(timespec="microseconds"),
                "revision": None,
                "complete": True,
                "alerts": [],
            }

        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(alerts, "STATE", Path(directory)),
            patch.object(alerts, "ALERT_SQL", ROOT / "ops/ansible/files/monitoring-alerts.sql"),
            patch.object(alerts, "snapshot", side_effect=healthy_snapshot),
            patch.object(subprocess, "run", side_effect=commit_then_timeout),
        ):
            self.assertFalse(alerts.record(None)[0])
            self.assertTrue(alerts.record(None)[0])
        self.assertEqual(
            self.sql(
                "SELECT rule, count(*), count(resolved_at) FROM operations.alerts GROUP BY rule;"
            ),
            "DatabaseUnavailable|1|1",
        )
        _ = self.sql("TRUNCATE operations.alerts, operations.alert_cursor;")

    def test_external_history_partial_recovery_reruns_privacy_and_retention(self) -> None:
        """Real SQL retains skipped-check gaps and cannot turn incomplete polling into recovery."""
        _ = self.sql((ROOT / "ops/ansible/files/monitoring-schema.sql").read_text())
        _ = self.sql("""DO $$ BEGIN
            IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN
                CREATE ROLE postgres NOLOGIN;
            END IF;
            IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
                CREATE ROLE simplestchat_app NOLOGIN;
                CREATE ROLE simplestchat_migrate NOLOGIN;
            END IF;
            END $$;
            ALTER SCHEMA operations OWNER TO postgres;
            ALTER TABLE operations.schema_version OWNER TO postgres;""")
        _ = self.sql(
            "SET ROLE postgres;\n"
            + (ROOT / "ops/ansible/files/monitoring-external-schema.sql").read_text()
        )
        record = (ROOT / "ops/ansible/files/monitoring-external.sql").read_text()
        now = datetime.now(UTC)

        def attempt(identity: int, number: int, result: str, seconds: int) -> JsonObject:
            return {
                "runId": identity,
                "attempt": number,
                "sourceRevision": "a" * 40,
                "startedAt": (now + timedelta(seconds=seconds - 30)).isoformat(),
                "completedAt": (now + timedelta(seconds=seconds)).isoformat(),
                "result": result,
                "conclusion": "success" if result != "failure" else "failure",
                "checks": {"readiness": "success" if result == "success" else "missing"},
            }

        def snapshot(seconds: int, entries: list[JsonObject], *, complete: bool) -> JsonObject:
            return {
                "observedAt": (now + timedelta(seconds=seconds)).isoformat(),
                "windowStart": (now - timedelta(days=1)).isoformat(),
                "revision": "b" * 40,
                "workflows": [
                    {
                        "workflow": "availability",
                        "apiOk": True,
                        "complete": complete,
                        "pending": 0 if complete else 1,
                        "truncated": False,
                        "runs": list(entries),
                    }
                ],
            }

        failure = attempt(1, 1, "failure", -180)
        skipped = attempt(2, 1, "incomplete", -120)
        first = snapshot(0, [failure, skipped], complete=True)
        _ = self.sql(record, first)
        _ = self.sql(record, first)
        self.assertEqual(self.sql("SELECT count(*) FROM operations.external_runs;"), "2")
        self.assertEqual(
            self.sql("""SELECT failure_since IS NOT NULL
            FROM operations.external_status WHERE workflow='availability';"""),
            "t",
        )
        report = object_value(
            decode_json(self.sql((ROOT / "ops/ansible/files/monitoring-report.sql").read_text()))
        )
        coverage = [object_value(item) for item in array_value(report["externalCoverage"])]
        self.assertEqual(coverage[0]["coverage"], "incomplete")
        self.assertEqual(coverage[1]["coverage"], "missing")
        self.assertNotEqual(
            coverage[0]["source_revision"], coverage[0]["deployed_revision_at_import"]
        )
        # A successful rerun is distinct evidence, but partial polling cannot clear an incident.
        _ = self.sql(record, snapshot(60, [attempt(1, 2, "success", -60)], complete=False))
        _ = self.sql(record, first)  # stale complete replay cannot override the partial cursor.
        self.assertEqual(
            self.sql("""SELECT failure_since IS NOT NULL, complete
            FROM operations.external_status WHERE workflow='availability';"""),
            "t|f",
        )
        self.assertEqual(self.sql("SELECT count(*) FROM operations.external_runs;"), "3")
        _ = self.sql(record, snapshot(120, [], complete=True))
        self.assertEqual(
            self.sql("""SELECT failure_since IS NULL, complete
            FROM operations.external_status WHERE workflow='availability';"""),
            "t|t",
        )
        self.assertEqual(
            self.sql("""SELECT bool_and(NOT has_schema_privilege(
            r.rolname,'operations','USAGE,CREATE') AND NOT has_table_privilege(
            r.rolname,'operations.external_runs','SELECT,INSERT,UPDATE,DELETE,TRUNCATE'))
            FROM pg_roles r WHERE r.rolname IN ('simplestchat_app','simplestchat_migrate');"""),
            "t",
        )
        _ = self.sql("""INSERT INTO operations.external_runs
            SELECT 'availability',100+n,1,repeat('a',40),repeat('b',40),now(),now(),
                'success','success','{}',now() FROM generate_series(1,10001) n;
            UPDATE operations.external_runs SET started_at=now()-interval '41 days',
                completed_at=now()-interval '40 days' WHERE run_id=1;""")
        _ = self.sql(record, snapshot(180, [], complete=True))
        self.assertEqual(self.sql("SELECT count(*) FROM operations.external_runs;"), "10000")
        self.assertEqual(
            self.sql("SELECT count(*) FROM operations.external_runs WHERE run_id=1;"), "0"
        )
        # A gap longer than the bounded backfill must remain visible after later healthy polling.
        gap = snapshot(200, [], complete=True)
        gap["windowStart"] = (now + timedelta(seconds=190)).isoformat()
        _ = self.sql(record, gap)
        self.assertEqual(
            self.sql("""SELECT history_gap_since IS NOT NULL
            FROM operations.external_status WHERE workflow='availability';"""),
            "t",
        )

    def test_lifecycle_replay_privileges_and_retention(self) -> None:
        """Committed retries are idempotent and resolved incidents retain their own history."""
        _ = self.sql((ROOT / "ops/ansible/files/monitoring-schema.sql").read_text())
        record = (ROOT / "ops/ansible/files/monitoring-alerts.sql").read_text()
        alert = incident("DatabaseUnavailable", "critical", {})
        value: JsonObject = {
            "observedAt": "2026-09-23T10:00:00Z",
            "revision": "a" * 40,
            "complete": True,
            "alerts": [alert],
        }
        _ = self.sql(record, value)
        _ = self.sql(record, value)
        self.assertEqual(
            self.sql("SELECT count(*),sum(observations) FROM operations.alerts;"), "1|1"
        )
        value["observedAt"] = "2026-09-23T10:01:00Z"
        value["alerts"] = []
        value["complete"] = False
        _ = self.sql(record, value)
        self.assertEqual(
            self.sql("SELECT count(*) FROM operations.alerts WHERE resolved_at IS NULL;"), "1"
        )
        value["observedAt"] = "2026-09-23T10:02:00Z"
        value["complete"] = True
        _ = self.sql(record, value)
        self.assertEqual(
            self.sql("SELECT count(*) FROM operations.alerts WHERE resolved_at IS NULL;"), "0"
        )
        value["observedAt"] = "2026-09-23T10:03:00Z"
        value["alerts"] = [alert]
        _ = self.sql(record, value)
        self.assertEqual(
            self.sql("SELECT count(*),count(resolved_at) FROM operations.alerts;"), "2|1"
        )
        # PUBLIC must not gain schema access when a new unprivileged role appears.
        _ = self.sql("CREATE ROLE operations_fixture; SET ROLE operations_fixture;")
        self.assertEqual(
            self.sql("SELECT has_schema_privilege('operations_fixture','operations','USAGE');"), "f"
        )
        _ = self.sql(
            """UPDATE operations.alerts SET first_seen=now()-interval '40 days',
            last_seen=now()-interval '40 days', resolved_at=now()-interval '40 days'
            WHERE resolved_at IS NOT NULL;"""
        )
        value["observedAt"] = "2026-09-23T10:04:00Z"
        _ = self.sql(record, value)
        self.assertEqual(self.sql("SELECT count(*) FROM operations.alerts;"), "1")
