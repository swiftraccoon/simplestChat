"""Validate external evidence boundaries without contacting GitHub or a deployment."""

import io
import json
import subprocess
import tempfile
import unittest
from email.message import Message
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import Mock, patch
from urllib.error import HTTPError

from test_support import ROOT, array, obj, objects, yaml_value

# isort: split
import monitoring_external as external
import monitoring_prepare as prepare
import monitoring_report as report
from release_json import JsonObject, array_value, decode_json, object_value, string_value
from release_public import ReleaseError

if TYPE_CHECKING:
    from urllib.request import Request


def fixture_run(workflow: str = "canary", identity: int = 1) -> JsonObject:
    """Construct only a normalized fixed-repository run identity."""
    return {
        "runId": identity,
        "attempt": 1,
        "workflow": workflow,
        "sourceRevision": "a" * 40,
        "createdAt": "2026-09-23T01:00:00Z",
        "updatedAt": "2026-09-23T01:01:00Z",
    }


def fixture_jobs(workflow: str = "canary", identity: int = 1) -> JsonObject:
    """Return a completed fixture with the exact reviewed workflow checks."""
    name, expected = external.WORKFLOWS[workflow]
    return {
        "total_count": 1,
        "jobs": [
            {
                "name": name,
                "run_id": identity,
                "head_sha": "a" * 40,
                "status": "completed",
                "conclusion": "success",
                "started_at": "2026-09-23T01:00:01Z",
                "completed_at": "2026-09-23T01:01:00Z",
                "steps": [
                    {"name": step, "status": "completed", "conclusion": "success"}
                    for step in expected.values()
                ],
            }
        ],
    }


