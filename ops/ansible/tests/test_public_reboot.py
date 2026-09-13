"""Offline reboot lifecycle tests; no host, Docker daemon or reboot is contacted.

The fixture accepts only the small Docker/Compose command set permitted during
an existing-container reboot. Journal, identity, readiness and recovery decisions
run against private temporary files; ownership and Linux boot IDs are mocked.
"""

from contextlib import redirect_stdout
from copy import deepcopy
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml

FILES = Path(__file__).resolve().parents[1] / 'files'
SPEC = importlib.util.spec_from_file_location('public_reboot', FILES / 'reboot-public.py')
REBOOT = importlib.util.module_from_spec(SPEC)
with patch.object(sys, 'path', [str(FILES), *sys.path]):
    SPEC.loader.exec_module(REBOOT)
PUBLIC = REBOOT.PUBLIC
OLD_BOOT = '11111111-1111-1111-1111-111111111111'
NEW_BOOT = '22222222-2222-2222-2222-222222222222'
REVISION = 'a' * 40
APP_IMAGE = 'sha256:' + 'a' * 64
PG_IMAGE = 'sha256:' + 'b' * 64
PROXY_IMAGE = 'sha256:' + 'c' * 64
PG_SELECTOR = 'docker.io/library/postgres:fixture@sha256:' + 'd' * 64
PROXY_SELECTOR = 'docker.io/library/caddy:fixture@sha256:' + 'e' * 64


class FixtureRunner:
    """Reject unapproved commands rather than supplying a permissive mock."""

    def __init__(self, state, attempt):
        self.state = state
        self.attempt = attempt

    def compose(self, *args, **kwargs):
        self.state.calls.append(('compose', args, kwargs))
        if args == ('start', '--help'):
            return self.state.start_help.encode()
        if len(args) == 4 and args[:3] == ('ps', '--all', '--quiet'):
            service = args[-1]
            value = self.state.containers[service]
            return (value['id'] if value else '').encode()
        if args[:2] == ('config', '--hash'):
            service = args[-1]
            value = self.state.containers[service]['configHash']
            if service == self.state.changed_hash:
                value = 'f' * 64
            return f'{service} {value}\n'.encode()
        if args == ('config', '--format', 'json'):
            return json.dumps(self.state.resolved).encode()
        if args == ('--profile', 'maintenance', 'ps', '--all', '--quiet', 'migrate'):
            return b'f' * 64 if self.state.migration_container else b''
        if args[0] == 'stop':
            service = args[-1]
            assert args == ('stop', '--timeout', '60' if service == 'postgres' else '30', service)
            value = self.state.containers[service]
            value['state'].update(Running=False, ExitCode=0)
            if service == self.state.stop_failure:
                self.state.stop_failure = None
                raise PUBLIC.ReleaseError('fixture graceful stop failed')
            if service == self.state.unclean_stop:
                value['state']['ExitCode'] = 137
            return b''
        if args[0] == 'start':
            service = args[-1]
            assert args == (('start', '--wait', '--wait-timeout', '180', 'postgres') if service == 'postgres'
                            else ('start', service))
            value = self.state.containers[service]
            value['state']['Running'] = True
            if service == 'postgres':
                value['state']['Health']['Status'] = 'unhealthy' if self.state.unhealthy_resume else 'healthy'
            return b''
        raise AssertionError(f'Unexpected Compose command: {args}')

    def docker(self, *args, **kwargs):
        self.state.calls.append(('docker', args, kwargs))
        if args[:2] == ('image', 'inspect'):
            selector = args[-1]
            if selector in (PG_SELECTOR, PROXY_SELECTOR):
                return (PG_IMAGE if selector == PG_SELECTOR else PROXY_IMAGE).encode()
            assert selector == APP_IMAGE
            return json.dumps({'id': APP_IMAGE, 'os': 'linux', 'architecture': 'amd64', 'user': '10001:10001',
                               'labels': {'org.opencontainers.image.revision': REVISION},
                               'cmd': ['/app/simplestChat'], 'entrypoint': None}).encode()
        if args[0] == 'inspect':
            return json.dumps(next(value for value in self.state.containers.values() if value and value['id'] == args[-1])).encode()
        raise AssertionError(f'Unexpected Docker command: {args}')

    def run(self, args, **kwargs):
        self.state.calls.append(('run', tuple(args), kwargs))
        assert args[0] == '/usr/bin/curl' and args[-1].endswith('/ready')
        assert '--insecure' not in args and '--location' not in args
        assert self.state.containers['postgres']['state']['Running']
        assert self.state.containers['simplestchat']['state']['Running']
        if args[-1].startswith('https:'):
            assert self.state.containers['caddy']['state']['Running']
        if self.state.unready or (self.state.proxy_unready and args[-1].startswith('https:')):
            raise PUBLIC.ReleaseError('fixture readiness unavailable')
        return b'{"status":"ready"}'


class PublicRebootTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='simplestchat-public-reboot-test.')
        self.root = Path(self.temporary.name).resolve()
        self.config = self.root / 'config'
        self.work = self.root / 'work'
        for directory in (self.config, self.work, self.root / 'results'):
            directory.mkdir(mode=0o700)
        (self.work / 'workload.lock').touch(mode=0o600)
        for name, mode in REBOOT.CONFIGURATION.items():
            (self.config / name).write_text(f'private fixture {name}\n')
            (self.config / name).chmod(mode)
        selected = {'revision': REVISION, 'serverImage': APP_IMAGE, 'postgresImage': PG_SELECTOR, 'caddyImage': PROXY_SELECTOR}
        (self.config / 'images.json').write_text(json.dumps(selected))
        self.before = {name: (self.config / name).read_bytes() for name in REBOOT.CONFIGURATION}
        self.current_boot = OLD_BOOT
        self.calls = []
        self.containers = {}
        for index, (service, image) in enumerate((('simplestchat', APP_IMAGE), ('postgres', PG_IMAGE), ('caddy', PROXY_IMAGE)), 1):
            self.containers[service] = {'id': str(index) * 64, 'image': image, 'configHash': str(index) * 64,
                                        'state': {'Running': True, 'OOMKilled': False, 'ExitCode': 0}}
        self.containers['postgres']['state']['Health'] = {'Status': 'healthy'}
        self.resolved = {'services': {service: {'restart': 'unless-stopped'} for service in REBOOT.SERVICES}}
        self.resolved['services']['simplestchat']['environment'] = {
            'RUN_MIGRATIONS': 'false', 'WEBAUTHN_ORIGIN': 'https://fixture.invalid',
        }
        self.changed_hash = None
        self.migration_container = False
        self.stop_failure = None
        self.unclean_stop = None
        self.unhealthy_resume = False
        self.unready = False
        self.proxy_unready = False
        self.start_help = '      --wait             Wait for services\n      --wait-timeout int  Maximum wait\n'
        self.protection_calls = []
        self.patches = [
            patch.object(PUBLIC, 'ROOT', self.root), patch.object(PUBLIC, 'CONFIG', self.config),
            patch.object(PUBLIC, 'WORK', self.work), patch.object(PUBLIC, 'protected', side_effect=self.protected),
            patch.object(PUBLIC, 'Runner', side_effect=lambda attempt: FixtureRunner(self, attempt)),
            patch.object(PUBLIC, 'boot_id', side_effect=lambda: self.current_boot),
            patch.object(PUBLIC.os, 'geteuid', return_value=0), patch.object(PUBLIC.signal, 'signal'),
            patch.object(PUBLIC.time, 'monotonic', side_effect=iter(range(0, 100000, 100))), patch.object(PUBLIC.time, 'sleep'),
        ]
        for value in self.patches:
            value.start()
        self.previous_umask = os.umask(0o077)

    def tearDown(self):
        os.umask(self.previous_umask)
        for value in reversed(self.patches):
            value.stop()
        self.temporary.cleanup()

    def protected(self, path, *, directory=False, modes=(0o600,), limit=None):
        """Exercise real fixture type/mode/size checks, replacing only root ownership."""
        self.protection_calls.append(path)
        metadata = path.lstat()
        PUBLIC.require(stat.S_IMODE(metadata.st_mode) in modes, 'Fixture permission mismatch')
        PUBLIC.require(stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode), 'Fixture type mismatch')
        PUBLIC.require(limit is None or metadata.st_size <= limit, 'Fixture size exceeded')

    def execute(self, action):
        with patch.object(sys, 'argv', ['reboot-public.py', action]), redirect_stdout(io.StringIO()) as output:
            REBOOT.main()
        return json.loads(output.getvalue())

    def state(self):
        return json.loads((self.root / 'release-state.json').read_text())

    def attempt(self):
        values = list((self.root / 'results').glob('reboot.*'))
        self.assertEqual(len(values), 1)
        return values[0]

    def report(self):
        return json.loads((self.attempt() / 'outcome.json').read_text())

    def mutations(self):
        return [args for kind, args, _ in self.calls if kind == 'compose' and args[0] in ('stop', 'start') and '--help' not in args]

    def test_older_compose_is_rejected_before_any_stops_or_unfinished_journal(self):
        for help_text in ('Usage: compose start\n', '      --wait-timeout int  Maximum wait\n'):
            with self.subTest(help_text=help_text):
                self.start_help = help_text
                with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Installed Compose must support'):
                    self.execute('prepare')
                self.assertEqual(self.mutations(), [])
                self.assertFalse((self.root / 'release-state.json').exists())

    def test_prepare_stops_only_existing_public_services_in_order_and_retains_unfinished_evidence(self):
        output = self.execute('prepare')
        self.assertEqual(self.mutations(), [('stop', '--timeout', '30', 'simplestchat'),
                                            ('stop', '--timeout', '30', 'caddy'), ('stop', '--timeout', '60', 'postgres')])
        state = self.state()
        self.assertEqual(state['bootId'], OLD_BOOT)
        self.assertEqual(state['phase'], 'await_reboot')
        self.assertIs(state['finalized'], False)
        self.assertIs(output['passed'], False, 'Preparation must not claim the reboot completed')
        self.assertEqual(state['identitySha256'], PUBLIC.sha256_file(self.attempt() / 'identity.json'))
        self.assertEqual({name: (self.config / name).read_bytes() for name in REBOOT.CONFIGURATION}, self.before)
        for path in (self.attempt() / 'identity.json', self.attempt() / 'outcome.json', self.root / 'release-state.json'):
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_changed_boot_resumes_same_containers_in_dependency_order_and_finalizes(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.calls.clear()
        output = self.execute('resume')
        self.assertEqual(self.mutations(), [('start', '--wait', '--wait-timeout', '180', 'postgres'),
                                            ('start', 'simplestchat'), ('start', 'caddy')])
        self.assertIs(output['passed'], True)
        self.assertIs(self.state()['finalized'], True)
        self.assertEqual(self.state()['phase'], 'reboot_complete')
        self.assertEqual(self.report()['recoveryBootId'], NEW_BOOT)
        self.assertEqual(Path(self.state()['attempt']), self.attempt())
        self.assertTrue((self.attempt() / 'resume').is_dir())

    def test_same_boot_resume_refuses_without_starting_anything(self):
        self.execute('prepare')
        self.calls.clear()
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('resume')
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()['phase'], 'await_reboot')

    def test_cancel_restores_original_boot_but_preserves_failed_reboot_outcome(self):
        self.execute('prepare')
        self.calls.clear()
        output = self.execute('cancel')
        self.assertEqual([args[-1] for args in self.mutations()], ['postgres', 'simplestchat', 'caddy'])
        self.assertIs(output['passed'], False)
        self.assertIs(self.report()['recoveryPassed'], True)
        self.assertIn('did not complete', self.report()['failure'])
        self.assertIs(self.state()['finalized'], True)
        self.assertEqual(self.state()['phase'], 'reboot_cancelled')

    def test_cancel_refuses_a_changed_boot(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.calls.clear()
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('cancel')
        self.assertEqual(self.calls, [])

    def test_preparation_failure_recovers_once_and_preserves_original_failure(self):
        self.stop_failure = 'caddy'
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'fixture graceful stop failed'):
            self.execute('prepare')
        report = self.report()
        self.assertIs(report['passed'], False)
        self.assertIs(report['recoveryPassed'], True)
        self.assertEqual(report['failure'], 'fixture graceful stop failed')
        self.assertEqual([args[-1] for args in self.mutations() if args[0] == 'start'], ['postgres', 'simplestchat', 'caddy'])
        self.assertIs(self.state()['finalized'], True)
        self.assertEqual(self.state()['phase'], 'reboot_prepare_failed_recovered')

    def test_failed_preparation_recovery_keeps_unfinished_evidence_and_original_failure(self):
        self.stop_failure = 'caddy'
        self.unhealthy_resume = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'fixture graceful stop failed'):
            self.execute('prepare')
        self.assertIs(self.report()['recoveryPassed'], False)
        self.assertIs(self.state()['finalized'], False)
        self.assertEqual(self.state()['phase'], 'stop_for_reboot')
        self.assertEqual([args[-1] for args in self.mutations() if args[0] == 'start'], ['postgres'])

    def test_force_killed_container_does_not_count_as_graceful_preparation(self):
        self.unclean_stop = 'simplestchat'
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'did not stop cleanly'):
            self.execute('prepare')
        self.assertIs(self.report()['passed'], False)
        self.assertIs(self.report()['recoveryPassed'], True)

    def test_unhealthy_database_rejects_preparation_before_stopping_services(self):
        self.containers['postgres']['state']['Health']['Status'] = 'unhealthy'
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Database must be healthy'):
            self.execute('prepare')
        self.assertEqual(self.mutations(), [])
        self.assertFalse((self.root / 'release-state.json').exists())

    def test_runtime_migrations_or_other_restart_policy_reject_preparation(self):
        for field, value in (('RUN_MIGRATIONS', 'true'), ('restart', 'always')):
            with self.subTest(field=field):
                original = deepcopy(self.resolved)
                if field == 'restart':
                    self.resolved['services']['simplestchat'][field] = value
                else:
                    self.resolved['services']['simplestchat']['environment'][field] = value
                with self.assertRaises(PUBLIC.ReleaseError):
                    self.execute('prepare')
                self.assertEqual(self.mutations(), [])
                self.resolved = original

    def test_changed_running_configuration_or_retained_migration_rejects_preparation(self):
        self.changed_hash = 'caddy'
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'configuration differs'):
            self.execute('prepare')
        self.assertEqual(self.mutations(), [])
        self.changed_hash = None
        self.migration_container = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'migration container'):
            self.execute('prepare')
        self.assertEqual(self.mutations(), [])

    def test_changed_configuration_after_reboot_refuses_service_starts(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        (self.config / 'Caddyfile').write_text('changed proxy config\n')
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Configuration changed'):
            self.execute('resume')
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()['finalized'], False)

    def test_changed_container_after_reboot_is_not_recreated(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.containers['simplestchat']['id'] = 'f' * 64
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'container changed'):
            self.execute('resume')
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()['finalized'], False)

    def test_missing_container_after_reboot_is_not_recreated(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.containers['postgres'] = None
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Exactly one existing postgres container'):
            self.execute('resume')
        self.assertEqual(self.mutations(), [])
        self.assertIs(self.state()['finalized'], False)

    def test_changed_resolved_environment_after_reboot_prevents_any_start(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.resolved['services']['simplestchat']['environment']['RUN_MIGRATIONS'] = 'true'
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Resolved configuration changed'):
            self.execute('resume')
        self.assertEqual(self.mutations(), [])

    def test_unfinished_preparation_cannot_be_started_again(self):
        self.execute('prepare')
        self.calls.clear()
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('prepare')
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()['phase'], 'await_reboot')

    def test_database_readiness_failure_prevents_app_and_proxy_start_and_automatic_retry(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.unhealthy_resume = True
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Database did not become healthy'):
            self.execute('resume')
        self.assertEqual([args[-1] for args in self.mutations()], ['postgres'])
        self.assertIs(self.state()['finalized'], False)
        self.assertIs(self.report()['passed'], False)
        self.calls.clear()
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('resume')
        self.assertEqual(self.calls, [])

    def test_app_readiness_failure_prevents_proxy_start(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.unready = True
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'readiness deadline'):
            self.execute('resume')
        self.assertEqual([args[-1] for args in self.mutations()], ['postgres', 'simplestchat'])
        self.assertIs(self.report()['passed'], False)

    def test_trusted_https_is_required_before_success(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.proxy_unready = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'readiness deadline'):
            self.execute('resume')
        self.assertIs(self.state()['finalized'], False)
        self.assertIs(self.report()['passed'], False)

    def test_unfinished_benchmark_blocks_prepare_and_post_reboot_recovery(self):
        PUBLIC.atomic(self.work / 'current.json', {'schemaVersion': 1, 'finalized': False})
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('prepare')
        self.assertEqual(self.calls, [])
        PUBLIC.atomic(self.work / 'current.json', {'schemaVersion': 1, 'finalized': True})
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        PUBLIC.atomic(self.work / 'current.json', {'schemaVersion': 1, 'finalized': False,
                                                 'phase': 'await_reboot', 'bootId': OLD_BOOT})
        self.calls.clear()
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute('resume')
        self.assertEqual(self.calls, [])

    def test_recovery_rejects_unprotected_or_tampered_evidence(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.calls.clear()
        snapshot = self.attempt() / 'identity.json'
        snapshot.chmod(0o644)
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'permission'):
            self.execute('resume')
        snapshot.chmod(0o600)
        snapshot.write_text('{}')
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'evidence changed'):
            self.execute('resume')
        self.assertEqual(self.calls, [])

    def test_recovery_rejects_attempt_path_outside_private_results(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        record = self.state()
        record['attempt'] = str(self.root / 'reboot.unrelated')
        PUBLIC.atomic(self.root / 'release-state.json', record)
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'attempt path'):
            self.execute('resume')
        self.assertEqual(self.calls, [])

    def test_finalized_reboot_cannot_be_resumed_again(self):
        self.execute('prepare')
        self.current_boot = NEW_BOOT
        self.execute('resume')
        self.calls.clear()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'No unfinished prepared reboot'):
            self.execute('resume')
        self.assertEqual(self.calls, [])


