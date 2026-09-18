"""Offline one-command deploy tests; no GitHub, SSH, Ansible or service actions."""

import argparse
from contextlib import redirect_stdout
from copy import deepcopy
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('deploy_controller_tests', ROOT / 'build/deploy.py')
DEPLOY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DEPLOY)
REVISION = 'a' * 40
SECRET = 'PRIVATE_CONTROLLER_INVENTORY_SENTINEL'


def ci_run(**changes):
    return dict({'id': 789, 'name': 'CI', 'path': '.github/workflows/ci.yml', 'event': 'push',
                 'head_branch': 'main', 'head_sha': REVISION, 'status': 'completed', 'conclusion': 'success',
                 'repository': {'full_name': 'owner/repo'}, 'head_repository': {'full_name': 'owner/repo'}}, **changes)


def artifact(**changes):
    return dict({'id': 123, 'expired': False, 'name': f'simplestchat-production-{REVISION}',
                 'size_in_bytes': 12345, 'digest': 'sha256:' + 'b' * 64,
                 'workflow_run': {'id': 789, 'head_sha': REVISION, 'repository_id': 42, 'head_repository_id': 42}},
                **changes)


def api_records():
    return [{'workflow_runs': [ci_run()]}, ci_run(),
            {'total_count': 1, 'artifacts': [artifact()]}, artifact(), ci_run()]


def inventory():
    return {'benchmark_hosts': {'hosts': ['public']}, '_meta': {'hostvars': {'public': {
        'scpub_domain': 'chat.example.test', 'ansible_host': '192.0.2.10', 'ansible_user': 'root',
        'ansible_ssh_private_key_file': '/private/key', 'controller_fixture': SECRET}}}}


class FakeRunner:
    def __init__(self, fixture, output=None):
        self.fixture = fixture
        self.output = output

    def run(self, argv, **kwargs):
        state = self.fixture
        state.calls.append((argv, kwargs))
        if argv[:2] == ['git', 'rev-parse']:
            state.revision_reads += 1
            return 0, REVISION if state.revision_reads == 1 else state.final_revision
        if argv[:2] == ['git', 'status']:
            return 0, ' M changed-file' if state.dirty else ''
        if argv[:3] == ['git', 'remote', 'get-url']:
            return 0, state.remote
        if argv[:2] == ['git', 'check-ignore']:
            return 0, argv[-1]
        if argv == ['/node', '--version']:
            return 0, 'v22.12.0'
        if argv[0] == '/ansible-inventory':
            state.inventory_reads += 1
            value = state.inventory if state.inventory_reads == 1 else state.final_inventory
            return 0, json.dumps(value)
        if argv[0] == '/ansible-playbook':
            if state.release_error is not None:
                raise state.release_error
            return 0, ''
        if argv[0] == '/node':
            return 0, json.dumps({'passed': state.smoke_passed, 'origin': 'https://chat.example.test', 'room': 'lobby'})
        raise AssertionError(f'Unexpected test command: {argv}')


class DeployTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='simplestchat-deploy-controller.')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.inventory_path = self.root / 'inventory.local.yml'
        self.inventory_path.write_text('unused offline inventory fixture\n')
        self.args = argparse.Namespace(inventory=str(self.inventory_path), repository='owner/repo',
            origin='https://chat.example.test', room='lobby', limit=None, wait_seconds=30,
            install_helpers=False, ansible_playbook=None)
        self.calls = []
        self.inventory = inventory()
        self.final_inventory = deepcopy(self.inventory)
        self.inventory_reads = 0
        self.revision_reads = 0
        self.final_revision = REVISION
        self.remote = 'https://github.com/owner/repo.git'
        self.dirty = False
        self.release_error = None
        self.smoke_passed = True

    def execute(self, records=None):
        with patch.object(DEPLOY.BUILD, 'Runner', side_effect=lambda output=None: FakeRunner(self, output)), \
             patch.object(DEPLOY, 'controller_tools', return_value=('/ansible-playbook', '/ansible-inventory', '/node')), \
             patch.object(DEPLOY.FETCH, 'api', side_effect=records if records is not None else api_records()) as api:
            old_umask = os.umask(0o077)
            try:
                result = DEPLOY.execute(self.args, self.root)
            finally:
                os.umask(old_umask)
        return result, api

    def deployments(self):
        return [call for call in self.calls if call[0][0] == '/ansible-playbook']

    def smokes(self):
        return [call for call in self.calls if call[0][:2] == ['/node', str(self.root / 'build/public-smoke.mjs')]]

    def test_clean_exact_ci_image_deploys_once_then_smokes_with_private_evidence(self):
        report, api = self.execute()
        self.assertTrue(report['passed'] and report['deployed'])
        self.assertEqual(report['artifactId'], 123)
        self.assertEqual(report['ciRunId'], 789)
        self.assertEqual(len(self.deployments()), 1)
        self.assertEqual(len(self.smokes()), 1)
        self.assertEqual(self.revision_reads, 2)
        self.assertEqual(self.inventory_reads, 2)
        command, configuration = self.deployments()[0]
        extra = json.loads(command[command.index('--extra-vars') + 1])
        self.assertEqual(extra, {'scpub_release_repository': 'owner/repo', 'scpub_release_artifact_id': 123,
            'scpub_release_expected_revision': REVISION, 'scpub_release_ci_run': 789,
            'scpub_release_prepared': True, 'scpub_release_deploy': True})
        self.assertEqual(configuration['timeout'], 2400)
        self.assertEqual(command[command.index('--limit') + 1], 'public')
        self.assertTrue(self.calls.index(self.deployments()[0]) < self.calls.index(self.smokes()[0]))
        self.assertFalse(any(call[0][0] in ('docker', 'ssh') or 'push' in call[0] for call in self.calls))
        self.assertEqual(api.call_count, 5)
        evidence = Path(report['evidence'])
        self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
        frozen = Path(command[command.index('-i') + 1])
        self.assertEqual(frozen.parent, evidence)
        self.assertEqual(json.loads(frozen.read_text()), {'benchmark_hosts': {'hosts': {
            'public': self.inventory['_meta']['hostvars']['public']}}})
        for path in evidence.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            if path != frozen:
                self.assertNotIn(SECRET, path.read_text())
        self.assertNotIn(SECRET, json.dumps(report))
        self.assertNotIn(SECRET, json.dumps(command))
        self.assertEqual(json.loads((evidence / 'outcome.json').read_text()), report)

    def test_helper_installation_is_only_selected_explicitly(self):
        self.args.install_helpers = True
        report, _ = self.execute()
        self.assertTrue(report['passed'])
        command = self.deployments()[0][0]
        self.assertFalse(json.loads(command[command.index('--extra-vars') + 1])['scpub_release_prepared'])

    def test_dirty_checkout_or_wrong_origin_never_queries_ci_or_deploys(self):
        for dirty, remote in [(True, self.remote), (False, 'https://github.com/other/repo.git'),
                              (False, 'https://credential@github.com/owner/repo.git')]:
            self.dirty, self.remote = dirty, remote
            self.revision_reads = 0
            with self.subTest(dirty=dirty, remote=remote):
                report, api = self.execute()
                self.assertFalse(report['passed'])
                api.assert_not_called()
        self.assertEqual(self.deployments(), [])
        self.assertFalse((self.root / 'results').exists())

    def test_checkout_changed_during_ci_wait_cannot_deploy(self):
        self.final_revision = 'c' * 40
        report, _ = self.execute()
        self.assertEqual(report['failureClass'], 'checkout_changed')
        self.assertEqual(self.deployments(), [])
        self.assertEqual(report['remoteOutcome'], 'not_started')

    def test_same_alias_cannot_switch_connection_details_during_ci_wait(self):
        for field, value in [('ansible_host', '192.0.2.20'), ('ansible_user', 'other'),
                             ('ansible_ssh_private_key_file', '/different/key'), ('ansible_port', 2222)]:
            self.inventory_reads = self.revision_reads = 0
            self.final_inventory = deepcopy(self.inventory)
            self.final_inventory['_meta']['hostvars']['public'][field] = value
            with self.subTest(field=field):
                report, _ = self.execute()
                self.assertEqual(report['failureClass'], 'inventory_target_changed')
                self.assertEqual(self.deployments(), [])

    def test_host_group_name_collision_cannot_expand_the_frozen_inventory(self):
        self.inventory['benchmark_hosts']['hosts'].append('second')
        self.inventory['public'] = {'hosts': ['public', 'second']}
        self.inventory['_meta']['hostvars']['second'] = {'scpub_domain': 'chat.example.test'}
        self.final_inventory = deepcopy(self.inventory)
        self.args.limit = 'public'
        report, _ = self.execute()
        self.assertTrue(report['passed'])
        command = self.deployments()[0][0]
        frozen = json.loads(Path(command[command.index('-i') + 1]).read_text())
        self.assertEqual(list(frozen['benchmark_hosts']['hosts']), ['public'])

    def test_multiple_targets_and_wrong_smoke_origin_fail_before_ci(self):
        for mode in ['multiple', 'origin']:
            self.inventory = inventory()
            self.inventory_reads = self.revision_reads = 0
            if mode == 'multiple':
                self.inventory['benchmark_hosts']['hosts'].append('second')
            else:
                self.inventory['_meta']['hostvars']['public']['scpub_domain'] = 'other.example.test'
            with self.subTest(mode=mode):
                report, api = self.execute()
                self.assertFalse(report['passed'])
                api.assert_not_called()
        self.assertEqual(self.deployments(), [])

    def test_release_failure_or_interrupt_never_retries_or_runs_smoke(self):
        for error in [DEPLOY.BUILD.BuildError(SECRET), DEPLOY.DeployError('interrupted'), KeyboardInterrupt()]:
            self.calls = []
            self.inventory_reads = self.revision_reads = 0
            self.release_error = error
            with self.subTest(error=type(error).__name__):
                report, _ = self.execute()
                self.assertFalse(report['passed'] or report['deployed'])
                self.assertEqual(report['phase'], 'deploy')
                self.assertEqual(report['remoteOutcome'], 'inspect_if_interrupted')
                self.assertEqual(len(self.deployments()), 1)
                self.assertEqual(self.smokes(), [])
                self.assertNotIn(SECRET, json.dumps(report))

    def test_smoke_failure_retains_successful_deployment_without_retry_or_rollback(self):
        self.smoke_passed = False
        report, _ = self.execute()
        self.assertFalse(report['passed'])
        self.assertTrue(report['deployed'])
        self.assertEqual(report['failureClass'], 'public_smoke_failed')
        self.assertEqual(len(self.deployments()), 1)
        self.assertEqual(len(self.smokes()), 1)

    def test_missing_failed_cancelled_or_expired_ci_never_deploys(self):
        failed = [{'workflow_runs': [ci_run()]}, ci_run(conclusion='failure')]
        cancelled = [{'workflow_runs': [ci_run()]}, ci_run(conclusion='cancelled')]
        expired = api_records()
        expired[3]['expired'] = True
        for records, code in [([{'workflow_runs': []}], 'ci_missing_push_required'),
                               (failed, 'ci_failed'), (cancelled, 'ci_failed'),
                               (expired, 'artifact_identity_mismatch')]:
            self.inventory_reads = self.revision_reads = 0
            with self.subTest(code=code):
                report, _ = self.execute(records)
                self.assertEqual(report['failureClass'], code)
                self.assertEqual(self.deployments(), [])

    def test_wait_is_bounded_and_pins_one_run_without_dispatching_or_switching(self):
        now = [0]
        records = api_records()
        records.insert(1, ci_run(status='in_progress', conclusion=None))
        with patch.object(DEPLOY.time, 'monotonic', side_effect=lambda: now[0]), \
             patch.object(DEPLOY.time, 'sleep', side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)) as sleep, \
             patch.object(DEPLOY.FETCH, 'api', side_effect=records) as api, redirect_stdout(io.StringIO()):
            selected = DEPLOY.select_ci_artifact(self.args, REVISION)
        self.assertEqual(selected['ciRunId'], 789)
        sleep.assert_called_once_with(15)
        self.assertTrue(all('dispatch' not in call.args[0] for call in api.call_args_list))
        self.assertEqual(sum('/workflows/ci.yml/runs?' in call.args[0] for call in api.call_args_list), 1)

        self.args.wait_seconds = 0
        with patch.object(DEPLOY.FETCH, 'api', side_effect=[{'workflow_runs': [ci_run()]},
             ci_run(status='queued', conclusion=None)]), patch.object(DEPLOY.time, 'sleep') as sleep:
            with self.assertRaisesRegex(DEPLOY.DeployError, 'ci_wait_timeout'):
                DEPLOY.select_ci_artifact(self.args, REVISION)
        sleep.assert_not_called()

    def test_wrong_workflow_revision_repository_or_artifact_cannot_be_selected(self):
        for changes in [{'event': 'pull_request'}, {'head_branch': 'feature'}, {'head_sha': 'b' * 40},
                        {'path': '.github/workflows/other.yml'}, {'name': 'Other'},
                        {'head_repository': {'full_name': 'fork/repo'}}]:
            with self.subTest(changes=changes), patch.object(DEPLOY.FETCH, 'api',
                 return_value={'workflow_runs': [ci_run(**changes)]}), self.assertRaises(DEPLOY.DeployError):
                DEPLOY.select_ci_artifact(self.args, REVISION)
        records = api_records()
        records[3]['workflow_run']['id'] = 456
        with patch.object(DEPLOY.FETCH, 'api', side_effect=records), self.assertRaises(DEPLOY.FETCH.FetchError):
            DEPLOY.select_ci_artifact(self.args, REVISION)
        records = api_records()
        records[2]['total_count'] = 2
        with patch.object(DEPLOY.FETCH, 'api', side_effect=records), self.assertRaisesRegex(
                DEPLOY.DeployError, 'ci_artifact_missing_or_ambiguous'):
            DEPLOY.select_ci_artifact(self.args, REVISION)

    def test_ssh_origin_and_controller_configuration_are_explicit(self):
        for remote in ['git@github.com:owner/repo.git', 'ssh://git@github.com/owner/repo.git',
                       'https://github.com/owner/repo']:
            self.remote = remote
            self.assertEqual(DEPLOY.checkout_identity(FakeRunner(self), self.root, 'owner/repo'), REVISION)
        with patch.dict(os.environ, {'ANSIBLE_HOST_KEY_CHECKING': 'False', 'ANSIBLE_CONFIG': '/unreviewed',
                                    'GH_TOKEN': SECRET}):
            environment = DEPLOY.ansible_environment(self.root)
        self.assertEqual(environment['ANSIBLE_HOST_KEY_CHECKING'], 'True')
        self.assertEqual(environment['ANSIBLE_CONFIG'], str(self.root / 'ops/ansible/ansible.cfg'))
        self.assertEqual(environment['GH_TOKEN'], SECRET)

    def test_help_does_not_inspect_git_or_start_any_command(self):
        with patch.object(DEPLOY, 'execute') as execute, redirect_stdout(io.StringIO()) as output:
            self.assertEqual(DEPLOY.main(['--help']), 0)
        execute.assert_not_called()
        self.assertIn('--install-helpers', output.getvalue())


if __name__ == '__main__':
    unittest.main()
