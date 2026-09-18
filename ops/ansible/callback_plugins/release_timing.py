"""Opt-in, payload-free controller timings; never interpret them as downtime."""

from __future__ import annotations

import json
import re
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, ClassVar, Literal, Protocol, cast, override, runtime_checkable

if TYPE_CHECKING:
    from collections.abc import Callable

from ansible.plugins.callback import CallbackBase

DOCUMENTATION = r"""
name: release_timing
type: aggregate
short_description: Emit bounded controller task timings without task payloads
description:
  - Enable with ANSIBLE_CALLBACKS_ENABLED=release_timing.
  - Measures monotonic dispatch-to-result time, including transport and module overhead.
  - Loop items are measured together; concurrent tasks can overlap.
  - Emits no arguments, result contents, command output, host names, or inventory values.
  - Missing completions remain unfinished; timings do not establish service downtime.
requirements:
  - Enable this callback explicitly.
"""

MAX_TASKS = 512
MAX_SOURCE_LENGTH = 4096
LABELS = {
    "Gathering Facts": "facts",
    "Require an explicitly selected artifact and prepared public host": "release_preflight",
    "Verify the host and exact prepared helpers without broad fact gathering": "prepared_host",
    "Require a fresh facts-free release unit identity": "unit_identity",
    "Require existing public configuration without exposing it": "existing_configuration",
    "Install the isolated release helper directory": "helper_directory",
    "Install the release command and shared artifact validator": "release_helpers",
    "Install the bounded GitHub artifact receiver": "fetch_helper",
    "Create private immutable release storage": "release_storage",
    "Verify the exact GitHub release and fetch it directly onto the prepared host": "github_fetch",
    "Transfer artifact files without overwriting retained releases": "artifact_copy",
    "Verify retained destination bytes match this exact controller artifact": "artifact_checksum",
    "Import and verify the image while the public service remains running": "image_stage",
    "Replace only the application when deployment was explicitly selected": "app_deploy",
}
type Status = Literal["ok", "failed", "skipped", "unreachable", "unfinished"]
type Record = dict[str, str | int | float | dict[Status, int] | None]


@runtime_checkable
class EventTask(Protocol):
    """Task metadata is untrusted and must be projected before output."""

    @property
    def name(self) -> object:
        """Return a task name without assuming it is a safe label."""
        ...

    def get_path(self) -> object:
        """Return upstream source metadata for bounded path parsing."""
        ...


@runtime_checkable
class EventResult(Protocol):
    """The observer needs identities, never result contents."""

    @property
    def host(self) -> object:
        """Identify the callback's host internally."""
        ...

    @property
    def task(self) -> object:
        """Identify the task without reading its arguments or result payload."""
        ...


@dataclass(frozen=True)
class ActiveTask:
    """The bounded, payload-free state retained for an outstanding task."""

    began: float
    sequence: int
    metadata: Record