class RebootPlaybookTests(unittest.TestCase):
    def test_explicit_authorization_and_existing_host_guards_precede_reboot(self):
        play = yaml.safe_load((FILES.parent / 'reboot.yml').read_text())[0]
        self.assertEqual(play['serial'], 1)
        guard = play['pre_tasks'][0]
        self.assertEqual(guard['tags'], ['always'])
        self.assertIn('scpub_reboot | default(false) | bool', guard['ansible.builtin.assert']['that'])
        self.assertIn('scpub_enabled | bool', guard['ansible.builtin.assert']['that'])
        tasks = play['tasks']
        workflow = tasks[-1]
        preparation = workflow['block'][0]
        self.assertEqual(preparation['ansible.builtin.command']['argv'][-1], 'prepare')
        self.assertEqual(workflow['when'], 'not ansible_check_mode')
        self.assertIn('ansible.builtin.reboot', workflow['block'][1])
        self.assertEqual(workflow['block'][2]['ansible.builtin.command']['argv'][-1], 'resume')
        self.assertEqual(workflow['rescue'][0]['ansible.builtin.command']['argv'][-1], 'cancel')
        self.assertIs(workflow['rescue'][0]['failed_when'], False)
        self.assertIn('ansible.builtin.fail', workflow['rescue'][-1])
        for task in (preparation, workflow['block'][2], workflow['rescue'][0]):
            argv = task['ansible.builtin.command']['argv']
            self.assertIn('--property=RuntimeMaxSec=900', argv)
            self.assertIn('--wait', argv)
        self.assertFalse(any('ansible.builtin.apt' in task or 'ansible.builtin.service' in task for task in tasks))


if __name__ == '__main__':
    unittest.main()
