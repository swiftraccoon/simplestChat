"""Synthetic typed callback events; no host, subprocess, or network access."""

from __future__ import annotations

import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Callable

from test_support import ROOT

# isort: split

import release_timing as timing
from release_json import JsonObject, decode_json, object_value

PRIVATE_MARKER = "PRIVATE_CREDENTIAL_SENTINEL"
TASK_ID = "01234567-1234-1234-0123-0123456789ab"
MAX_RECORD_LENGTH = 1024


@dataclass
class Clock:
    """A deterministic monotonic clock shared by every synthetic event."""

    now: float = 100.0

    def __call__(self) -> float:
        """Return the explicitly controlled test time."""
        return self.now


@dataclass
class Host:
    """Expose Ansible's internal identity without exposing its inventory name."""

    identity: str = "host-internal-uuid"
    name: str = PRIVATE_MARKER

    @property
    def _uuid(self) -> str:
        return self.identity


@dataclass
class Task:
    """Keep unsafe names and paths separate from the correlation identity."""

    identity: str = TASK_ID
    name: object = "Verify the exact GitHub release and fetch it directly onto the prepared host"
    path: object = f"/private/{PRIVATE_MARKER}/ops/ansible/release.yml:148"

    @property
    def _uuid(self) -> str:
        return self.identity

    def get_path(self) -> object:
        """Return the controlled source metadata in the upstream shape."""
        return self.path


@dataclass(frozen=True)
class Result:
    """Reject result-payload access while exposing the two event identities."""

    task: Task
    host: Host

    @property
    def result(self) -> None:
        """Fail if an observer reads the deliberately forbidden result body."""
        message = "Timing must never access result contents"
        raise AssertionError(message)


