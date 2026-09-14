"""Opt-in, payload-free controller timings; never interpret them as downtime."""

import json
import re
import time

from ansible.plugins.callback import CallbackBase

DOCUMENTATION = r'''
name: release_timing
type: aggregate
short_description: Emit bounded controller task timings without task payloads
description:
  - Enable with ANSIBLE_CALLBACKS_ENABLED=release_timing.
  - Measures monotonic controller dispatch-to-result time, including transport and module overhead.
  - Loop items are measured together; concurrent tasks can overlap.
  - Emits no arguments, result contents, command output, host names, or inventory values.
  - Missing completion events remain unfinished; these timings do not establish service downtime.
requirements:
  - Enable this callback explicitly.
'''

MAX_TASKS = 512
LABELS = {
    'Gathering Facts': 'facts',
    'Require an explicitly selected artifact and prepared public host': 'release_preflight',
    'Verify the host and exact prepared helpers without broad fact gathering': 'prepared_host',
    'Require a fresh facts-free release unit identity': 'unit_identity',
    'Require existing public configuration without exposing it': 'existing_configuration',
    'Install the isolated release helper directory': 'helper_directory',
    'Install the release command and shared artifact validator': 'release_helpers',
    'Install the bounded GitHub artifact receiver': 'fetch_helper',
    'Create private immutable release storage': 'release_storage',
    'Verify the exact GitHub release and fetch it directly onto the prepared host': 'github_fetch',
    'Transfer artifact files without overwriting retained releases': 'artifact_copy',
    'Verify retained destination bytes match this exact controller artifact': 'artifact_checksum',
    'Import and verify the image while the public service remains running': 'image_stage',
    'Replace only the application when deployment was explicitly selected': 'app_deploy',
}


class CallbackModule(CallbackBase):
    """An aggregate observer only: never changes task execution or results."""

    CALLBACK_VERSION = 2.0
    CALLBACK_TYPE = 'aggregate'
    CALLBACK_NAME = 'release_timing'
    CALLBACK_NEEDS_ENABLED = True

    def __init__(self):
        super().__init__()
        self._started = None
        self._active = {}
        self._task_count = 0
        self._suppressed = 0
        self._counts = dict.fromkeys(('ok', 'failed', 'skipped', 'unreachable', 'unfinished'), 0)

    @staticmethod
    def _key(host, task):
        # Internal UUIDs correlate callbacks; inventory names/values are unused.
        return task._uuid, host._uuid

    @staticmethod
    def _metadata(task):
        identifier = task._uuid
        safe_id = identifier if isinstance(identifier, str) and re.fullmatch(r'[a-fA-F0-9-]{1,64}', identifier) else None
        name = task.name
        label = LABELS.get(name, 'task') if isinstance(name, str) else 'task'
        source = task.get_path()
        match = re.search(r'(?:^|/)release\.yml:([1-9][0-9]{0,6})$', source) if isinstance(source, str) and len(source) <= 4096 else None
        return {'taskId': safe_id, 'label': label, 'releaseLine': int(match[1]) if match else None}

    def _emit(self, value):
        record = {'schemaVersion': 1, 'observer': 'release_timing', **value}
        try:
            self._display.display(json.dumps(record, separators=(',', ':')))
        except Exception:
            # Observability must not replace or reinterpret the original result.
            self._suppressed += 1

    def v2_playbook_on_start(self, playbook):
        self._started = time.monotonic()

    def v2_runner_on_start(self, host, task):
        if self._task_count >= MAX_TASKS:
            self._suppressed += 1
            return
        key = self._key(host, task)
        if key in self._active:
            return
        self._task_count += 1
        self._active[key] = (time.monotonic(), self._task_count, self._metadata(task))

    def _finish(self, result, status):
        active = self._active.pop(self._key(result.host, result.task), None)
        if active is None:
            return
        began, sequence, metadata = active
        self._counts[status] += 1
        self._emit({'event': 'task', 'sequence': sequence, **metadata, 'status': status,
                    'elapsedMs': round(max(0, time.monotonic() - began) * 1000, 3)})

    def v2_runner_on_ok(self, result):
        self._finish(result, 'ok')

    def v2_runner_on_failed(self, result, ignore_errors=False):
        self._finish(result, 'failed')

    def v2_runner_on_skipped(self, result):
        self._finish(result, 'skipped')

    def v2_runner_on_unreachable(self, result):
        self._finish(result, 'unreachable')

    def v2_playbook_on_stats(self, stats):
        ended = time.monotonic()
        for began, sequence, metadata in self._active.values():
            self._counts['unfinished'] += 1
            self._emit({'event': 'task', 'sequence': sequence, **metadata, 'status': 'unfinished',
                        'elapsedMs': round(max(0, ended - began) * 1000, 3)})
        self._active.clear()
        self._emit({'event': 'playbook',
                    'elapsedMs': round(max(0, ended - self._started) * 1000, 3) if self._started is not None else None,
                    'taskCounts': dict(self._counts), 'suppressedEvents': self._suppressed})