class ExternalMonitoringTests(unittest.TestCase):
    """Keep import completeness, request budgets and durable replay independently honest."""

    def test_only_real_required_steps_can_pass(self) -> None:
        """A green outer workflow cannot hide absent, skipped or failing media checks."""
        github = external.Github({})
        for outcome, expected in (
            ("success", "success"),
            ("skipped", "incomplete"),
            ("failure", "failure"),
        ):
            jobs = fixture_jobs()
            job = object_value(array_value(jobs["jobs"])[0])
            object_value(array_value(job["steps"])[1])["conclusion"] = outcome
            with self.subTest(outcome=outcome), patch.object(github, "get", return_value=jobs):
                self.assertEqual(external.classify(github, fixture_run(), 1)["result"], expected)
        empty: JsonObject = {"total_count": 0, "jobs": []}
        for jobs in (empty, fixture_jobs()):
            if jobs["jobs"]:
                object_value(array_value(jobs["jobs"])[0])["steps"] = []
            with patch.object(github, "get", return_value=jobs):
                self.assertEqual(
                    external.classify(github, fixture_run(), 1)["result"], "incomplete"
                )

    def test_expected_workflow_jobs_and_steps_match_repository(self) -> None:
        """A future workflow rename must update the importer contract in the same change."""
        for workflow, (expected_name, expected_steps) in external.WORKFLOWS.items():
            config = yaml_value(
                (ROOT / f".github/workflows/{workflow}.yml").read_text(), scalars_as_strings=True
            )
            jobs = obj(obj(config)["jobs"])
            self.assertEqual(len(jobs), 1)
            job = obj(next(iter(jobs.values())))
            self.assertEqual(job["name"], expected_name)
            names = [step.get("name") for step in objects(job, "steps")]
            for name in expected_steps.values():
                self.assertEqual(names.count(name), 1)

    def test_empty_listing_and_cadence_guard_never_become_success(self) -> None:
        """Missing runs stay incomplete and repeated starts cannot spend another API budget."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(external, "STATE", Path(directory)),
            patch.object(
                external.Github, "get", return_value={"total_count": 0, "workflow_runs": []}
            ),
        ):
            value = external.collect({}, "a" * 40)
            self.assertTrue(
                all(obj(batch)["complete"] is False for batch in array(value, "workflows"))
            )
            _ = (Path(directory) / "external.json").write_text('{"lastPollAt":10000}')
            with (
                patch("os.geteuid", return_value=0),
                patch("os.umask"),
                patch("time.time", return_value=10001),
                patch.object(external, "collect") as collect,
            ):
                self.assertEqual(external.main(), 0)
                collect.assert_not_called()

    def test_job_identity_and_truncated_job_listing_fail_closed(self) -> None:
        """Evidence cannot come from another revision or an incomplete API page."""
        for field, value in (("head_sha", "b" * 40), ("run_id", 2), ("run_attempt", 2)):
            jobs = fixture_jobs()
            object_value(array_value(jobs["jobs"])[0])[field] = value
            github = external.Github({})
            with (
                self.subTest(field=field),
                patch.object(github, "get", return_value=jobs),
                self.assertRaises(ReleaseError),
            ):
                _ = external.classify(github, fixture_run(), 1)
        with (
            patch.object(github, "get", return_value={"total_count": 2, "jobs": []}),
            self.assertRaises(ReleaseError),
        ):
            _ = external.classify(github, fixture_run(), 1)

    def test_fixed_origin_budget_response_limit_and_rate_backoff(self) -> None:
        """No token, redirect or unbounded response is needed for public evidence."""
        read = Mock(return_value=b"{}")
        response = Mock(status=200, read=read)
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        opened = Mock(return_value=response)
        opener = Mock(open=opened)
        github = external.Github({})
        with patch.object(external, "build_opener", return_value=opener):
            for _ in range(external.MAX_REQUESTS):
                self.assertEqual(github.get("runs/1/attempts/1/jobs?per_page=100"), {})
            with self.assertRaises(ReleaseError):
                _ = github.get("runs/1/attempts/1/jobs?per_page=100")
        self.assertEqual(opened.call_count, external.MAX_REQUESTS)
        request = cast("Request", opened.call_args.args[0])
        self.assertEqual(request.host, "api.github.com")
        self.assertNotIn("Authorization", request.headers)
        read.assert_called_with(external.MAX_RESPONSE + 1)
        with self.assertRaises(ReleaseError):
            _ = external.Github({}).get("https://elsewhere.example/runs")
        self.assertIsNone(external.NoRedirect().redirect_request())
        headers = Message()
        headers["Retry-After"] = "999999"
        state: JsonObject = {}
        limited = Mock(open=Mock(side_effect=HTTPError("fixed", 429, "limited", headers, None)))
        with (
            patch.object(external, "build_opener", return_value=limited),
            patch("time.time", return_value=10000),
            self.assertRaises(HTTPError),
        ):
            _ = external.Github(state).get("runs/1/attempts/1/jobs?per_page=100")
        self.assertEqual(state["notBefore"], 13600)

    def test_attempts_budget_completeness_and_backward_clock(self) -> None:
        """A crash before state persistence and a clock correction cannot reorder snapshots."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(external, "STATE", Path(directory)),
        ):
            _ = external.spool({"observedAt": "2030-01-01T00:00:00+00:00"})
            state: JsonObject = {}
            calls = 0

            def fetched(github: external.Github, path: str) -> JsonObject:
                nonlocal calls
                github.requests += 1
                calls += 1
                if path.startswith("workflows/"):
                    workflow = "availability" if "availability.yml" in path else "canary"
                    run = fixture_run(workflow)
                    return {
                        "total_count": 1,
                        "workflow_runs": [
                            {
                                "id": 1 if workflow == "availability" else 2,
                                "run_attempt": 10,
                                "head_sha": run["sourceRevision"],
                                "created_at": run["createdAt"],
                                "updated_at": run["updatedAt"],
                                "head_branch": "main",
                                "event": "schedule",
                                "status": "completed",
                                "path": f".github/workflows/{workflow}.yml",
                                "repository": {"full_name": external.REPOSITORY},
                                "head_repository": {"full_name": external.REPOSITORY},
                            }
                        ],
                    }
                identity = int(path.split("/")[1])
                return fixture_jobs("availability" if identity == 1 else "canary", identity)

            with patch.object(external.Github, "get", new=fetched):
                value = external.collect(state, "b" * 40)
            self.assertEqual(calls, external.MAX_REQUESTS)
            self.assertGreater(string_value(value["observedAt"]), "2030-01-01T00:00:00+00:00")
            batches = [object_value(item) for item in array_value(value["workflows"])]
            self.assertTrue(all(batch["complete"] is False for batch in batches))
            self.assertTrue(all(array_value(batch["runs"]) for batch in batches))
            self.assertEqual(sum(int(str(batch["pending"])) for batch in batches), 14)

    def test_spool_is_bounded_and_failed_replay_keeps_exact_bytes(self) -> None:
        """A database outage cannot grow disk forever or discard the failed commit candidate."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(external, "STATE", Path(directory)),
            patch.object(external, "SQL_ROOT", ROOT / "ops/ansible/files"),
            patch.object(external, "MAX_SPOOL", 3),
            patch.object(external, "database", side_effect=subprocess.TimeoutExpired("fixture", 1)),
        ):
            for number in range(3):
                self.assertEqual(external.spool({"number": number}), 0)
            self.assertEqual(external.spool({"number": 3}), 1)
            paths = sorted((Path(directory) / "external-spool").glob("*.json"))
            retained = [path.read_bytes() for path in paths]
            self.assertEqual(
                [object_value(decode_json(data))["number"] for data in retained], [0, 2, 3]
            )
            self.assertEqual(external.replay(), (False, 3))
            self.assertEqual([path.read_bytes() for path in paths], retained)
            with patch.object(external, "database", return_value=b""):
                self.assertEqual(external.replay(), (True, 0))

    def test_new_schema_backup_precedes_create_and_existing_privilege_drift_is_rejected(
        self,
    ) -> None:
        """A retained schema needs no new backup, but must retain its private ownership."""
        calls: list[str] = []

        def execute(source: bytes) -> bytes:
            calls.append("create" if b"CREATE TABLE" in source else "query")
            if b"to_regclass" in source:
                return b""
            if b"SELECT version" in source:
                return b"1"
            return b"t" if b"count(*)=3" in source else b""

        with (
            patch.object(prepare, "SQL_ROOT", ROOT / "ops/ansible/files"),
            patch.object(prepare, "database", side_effect=execute),
            patch.object(prepare, "backup_database", side_effect=lambda: calls.append("backup")),
        ):
            self.assertTrue(prepare.prepare_external())
        self.assertLess(calls.index("backup"), calls.index("create"))
        with (
            patch.object(prepare, "database", side_effect=[b"exists", b"1", b"", b"f"]),
            patch.object(prepare, "backup_database") as backup,
            self.assertRaises(ReleaseError),
        ):
            _ = prepare.prepare_external()
        backup.assert_not_called()

    def test_report_exposes_spool_loss_without_cache_or_raw_response(self) -> None:
        """The operator report includes missing history without disclosing API or local secrets."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(report, "STATE", Path(directory)),
            patch.object(report, "SQL_ROOT", ROOT / "ops/ansible/files"),
            patch("os.geteuid", return_value=0),
            patch.object(report, "database", return_value=b'{"incidents":[]}'),
            patch("sys.stdout", new_callable=io.StringIO) as output,
        ):
            _ = (Path(directory) / "external.json").write_text(
                json.dumps(
                    {
                        "dropped": 2,
                        "lastPollAt": 1,
                        "seen": {"private": "excluded"},
                    }
                )
            )
            self.assertEqual(report.main(), 0)
            data = object_value(decode_json(output.getvalue()))
            self.assertEqual(object_value(data["importer"])["discardedSnapshots"], 2)
            self.assertNotIn("excluded", output.getvalue())


if __name__ == "__main__":
    _ = unittest.main()