class ReleaseTimingTests(unittest.TestCase):
    """Verify bounded projection and unchanged execution outcomes."""

    clock: Clock
    callback: timing.CallbackModule
    host: Host
    task: Task
    output: list[str]

    def __init__(self, method_name: str = "runTest") -> None:
        """Create each test's typed event source and payload-free output collector."""
        super().__init__(method_name)
        self.clock = Clock()
        self.output = []
        self.callback = timing.CallbackModule(clock=self.clock, emit=self.output.append)
        self.host, self.task = Host(), Task()

    def records(self) -> list[JsonObject]:
        """Parse only the observer's bounded JSON records."""
        return [object_value(decode_json(line)) for line in self.output]

    def test_callback_is_opt_in_and_aggregate(self) -> None:
        """The observer supplements normal Ansible output only when enabled."""
        self.assertEqual(self.callback.CALLBACK_TYPE, "aggregate")
        self.assertEqual(
            Path(timing.__file__).resolve(), ROOT / "ops/ansible/callback_plugins/release_timing.py"
        )
        self.assertEqual(self.callback.CALLBACK_NAME, "release_timing")
        self.assertTrue(self.callback.CALLBACK_NEEDS_ENABLED)
        self.assertEqual(timing.LABELS[str(self.task.name)], "github_fetch")

    def test_monotonic_dispatch_and_completion(self) -> None:
        """Controller intervals use monotonic dispatch and completion boundaries."""
        self.callback.v2_playbook_on_start(object())
        self.clock.now = 102
        self.callback.v2_runner_on_start(self.host, self.task)
        self.clock.now = 104.125
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.clock.now = 110
        self.callback.v2_playbook_on_stats(object())
        task, summary = self.records()
        self.assertEqual(task["elapsedMs"], 2125)
        self.assertEqual(task["label"], "github_fetch")
        self.assertEqual(task["releaseLine"], 148)
        self.assertEqual(task["status"], "ok")
        self.assertEqual(summary["elapsedMs"], 10000)
        self.assertEqual(object_value(summary["taskCounts"])["ok"], 1)
        self.assertNotIn(PRIVATE_MARKER, "".join(self.output))

    def test_terminal_statuses_preserve_original_outcomes(self) -> None:
        """Ignored failure, skipped work, and unreachable hosts stay distinct."""
        events: tuple[tuple[Callable[[object], None], str], ...] = (
            (
                lambda result: self.callback.v2_runner_on_failed(result, ignore_errors=True),
                "failed",
            ),
            (self.callback.v2_runner_on_skipped, "skipped"),
            (self.callback.v2_runner_on_unreachable, "unreachable"),
        )
        for finish, status in events:
            with self.subTest(status=status):
                self.callback.v2_runner_on_start(self.host, self.task)
                result = Result(self.task, self.host)
                finish(result)
                self.assertIs(result.host, self.host)
                self.assertIs(result.task, self.task)
                self.assertEqual(self.records()[-1]["status"], status)

    def test_metadata_projection_does_not_leak_names_or_paths(self) -> None:
        """Arbitrary task names, source paths, and IDs become fixed safe labels."""
        for name in (
            PRIVATE_MARKER,
            "{{ " + PRIVATE_MARKER + " }}",
            "github_fetch " + PRIVATE_MARKER,
        ):
            self.task.name, self.task.identity, self.task.path = (
                name,
                PRIVATE_MARKER,
                f"/{PRIVATE_MARKER}/secret.yml:10",
            )
            self.callback.v2_runner_on_start(self.host, self.task)
            self.callback.v2_runner_on_failed(Result(self.task, self.host))
            record = self.records()[-1]
            self.assertEqual(record["label"], "task")
            self.assertIsNone(record["taskId"])
            self.assertIsNone(record["releaseLine"])
        self.assertNotIn(PRIVATE_MARKER, "".join(self.output))

    def test_pending_and_unknown_events_never_fabricate_success(self) -> None:
        """Unmatched completions are ignored and outstanding work stays unfinished."""
        self.callback.v2_playbook_on_start(object())
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.assertEqual(self.output, [])
        self.callback.v2_runner_on_start(self.host, self.task)
        self.clock.now = 105
        self.callback.v2_playbook_on_stats(object())
        task, summary = self.records()
        self.assertEqual(task["status"], "unfinished")
        self.assertEqual(task["elapsedMs"], 5000)
        self.assertEqual(object_value(summary["taskCounts"])["unfinished"], 1)
        self.callback.v2_playbook_on_stats(object())
        self.assertEqual(len(self.output), 3, "Finished playbooks retain no active task entries")

    def test_same_task_on_multiple_hosts_is_correlated_separately(self) -> None:
        """Internal host identity separates concurrent instances of one task."""
        other = Host("another-internal-uuid")
        self.callback.v2_runner_on_start(self.host, self.task)
        self.clock.now = 101
        self.callback.v2_runner_on_start(other, self.task)
        self.clock.now = 104
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.callback.v2_runner_on_ok(Result(self.task, other))
        self.assertEqual([record["elapsedMs"] for record in self.records()], [4000, 3000])
        self.assertNotIn(PRIVATE_MARKER, "".join(self.output))

    def test_event_count_and_record_size_are_bounded(self) -> None:
        """Only a fixed maximum number of task records can be retained or emitted."""
        for index in range(timing.MAX_TASKS + 1):
            self.task.identity = f"{index:032x}"
            self.callback.v2_runner_on_start(self.host, self.task)
            self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.callback.v2_playbook_on_stats(object())
        self.assertEqual(len(self.output), timing.MAX_TASKS + 1)
        self.assertEqual(self.records()[-1]["suppressedEvents"], 1)
        self.assertTrue(all(len(line) < MAX_RECORD_LENGTH for line in self.output))

    def test_output_failure_does_not_replace_task_failure(self) -> None:
        """A failing observer sink cannot raise through execution callbacks."""
        calls: list[str] = []

        def failing_sink(value: str) -> None:
            calls.append(value)
            raise OSError(PRIVATE_MARKER)

        callback = timing.CallbackModule(clock=self.clock, emit=failing_sink)
        callback.v2_runner_on_start(self.host, self.task)
        callback.v2_runner_on_failed(Result(self.task, self.host))
        callback.v2_playbook_on_stats(object())
        summary = object_value(decode_json(calls[-1]))
        self.assertEqual(object_value(summary["taskCounts"])["failed"], 1)
        self.assertEqual(summary["suppressedEvents"], 1)
        self.assertNotIn(PRIVATE_MARKER, "".join(calls))


if __name__ == "__main__":
    _ = unittest.main()
