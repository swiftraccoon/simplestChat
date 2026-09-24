"""Import bounded public GitHub check evidence without credentials or arbitrary destinations."""

import fcntl
import json
import os
import re
import subprocess
import sys
import time
from datetime import UTC, datetime, timedelta
from http import HTTPStatus
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from monitoring_report import SQL_ROOT, database
from release_json import (
    JsonObject,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)
from release_public import ReleaseError, atomic, require

if TYPE_CHECKING:
    from http.client import HTTPResponse

STATE = Path("/var/lib/simplestchat-monitoring")
REPOSITORY = "swiftraccoon/simplestChat"
API = "https://api.github.com/repos/" + REPOSITORY + "/actions/"
MAX_REQUESTS = 8
MAX_RESPONSE = 4 * 1024 * 1024
MAX_SPOOL = 144
MAX_SNAPSHOT = 120 * 1024
INTERVAL = 600
MAX_ATTEMPTS = 10
MAX_ATTEMPT_ID = 1_000_000
PAGE_SIZE = 100
MAX_RUNS = 2 * PAGE_SIZE
MAX_JOBS = 4
MAX_STEPS = 40
MAX_SEEN = 2 * MAX_RUNS * MAX_ATTEMPTS
CONCLUSIONS = frozenset(
    {
        "success",
        "failure",
        "cancelled",
        "timed_out",
        "skipped",
        "neutral",
        "action_required",
        "stale",
        "unknown",
    }
)
WORKFLOWS = {
    "availability": (
        "External HTTPS readiness",
        {
            "readiness": "Require configured origin and check readiness through the public proxy",
        },
    ),
    "canary": (
        "Owned browsers against the public deployment",
        {
            "direct": "Publish and receive in the canary room",
            "relay": "Require decoded media through TURN",
        },
    ),
}


class NoRedirect(HTTPRedirectHandler):
    """Never follow a response to another host, scheme or API path."""

    @override
    def redirect_request(self, *_args: object, **_kwargs: object) -> None:
        """Let urllib reject redirect status codes without issuing another request."""


class Github:
    """Enforce a total request budget, bounded JSON and the fixed unauthenticated API."""

    def __init__(self, state: JsonObject) -> None:
        """Share only retry timing with the private durable state."""
        self.state: JsonObject = state
        self.requests: int = 0

    def get(self, path: str) -> JsonObject:
        """Fetch an internally constructed path; reject redirects and secondary rate limits."""
        require(self.requests < MAX_REQUESTS, "GitHub polling budget exhausted")
        require(
            re.fullmatch(
                r"(?:workflows/(?:availability|canary)\.yml/runs\?[A-Za-z0-9%=&_.+-]+"
                + r"|runs/[1-9][0-9]*/attempts/[1-9][0-9]*/jobs\?per_page=100)",
                path,
            ),
            "Unexpected GitHub API path",
        )
        require(time.time() >= float(str(self.state.get("notBefore", 0))), "GitHub retry deferred")
        self.requests += 1
        request = Request(  # noqa: S310 - fixed HTTPS origin and strictly allowlisted relative paths.
            API + path,
            headers={
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2026-03-10",
                "User-Agent": "simplestchat-private-monitor/1",
            },
        )
        try:
            with cast(
                "HTTPResponse",
                build_opener(ProxyHandler({}), NoRedirect()).open(
                    request,
                    timeout=3,
                ),
            ) as response:
                require(response.status == HTTPStatus.OK, "GitHub response was not successful")
                data = response.read(MAX_RESPONSE + 1)
                require(len(data) <= MAX_RESPONSE, "Oversized GitHub response")
        except HTTPError as error:
            if error.code in (403, 429):
                reset = error.headers.get("X-RateLimit-Reset", "")
                retry = error.headers.get("Retry-After", "")
                wait_until = time.time() + INTERVAL
                if reset.isdecimal():
                    wait_until = max(wait_until, min(float(reset), time.time() + 3600))
                if retry.isdecimal():
                    wait_until = max(wait_until, time.time() + min(int(retry), 3600))
                self.state["notBefore"] = wait_until
            raise
        return object_value(decode_json(data))


def instant(value: object) -> datetime:
    """Accept only GitHub's explicit UTC timestamp format."""
    require(
        isinstance(value, str) and re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", value),
        "Invalid external timestamp",
    )
    return datetime.fromisoformat(str(value))