class CallbackModule(CallbackBase):
    """Observe execution without changing its results or exposing payloads."""

    CALLBACK_VERSION: ClassVar[float] = 2.0
    CALLBACK_TYPE: ClassVar[str] = "aggregate"
    CALLBACK_NAME: ClassVar[str] = "release_timing"
    CALLBACK_NEEDS_ENABLED: ClassVar[bool] = True

    def __init__(
        self,
        *,
        clock: Callable[[], float] = time.monotonic,
        emit: Callable[[str], None] | None = None,
    ) -> None:
        """Allow deterministic observers without replacing Ansible's event objects."""
        super().__init__()
        self._clock: Callable[[], float] = clock
        self._sink: Callable[[str], None] = emit if emit is not None else self._display.display
        self._started: float | None = None
        self._active: dict[tuple[str, str], ActiveTask] = {}
        self._task_count: int = 0
        self._suppressed: int = 0
        self._counts: dict[Status, int] = {
            "ok": 0,
            "failed": 0,
            "skipped": 0,
            "unreachable": 0,
            "unfinished": 0,
        }

    @staticmethod
    def _identity(value: object) -> str | None:
        # Treat this external attribute as unknown until its scalar shape is checked.
        identifier = cast("object", getattr(value, "_uuid", None))
        return identifier if isinstance(identifier, str) else None

    @classmethod
    def _key(cls, host: object, task: object) -> tuple[str, str] | None:
        host_id, task_id = cls._identity(host), cls._identity(task)
        return (task_id, host_id) if host_id is not None and task_id is not None else None

    @staticmethod
    def _metadata(task: EventTask) -> Record:
        identifier = CallbackModule._identity(task)
        safe_id = (
            identifier if identifier and re.fullmatch(r"[a-fA-F0-9-]{1,64}", identifier) else None
        )
        name = task.name
        label = LABELS.get(name, "task") if isinstance(name, str) else "task"
        source = task.get_path()
        match = (
            re.search(r"(?:^|/)release\.yml:([1-9][0-9]{0,6})$", source)
            if isinstance(source, str) and len(source) <= MAX_SOURCE_LENGTH
            else None
        )
        return {"taskId": safe_id, "label": label, "releaseLine": int(match[1]) if match else None}

    def _emit(self, value: Record) -> None:
        record: Record = {"schemaVersion": 1, "observer": "release_timing", **value}
        try:
            self._sink(json.dumps(record, separators=(",", ":")))
        except Exception:  # noqa: BLE001 -- Observation must never replace an execution result.
            self._suppressed += 1

    @override
    def v2_playbook_on_start(self, playbook: object) -> None:
        """Start the monotonic controller interval without reading the playbook."""
        self._started = self._clock()

    @override
    def v2_runner_on_start(self, host: object, task: object) -> None:
        """Retain at most the fixed limit of identity-only outstanding tasks."""
        if not isinstance(task, EventTask):
            self._suppressed += 1
            return
        if self._task_count >= MAX_TASKS:
            self._suppressed += 1
            return
        key = self._key(host, task)
        if key is None:
            self._suppressed += 1
            return
        if key in self._active:
            return
        self._task_count += 1
        self._active[key] = ActiveTask(self._clock(), self._task_count, self._metadata(task))

    def _finish(self, result: object, status: Status) -> None:
        if not isinstance(result, EventResult):
            return
        key = self._key(result.host, result.task)
        if key is None:
            return
        active = self._active.pop(key, None)
        if active is None:
            return
        self._counts[status] += 1
        self._emit(
            {
                "event": "task",
                "sequence": active.sequence,
                **active.metadata,
                "status": status,
                "elapsedMs": round(max(0, self._clock() - active.began) * 1000, 3),
            }
        )

    @override
    def v2_runner_on_ok(self, result: object) -> None:
        """Record success without inspecting the result payload."""
        self._finish(result, "ok")

    @override
    def v2_runner_on_failed(self, result: object, ignore_errors: bool = False) -> None:
        """Preserve a failure even if the playbook elects to ignore it."""
        self._finish(result, "failed")

    @override
    def v2_runner_on_skipped(self, result: object) -> None:
        """Record that execution was skipped rather than fabricate success."""
        self._finish(result, "skipped")

    @override
    def v2_runner_on_unreachable(self, result: object) -> None:
        """Record an unreachable target separately from an executed failure."""
        self._finish(result, "unreachable")

    @override
    def v2_playbook_on_stats(self, stats: object) -> None:
        """Emit unfinished tasks and one bounded aggregate without reading stats."""
        ended = self._clock()
        for active in self._active.values():
            self._counts["unfinished"] += 1
            self._emit(
                {
                    "event": "task",
                    "sequence": active.sequence,
                    **active.metadata,
                    "status": "unfinished",
                    "elapsedMs": round(max(0, ended - active.began) * 1000, 3),
                }
            )
        self._active.clear()
        self._emit(
            {
                "event": "playbook",
                "elapsedMs": (
                    round(max(0, ended - self._started) * 1000, 3)
                    if self._started is not None
                    else None
                ),
                "taskCounts": dict(self._counts),
                "suppressedEvents": self._suppressed,
            }
        )
