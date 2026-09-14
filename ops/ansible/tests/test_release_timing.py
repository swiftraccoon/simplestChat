"""Synthetic callback events only; no inventory, host, subprocess or network."""

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[1] / 'callback_plugins/release_timing.py'
SPEC = importlib.util.spec_from_file_location('release_timing_tests', SOURCE)
TIMING = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TIMING)
SECRET = 'PRIVATE_CREDENTIAL_SENTINEL'


class Result:
    def __init__(self, task, host):
        self.task, self.host = task, host

    @property
    def result(self):
        raise AssertionError('Timing must never access result contents')


class ReleaseTimingTests(unittest.TestCase):
    def setUp(self):
        self.callback = TIMING.CallbackModule()
        self.callback._display = Mock()
        self.clock = patch.object(TIMING.time, 'monotonic', return_value=100.0)
        self.now = self.clock.start()
        self.addCleanup(self.clock.stop)
        self.host = SimpleNamespace(_uuid='host-internal-uuid', name=SECRET)
        self.task = SimpleNamespace(_uuid='01234567-1234-1234-1234-0123456789ab',
            name='Verify the exact GitHub release and fetch it directly onto the prepared host',
            args={'url': SECRET}, get_path=lambda: '/private/' + SECRET + '/ops/ansible/release.yml:148')

    def records(self):
        return [json.loads(call.args[0]) for call in self.callback._display.display.call_args_list]

    def test_callback_is_opt_in_and_adds_to_the_default_output(self):
        self.assertEqual(self.callback.CALLBACK_TYPE, 'aggregate')
        self.assertEqual(self.callback.CALLBACK_NAME, 'release_timing')
        self.assertIs(self.callback.CALLBACK_NEEDS_ENABLED, True)
        self.assertEqual(TIMING.LABELS['Verify the host and exact prepared helpers without broad fact gathering'],
                         'prepared_host')

    def test_monotonic_dispatch_to_result_and_playbook_timings(self):
        self.callback.v2_playbook_on_start(object())
        self.now.return_value = 102.0
        self.callback.v2_runner_on_start(self.host, self.task)
        self.now.return_value = 104.125
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.now.return_value = 110.0
        self.callback.v2_playbook_on_stats(object())
        task, summary = self.records()
        self.assertEqual(task['elapsedMs'], 2125.0)
        self.assertEqual(task['label'], 'github_fetch')
        self.assertEqual(task['releaseLine'], 148)
        self.assertEqual(task['status'], 'ok')
        self.assertEqual(summary['elapsedMs'], 10000.0)
        self.assertEqual(summary['taskCounts']['ok'], 1)
        self.assertNotIn(SECRET, json.dumps(self.records()))

    def test_failed_skipped_and_unreachable_events_preserve_their_actual_status(self):
        for method, status in (('v2_runner_on_failed', 'failed'), ('v2_runner_on_skipped', 'skipped'),
                               ('v2_runner_on_unreachable', 'unreachable')):
            with self.subTest(status=status):
                self.callback.v2_runner_on_start(self.host, self.task)
                result = Result(self.task, self.host)
                original = dict(vars(result))
                if status == 'failed':
                    getattr(self.callback, method)(result, ignore_errors=True)
                else:
                    getattr(self.callback, method)(result)
                self.assertEqual(vars(result), original)
                self.assertEqual(self.records()[-1]['status'], status)

    def test_arbitrary_names_paths_and_ids_cannot_leak_secrets(self):
        for name in (SECRET, '{{ ' + SECRET + ' }}', 'github_fetch ' + SECRET):
            self.task.name, self.task._uuid = name, SECRET
            self.task.get_path = lambda: '/private/' + SECRET + '/secret.yml:10'
            self.callback.v2_runner_on_start(self.host, self.task)
            self.callback.v2_runner_on_failed(Result(self.task, self.host))
            record = self.records()[-1]
            self.assertEqual(record['label'], 'task')
            self.assertIsNone(record['taskId'])
            self.assertIsNone(record['releaseLine'])
        self.assertNotIn(SECRET, json.dumps(self.records()))

    def test_pending_events_remain_unfinished_and_unknown_results_are_not_fabricated(self):
        self.callback.v2_playbook_on_start(object())
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.assertEqual(self.records(), [])
        self.callback.v2_runner_on_start(self.host, self.task)
        self.now.return_value = 105
        self.callback.v2_playbook_on_stats(object())
        task, summary = self.records()
        self.assertEqual(task['status'], 'unfinished')
        self.assertEqual(task['elapsedMs'], 5000)
        self.assertEqual(summary['taskCounts']['unfinished'], 1)
        self.assertEqual(self.callback._active, {})

    def test_multiple_hosts_are_timed_separately_without_inventory_names(self):
        second = SimpleNamespace(_uuid='second-host', name=SECRET)
        self.callback.v2_runner_on_start(self.host, self.task)
        self.now.return_value = 101
        self.callback.v2_runner_on_start(second, self.task)
        self.now.return_value = 103
        self.callback.v2_runner_on_ok(Result(self.task, second))
        self.now.return_value = 104
        self.callback.v2_runner_on_ok(Result(self.task, self.host))
        self.assertEqual([item['elapsedMs'] for item in self.records()], [2000, 4000])
        self.assertEqual([item['sequence'] for item in self.records()], [2, 1])
        self.assertNotIn(SECRET, json.dumps(self.records()))

    def test_event_and_output_size_limits_are_bounded(self):
        with patch.object(TIMING, 'MAX_TASKS', 2):
            for index in range(5):
                self.task._uuid = str(index)
                self.callback.v2_runner_on_start(self.host, self.task)
                self.callback.v2_runner_on_ok(Result(self.task, self.host))
            self.callback.v2_playbook_on_stats(object())
        records = self.records()
        self.assertEqual(len(records), 3)
        self.assertEqual(records[-1]['suppressedEvents'], 3)
        self.assertTrue(all(len(json.dumps(record)) < 1024 for record in records))

    def test_output_failure_does_not_replace_original_task_failure(self):
        self.callback._display.display.side_effect = OSError('closed timing sink')
        self.callback.v2_runner_on_start(self.host, self.task)
        result = Result(self.task, self.host)
        self.callback.v2_runner_on_failed(result)
        self.assertEqual(self.callback._counts['failed'], 1)
        self.assertEqual(self.callback._suppressed, 1)
        self.assertIs(result.task, self.task)


if __name__ == '__main__':
    unittest.main()