def runs(github: Github, workflow: str, since: str) -> tuple[list[JsonObject], bool]:
    """Inspect at most 200 recent runs per fixed workflow, with explicit truncation."""
    result: dict[int, JsonObject] = {}
    raw_ids: set[int] = set()
    total = 0
    first_total: int | None = None
    for page in (1, 2):
        query = urlencode(
            {
                "branch": "main",
                "created": ">=" + since,
                "per_page": 100,
                "page": page,
                "exclude_pull_requests": "true",
            }
        )
        value = github.get(f"workflows/{workflow}.yml/runs?" + query)
        total = integer_value(value["total_count"])
        if first_total is None:
            first_total = total
        entries = array_value(value["workflow_runs"])
        require(total >= 0 and len(entries) <= PAGE_SIZE, "Invalid workflow run count")
        for raw in entries:
            run = object_value(raw)
            require(
                run.get("path") == f".github/workflows/{workflow}.yml"
                and object_value(run["repository"]).get("full_name") == REPOSITORY
                and object_value(run["head_repository"]).get("full_name") == REPOSITORY,
                "Workflow repository identity differs",
            )
            identity = integer_value(run["id"])
            raw_ids.add(identity)
            if run.get("head_branch") != "main" or run.get("event") not in (
                "schedule",
                "workflow_dispatch",
            ):
                continue
            attempt = integer_value(run["run_attempt"])
            revision = string_value(run["head_sha"])
            require(
                identity > 0
                and 0 < attempt <= MAX_ATTEMPT_ID
                and re.fullmatch("[a-f0-9]{40}", revision),
                "Invalid workflow run identity",
            )
            _ = instant(run["created_at"])
            _ = instant(run["updated_at"])
            if run.get("status") != "completed":
                continue
            result[identity] = {
                "runId": identity,
                "attempt": attempt,
                "sourceRevision": revision,
                "createdAt": run["created_at"],
                "updatedAt": run["updated_at"],
                "workflow": workflow,
            }
        if total <= page * 100:
            break
    return list(result.values()), total > MAX_RUNS or total != first_total or len(raw_ids) != total


def classify(github: Github, run: JsonObject, attempt: int) -> JsonObject:
    """Validate actual attempt jobs/steps; empty, skipped or setup-only runs cannot pass."""
    identity = integer_value(run["runId"])
    workflow = string_value(run["workflow"])
    value = github.get(f"runs/{identity}/attempts/{attempt}/jobs?per_page=100")
    jobs = array_value(value["jobs"])
    require(
        integer_value(value["total_count"]) == len(jobs) and len(jobs) <= MAX_JOBS,
        "Incomplete external jobs response",
    )
    expected_name, expected_steps = WORKFLOWS[workflow]
    matches = [object_value(job) for job in jobs if object_value(job).get("name") == expected_name]
    checks: JsonObject = dict.fromkeys(expected_steps, "missing")
    conclusion = "unknown"
    started, completed = instant(run["updatedAt"]), instant(run["updatedAt"])
    if len(matches) == 1 and len(jobs) == 1:
        job = matches[0]
        require(
            job.get("run_id") == identity and job.get("head_sha") == run["sourceRevision"],
            "External job identity differs",
        )
        require(job.get("run_attempt", attempt) == attempt, "External job attempt differs")
        require(job.get("status") == "completed", "External job is unfinished")
        started, completed = instant(job["started_at"]), instant(job["completed_at"])
        require(
            started <= completed <= datetime.now(UTC) + timedelta(minutes=5),
            "External job timestamps differ",
        )
        conclusion = string_value(job.get("conclusion", "unknown"))
        require(conclusion in CONCLUSIONS, "Unknown external job conclusion")
        steps = [object_value(item) for item in array_value(job["steps"])]
        require(len(steps) <= MAX_STEPS, "Oversized external steps response")
        for key, expected in expected_steps.items():
            selected = [step for step in steps if step.get("name") == expected]
            if len(selected) == 1 and selected[0].get("status") == "completed":
                result = selected[0].get("conclusion")
                if result in ("success", "failure", "timed_out"):
                    checks[key] = "success" if result == "success" else "failure"
    result = "incomplete"
    if conclusion == "success" and all(item == "success" for item in checks.values()):
        result = "success"
    elif any(item == "failure" for item in checks.values()):
        result = "failure"
    return {
        "runId": identity,
        "attempt": attempt,
        "sourceRevision": run["sourceRevision"],
        "startedAt": started.isoformat(),
        "completedAt": completed.isoformat(),
        "result": result,
        "conclusion": conclusion,
        "checks": checks,
    }


def discover(
    github: Github, seen: JsonObject, since: str
) -> tuple[dict[str, JsonObject], list[tuple[JsonObject, int, str]]]:
    """Build a finite attempt queue while preserving every workflow's fetch status."""
    batches: dict[str, JsonObject] = {}
    pending: list[tuple[JsonObject, int, str]] = []
    for workflow in WORKFLOWS:
        batch: JsonObject = {
            "workflow": workflow,
            "apiOk": True,
            "complete": False,
            "pending": 0,
            "truncated": False,
            "hasCompleted": False,
            "runs": [],
        }
        batches[workflow] = batch
        try:
            entries, truncated = runs(github, workflow, since)
            batch["hasCompleted"] = bool(entries)
            batch["truncated"] = truncated
            for run in entries:
                attempts = integer_value(run["attempt"])
                if attempts > MAX_ATTEMPTS:
                    batch["truncated"] = True
                for attempt in range(max(1, attempts - MAX_ATTEMPTS + 1), attempts + 1):
                    key = f"{workflow}:{run['runId']}:{attempt}"
                    if key not in seen:
                        pending.append((run, attempt, key))
        except (OSError, ValueError, KeyError, ReleaseError):
            batch["apiOk"] = False
    pending.sort(key=lambda item: (string_value(item[0]["createdAt"]), item[1]), reverse=True)
    # One current attempt from each workflow precedes historical backfill.
    first = [
        next((item for item in pending if item[0]["workflow"] == name), None) for name in WORKFLOWS
    ]
    ordered = [item for item in first if item is not None]
    ordered.extend(item for item in pending if item not in ordered)
    return batches, ordered


def collect(state: JsonObject, revision: str) -> JsonObject:
    """Prefer current results while retaining explicit pending historical work."""
    now = datetime.now(UTC)
    previous = datetime.fromisoformat(string_value(state.get("observedAt", now.isoformat())))
    entries = sorted((STATE / "external-spool").glob("*.json"))
    if entries:
        data = entries[-1].read_bytes()
        require(len(data) <= MAX_SNAPSHOT, "Oversized external spool entry")
        latest = object_value(decode_json(data))
        previous = max(previous, datetime.fromisoformat(string_value(latest["observedAt"])))
    observed = max(now, previous + timedelta(microseconds=1)).isoformat()
    state["observedAt"] = observed
    since = (now - timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%SZ")
    seen = object_value(state.get("seen", {}))
    state["seen"] = seen
    for key in list(seen):
        if string_value(object_value(seen[key])["createdAt"]) < since:
            del seen[key]
    github = Github(state)
    batches, pending = discover(github, seen, since)
    for run, attempt, key in pending:
        batch = batches[string_value(run["workflow"])]
        if github.requests >= MAX_REQUESTS:
            batch["pending"] = integer_value(batch["pending"]) + 1
            continue
        try:
            value = classify(github, run, attempt)
            array_value(batch["runs"]).append(value)
            seen[key] = {"createdAt": run["createdAt"]}
        except (OSError, ValueError, KeyError, ReleaseError):
            batch["apiOk"] = False
            batch["pending"] = integer_value(batch["pending"]) + 1
    require(len(seen) <= MAX_SEEN, "External check cache exceeded its bound")
    for batch in batches.values():
        batch["complete"] = (
            batch["apiOk"] is True
            and batch["hasCompleted"] is True
            and batch["pending"] == 0
            and batch["truncated"] is False
        )
    return {
        "observedAt": observed,
        "windowStart": since,
        "revision": revision,
        "workflows": list(batches.values()),
    }


def spool(value: JsonObject) -> int:
    """Persist before remembering fetched attempts, with explicit bounded evidence loss."""
    directory = STATE / "external-spool"
    directory.mkdir(mode=0o700, exist_ok=True)
    entries = sorted(directory.glob("*.json"))
    sequence = max(time.time_ns(), int(entries[-1].stem) + 1 if entries else 0)
    data = json.dumps(value, separators=(",", ":")).encode()
    require(len(data) <= MAX_SNAPSHOT, "External snapshot exceeded its bound")
    path = directory / f"{sequence:020}.json"
    atomic(path, data)
    entries.append(path)
    dropped = max(0, len(entries) - MAX_SPOOL)
    for old in entries[1 : 1 + dropped]:
        old.unlink()
    return dropped


def replay() -> tuple[bool, int]:
    """Commit at most four idempotent snapshots per invocation; keep failed writes."""
    entries = sorted((STATE / "external-spool").glob("*.json"))
    healthy = True
    for path in entries[:4]:
        data = path.read_bytes()
        require(len(data) <= MAX_SNAPSHOT, "Oversized external spool entry")
        try:
            _ = database((SQL_ROOT / "monitoring-external.sql").read_bytes(), data.decode())
        except (OSError, ValueError, ReleaseError, subprocess.SubprocessError):
            healthy = False
            break
        path.unlink()
    return healthy, len(list((STATE / "external-spool").glob("*.json")))


def coverage_metrics() -> list[str]:
    """Read committed check coverage, never treating an unfinished import as a recovery."""
    data = database(b"""BEGIN READ ONLY; SET LOCAL statement_timeout='3s';
    SELECT COALESCE(json_agg(json_build_object('workflow',s.workflow,
        'failed',s.failure_since IS NOT NULL,'conclusive',COALESCE(
        (SELECT r.result IN ('success','failure') FROM operations.external_runs r
         WHERE r.workflow=s.workflow ORDER BY completed_at DESC,run_id DESC,attempt DESC
         LIMIT 1),false),'latest',COALESCE(
        (SELECT extract(epoch FROM max(completed_at)) FROM operations.external_runs r
         WHERE r.workflow=s.workflow AND r.result IN ('success','failure')),0))), '[]'::json)
    FROM operations.external_status s; COMMIT;""")
    samples = {
        string_value(object_value(item)["workflow"]): object_value(item)
        for item in array_value(decode_json(data))
    }
    lines: list[str] = []
    for workflow in WORKFLOWS:
        sample = samples.get(workflow, {})
        latest = float(str(sample.get("latest", 0)))
        require(latest >= 0, "Invalid external coverage timestamp")
        label = f'{{component="{workflow}"}}'
        lines += [
            f"simplestchat_ops_external_probe_timestamp_seconds{label} {latest}",
            f"simplestchat_ops_external_check_failed{label} {int(sample.get('failed') is True)}",
            f"simplestchat_ops_external_probe_complete{label} "
            + str(int(sample.get("conclusive") is True)),
        ]
    return lines


def main() -> int:
    """Serialize polling and enforce a durable ten-minute unauthenticated request budget."""
    require(os.geteuid() == 0, "External check import requires root")
    _ = os.umask(0o077)
    with (STATE / "external.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = STATE / "external.json"
        state = object_value(decode_json(path.read_bytes())) if path.exists() else {}
        now = time.time()
        if now < float(str(state.get("lastPollAt", 0))) + INTERVAL:
            return 0
        state["lastPollAt"] = now
        atomic(path, state)
        selected = object_value(
            decode_json(Path("/etc/simplestchat-public/images.json").read_bytes())
        )
        revision = string_value(selected["revision"])
        require(re.fullmatch("[a-f0-9]{40}", revision), "Invalid deployed revision")
        value = collect(state, revision)
        state["dropped"] = integer_value(state.get("dropped", 0)) + spool(value)
        atomic(path, state)
        healthy, pending = replay()
        try:
            committed = coverage_metrics()
        except (OSError, ValueError, ReleaseError, subprocess.SubprocessError):
            healthy = False
            committed = []
        lines = [
            f"simplestchat_ops_external_import_timestamp_seconds {now}",
            f"simplestchat_ops_external_database_ok {int(healthy)}",
            f"simplestchat_ops_external_spool_snapshots {pending}",
            f"simplestchat_ops_external_spool_dropped_total {state['dropped']}",
        ]
        lines.extend(committed)
        for batch_value in array_value(value["workflows"]):
            batch = object_value(batch_value)
            label = f'{{component="{batch["workflow"]}"}}'
            for field in ("apiOk", "complete", "pending"):
                metric = {"apiOk": "api_ok", "complete": "coverage_complete", "pending": "pending"}[
                    field
                ]
                sample = (
                    integer_value(batch[field]) if field == "pending" else int(batch[field] is True)
                )
                lines.append(f"simplestchat_ops_external_{metric}{label} {sample}")
        target = STATE / "textfile/external.prom"
        atomic(target, "\n".join(lines) + "\n")
        target.chmod(0o644)
    _ = sys.stdout.write("External check import completed; review private coverage report.\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ReleaseError, OSError, ValueError, KeyError, subprocess.SubprocessError):
        _ = sys.stderr.write("External check import failed; previous evidence remains private.\n")
        sys.exit(1)
