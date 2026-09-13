"""Offline release lifecycle tests; Docker, Compose and root ownership are mocked.

Only private temporary fixture files are changed. Archive validation, selection
render comparison, journal writes, readiness decisions and rollback are real.
"""

from copy import deepcopy
from contextlib import redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
FILES = ROOT / 'ops/ansible/files'
SPEC = importlib.util.spec_from_file_location('public_release', FILES / 'release-public.py')
PUBLIC = importlib.util.module_from_spec(SPEC)
with patch.object(sys, 'path', [str(FILES), *sys.path]):
    SPEC.loader.exec_module(PUBLIC)

REVISION = 'a' * 40
OLD_REVISION = 'b' * 40
NEW_IMAGE = 'sha256:' + 'c' * 64
OLD_IMAGE = 'sha256:' + 'd' * 64
TAG = f'simplestchat-release/production:{REVISION}'
MIGRATIONS = {'1': hashlib.sha384(b'SELECT 1;\n').hexdigest()}


def write_fixture_artifact(directory):
    config = {'architecture': 'amd64', 'os': 'linux', 'config': {
        'User': '10001:10001', 'Cmd': ['/app/simplestChat'], 'Entrypoint': None,
        'Labels': {'org.opencontainers.image.revision': REVISION},
    }}
    with tarfile.open(directory / 'image.tar', 'w') as archive:
        files = {
            'manifest.json': json.dumps([{'Config': 'config.json', 'RepoTags': [TAG], 'Layers': ['layer.tar']}]).encode(),
            'config.json': json.dumps(config).encode(), 'layer.tar': b'fixture-not-executed',
        }
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    manifest = {
        'schemaVersion': 1, 'revision': REVISION, 'platform': 'linux/amd64',
        'archiveSha256': PUBLIC.sha256_file(directory / 'image.tar'), 'imageTag': TAG,
        'migrations': MIGRATIONS, 'createdAt': '2026-09-13T12:00:00Z',
    }
    (directory / 'release.json').write_text(json.dumps(manifest))
    return manifest


class FixtureRunner:
    """Model only the explicitly permitted Docker/Compose release commands."""

    def __init__(self, attempt, config):
        self.attempt = attempt
        self.config = config
        self.calls = []
        self.app_image = OLD_IMAGE
        self.app_running = True
        self.ups = 0
        self.database_healthy = True
        self.database_migrations = dict(MIGRATIONS)
        self.image_migrations = dict(MIGRATIONS)
        self.preview_change = False
        self.config_hash_mismatch = None
        self.candidate_wrong_image = False
        self.candidate_unready = False
        self.rollback_unready = False
        self.validation_exit = b'0'
        self.validation_images = {}
        self.database = self._container('2', 'sha256:' + '2' * 64, health=True)
        self.proxy = self._container('3', 'sha256:' + '3' * 64)

    @staticmethod
    def _container(character, image, health=False):
        value = {'id': character * 64, 'image': image, 'state': {
            'Running': True, 'OOMKilled': False, 'StartedAt': '2026-09-13T00:00:00Z',
        }, 'restarts': 0, 'configHash': character * 64}
        if health:
            value['state']['Health'] = {'Status': 'healthy'}
        return value

    def factory(self, attempt):
        self.attempt = attempt
        return self

    def container(self, service):
        self.calls.append(('container', (service,), {}))
        if service == 'simplestchat':
            if not self.app_running:
                raise PUBLIC.ReleaseError('fixture application stopped')
            return self._container('1', self.app_image)
        if service == 'postgres':
            value = deepcopy(self.database)
            if not self.database_healthy:
                value['state']['Health']['Status'] = 'unhealthy'
            return value
        if service == 'caddy':
            return deepcopy(self.proxy)
        raise AssertionError(f'Unexpected service: {service}')

    def run(self, args, **kwargs):
        self.calls.append(('run', tuple(args), kwargs))
        if args[0] != '/usr/bin/curl' or not args[-1].endswith('/ready'):
            raise AssertionError(f'Unexpected external command: {args}')
        if (self.candidate_unready and self.app_image == NEW_IMAGE) or (self.rollback_unready and self.ups >= 2):
            raise PUBLIC.ReleaseError('fixture readiness unavailable')
        return b'{"status":"ready"}'

    def compose(self, *args, **kwargs):
        self.calls.append(('compose', args, kwargs))
        if args[:2] == ('config', '--hash'):
            service = args[-1]
            value = self.container(service)['configHash']
            if service == self.config_hash_mismatch:
                value = '9' * 64
            return f'{service} {value}\n'.encode()
        if 'config' in args:
            filename = Path(kwargs.get('filename') or self.config / 'compose.public.yml')
            images = re.findall(r'image: "(sha256:[a-f0-9]{64})"', filename.read_text())
            assert len(images) == 2
            value = {'services': {
                'simplestchat': {'image': images[0], 'environment': {
                    'RUN_MIGRATIONS': 'false', 'WEBAUTHN_ORIGIN': 'https://fixture.invalid',
                }}, 'migrate': {'image': images[1]},
                'postgres': {'image': self.database['image']}, 'caddy': {'image': self.proxy['image']},
            }}
            if kwargs.get('filename') and self.preview_change:
                value['services']['simplestchat']['environment']['ALLOW_AD_HOC_ROOMS'] = 'true'
            return json.dumps(value).encode()
        if 'ps' in args and args[-1] == 'migrate':
            return b''
        if args == ('ps', '--all', '--quiet', 'simplestchat'):
            return ('1' * 64).encode()
        if args[0] == 'stop':
            assert args == ('stop', '--timeout', '30', 'simplestchat')
            self.app_running = False
            return b''
        if args[0] == 'up':
            assert args == ('up', '--detach', '--no-build', '--pull', 'never', '--no-deps', 'simplestchat')
            self.ups += 1
            selected = json.loads((self.config / 'images.json').read_text())['serverImage']
            self.app_image = OLD_IMAGE if self.candidate_wrong_image and self.ups == 1 else selected
            self.app_running = True
            return b''
        if args[0] == 'logs':
            return b'fixture candidate failure retained\n'
        raise AssertionError(f'Unexpected Compose command: {args}')

    def docker(self, *args, **kwargs):
        self.calls.append(('docker', args, kwargs))
        if args[0] == 'inspect' and args[-1] == '1' * 64:
            assert args[1:3] == ('--format', '{"image":{{json .Image}},"state":{{json .State}}}')
            return json.dumps({'image': self.app_image, 'state': {
                'Running': self.app_running, 'OOMKilled': False, 'ExitCode': 0,
            }}).encode()
        if args[:2] == ('image', 'load'):
            return b'Loaded fixture image\n'
        if args[:2] == ('image', 'inspect'):
            selector = args[-1]
            assert selector in (TAG, NEW_IMAGE, OLD_IMAGE)
            image = OLD_IMAGE if selector == OLD_IMAGE else NEW_IMAGE
            return json.dumps({
                'id': image, 'os': 'linux', 'architecture': 'amd64', 'user': '10001:10001',
                'labels': {'org.opencontainers.image.revision': OLD_REVISION if image == OLD_IMAGE else REVISION},
                'cmd': ['/app/simplestChat'], 'entrypoint': None,
            }).encode()
        if args[0] == 'create':
            assert args[args.index('--network') + 1] == 'none'
            assert args[args.index('--entrypoint') + 1] == '/usr/bin/timeout'
            assert '--kill-after=2s' in args and '10s' in args and '/bin/sh' in args
            container = '4' * 64
            self.validation_images[container] = args[args.index('--entrypoint') + 2]
            return container.encode()
        if args[0] in ('start', 'wait', 'logs', 'rm', 'inspect', 'stop', 'kill'):
            container = args[-1]
            assert container in self.validation_images, f'Unknown validation container: {args}'
            if args[0] == 'wait':
                return self.validation_exit
            if args[0] == 'logs':
                return ''.join(f'{checksum}  /app/migrations/{int(version):03d}_fixture.sql\n'
                               for version, checksum in self.image_migrations.items()).encode()
            if args[0] == 'inspect':
                return json.dumps({'id': container, 'image': self.validation_images[container],
                                   'state': {'Running': False, 'OOMKilled': False, 'ExitCode': 0}}).encode()
            return b''
        if args[0] == 'exec':
            if 'psql' in args:
                return ''.join(f'{version} t {checksum}\n' for version, checksum in self.database_migrations.items()).encode()
            if 'pg_dump' in args:
                data = b'PGDMP-fixture-only'
                if kwargs.get('output_path'):
                    Path(kwargs['output_path']).write_bytes(data)
                    return b''
                return data
            if 'pg_restore' in args:
                supplied = Path(kwargs['input_path']).read_bytes() if kwargs.get('input_path') else kwargs.get('input_data')
                assert supplied == b'PGDMP-fixture-only'
                return b'fixture validated archive table of contents\n'
        raise AssertionError(f'Unexpected Docker command: {args}')


class PublicReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='simplestchat-public-release-test.')
        self.root = Path(self.temporary.name).resolve()
        self.config = self.root / 'config'
        self.work = self.root / 'work'
        for directory in (self.config, self.work, self.root / 'results', self.root / 'releases'):
            directory.mkdir(mode=0o700)
        (self.work / 'workload.lock').touch(mode=0o600)
        self.directory = self.root / 'releases' / REVISION
        self.directory.mkdir(mode=0o700)
        self.manifest = write_fixture_artifact(self.directory)
        (self.config / 'images.json').write_text(json.dumps({'revision': OLD_REVISION, 'serverImage': OLD_IMAGE}))
        (self.config / 'compose.public.yml').write_text(
            f'services:\n  simplestchat:\n    image: "{OLD_IMAGE}"\n  migrate:\n    image: "{OLD_IMAGE}"\n')
        (self.config / 'app.env').write_text(f'SIMPLESTCHAT_IMAGE={OLD_IMAGE}\nRUN_MIGRATIONS=false\n')
        self.before = {name: (self.config / name).read_bytes() for name in PUBLIC.SELECTION}
        self.attempt = self.root / 'direct-attempt'
        self.attempt.mkdir(mode=0o700)
        self.runner = FixtureRunner(self.attempt, self.config)
        self.patches = [
            patch.object(PUBLIC, 'ROOT', self.root), patch.object(PUBLIC, 'CONFIG', self.config),
            patch.object(PUBLIC, 'WORK', self.work), patch.object(PUBLIC, 'protected'),
            patch.object(PUBLIC, 'Runner', self.runner.factory), patch.object(PUBLIC.os, 'geteuid', return_value=0),
            patch.object(PUBLIC.signal, 'signal'), patch.object(PUBLIC.time, 'monotonic', side_effect=iter(range(0, 100000, 100))),
            patch.object(PUBLIC.time, 'sleep'),
        ]
        for value in self.patches:
            value.start()
        self.previous_umask = os.umask(0o077)

    def tearDown(self):
        os.umask(self.previous_umask)
        for value in reversed(self.patches):
            value.stop()
        self.temporary.cleanup()

    def stage(self):
        return PUBLIC.stage(self.runner, self.directory, self.manifest)

    def execute(self, action='deploy'):
        with patch.object(sys, 'argv', ['release-public.py', action, REVISION]), redirect_stdout(io.StringIO()):
            PUBLIC.main()

    def report(self):
        files = list((self.root / 'results').glob('release.*/outcome.json'))
        self.assertEqual(len(files), 1)
        return json.loads(files[0].read_text())

    def app_mutations(self):
        return [args for kind, args, _ in self.runner.calls if kind == 'compose' and args[0] in ('stop', 'up')]

    def assert_config_unchanged(self):
        self.assertEqual({name: (self.config / name).read_bytes() for name in PUBLIC.SELECTION}, self.before)

    def test_stage_validates_image_and_migrations_then_reuses_target_local_identity(self):
        staged = self.stage()
        self.assertEqual(staged['serverImage'], NEW_IMAGE)
        self.assertEqual(staged['manifestSha256'], PUBLIC.sha256_file(self.directory / 'release.json'))
        self.assertEqual(sum(args[:2] == ('image', 'load') for kind, args, _ in self.runner.calls if kind == 'docker'), 1)
        self.runner.calls.clear()
        self.assertEqual(self.stage(), staged)
        self.assertFalse(any(args[:2] == ('image', 'load') or args[0] == 'create'
                             for kind, args, _ in self.runner.calls if kind == 'docker'))
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def test_archive_or_staged_manifest_tampering_fails_before_loading(self):
        self.stage()
        self.runner.calls.clear()
        with (self.directory / 'image.tar').open('ab') as output:
            output.write(b'changed')
        with self.assertRaises(PUBLIC.ArtifactError):
            self.stage()
        self.assertEqual(self.runner.calls, [])
        self.manifest = write_fixture_artifact(self.directory)
        record = json.loads((self.directory / 'staged.json').read_text())
        record['manifestSha256'] = '0' * 64
        (self.directory / 'staged.json').write_text(json.dumps(record))
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Staged release changed'):
            self.stage()
        self.assertEqual(self.runner.calls, [])

    def test_stage_checksum_mismatch_does_not_publish_success_or_touch_public_app(self):
        self.runner.image_migrations = {'1': 'f' * 96}
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'migration manifest'):
            self.execute('stage')
        self.assertFalse((self.directory / 'staged.json').exists())
        self.assertFalse(self.report()['passed'])
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def test_schema_change_is_rejected_before_stop_without_rollback(self):
        self.stage()
        journal = (self.root / 'release-state.json').read_bytes()
        self.runner.database_migrations = {'1': 'e' * 96}
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Schema changes'):
            self.execute()
        self.assertFalse(self.report()['passed'])
        self.assertNotIn('rollbackAttempted', self.report())
        self.assertEqual(self.app_mutations(), [])
        self.assertEqual((self.root / 'release-state.json').read_bytes(), journal)
        self.assert_config_unchanged()

    def test_success_replaces_only_app_and_preserves_database_and_proxy(self):
        self.stage()
        database, proxy = deepcopy(self.runner.database), deepcopy(self.runner.proxy)
        self.execute()
        report = self.report()
        self.assertTrue(report['passed'])
        self.assertEqual(report['phase'], 'complete')
        self.assertNotIn('rollbackAttempted', report)
        self.assertEqual(self.runner.app_image, NEW_IMAGE)
        self.assertEqual(self.runner.database, database)
        self.assertEqual(self.runner.proxy, proxy)
        self.assertEqual(len(self.app_mutations()), 2)
        self.assertTrue(all(args[-1] == 'simplestchat' for args in self.app_mutations()))
        self.assertEqual(json.loads((self.config / 'images.json').read_text())['serverImage'], NEW_IMAGE)
        self.assertIn(f'SIMPLESTCHAT_IMAGE={NEW_IMAGE}\n', (self.config / 'app.env').read_text())
        self.assertTrue(json.loads((self.root / 'release-state.json').read_text())['finalized'])
        self.assertIn('backupSha256', report)
        for kind, args, _ in self.runner.calls:
            if kind == 'docker':
                self.assertNotIn('pull', args)
                self.assertNotIn('build', args)
                if 'pg_restore' in args:
                    self.assertIn('--list', args, 'A release must never restore the live database')

    def test_wrong_candidate_image_rolls_back_once_but_remains_failed(self):
        self.stage()
        self.runner.candidate_wrong_image = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'not the staged image'):
            self.execute()
        self.assert_failed_rollback()

    def test_candidate_readiness_failure_rolls_back_once_but_remains_failed(self):
        self.stage()
        self.runner.candidate_unready = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'readiness deadline'):
            self.execute()
        self.assert_failed_rollback()

    def assert_failed_rollback(self):
        report = self.report()
        self.assertFalse(report['passed'])
        self.assertTrue(report['rollbackAttempted'])
        self.assertTrue(report['rollbackPassed'])
        self.assertEqual(len(self.app_mutations()), 4)
        self.assertEqual(self.runner.ups, 2)
        self.assertEqual(self.runner.app_image, OLD_IMAGE)
        captures = [index for index, (kind, args, _) in enumerate(self.runner.calls)
                    if kind == 'docker' and args[0] == 'inspect' and args[-1] == '1' * 64]
        stops = [index for index, (kind, args, _) in enumerate(self.runner.calls)
                 if kind == 'compose' and args[0] == 'stop']
        self.assertEqual(len(captures), 1, 'Retain the exact failed app state once before rollback')
        self.assertLess(stops[0], captures[0])
        self.assertLess(captures[0], stops[1])
        self.assertTrue(json.loads((self.root / 'release-state.json').read_text())['finalized'])
        self.assert_config_unchanged()

    def test_failed_rollback_retains_unfinished_journal_and_failure(self):
        self.stage()
        self.runner.candidate_unready = True
        self.runner.rollback_unready = True
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute()
        report = self.report()
        self.assertFalse(report['passed'])
        self.assertTrue(report['rollbackAttempted'])
        self.assertFalse(report['rollbackPassed'])
        self.assertFalse(json.loads((self.root / 'release-state.json').read_text())['finalized'])
        self.assertEqual(self.runner.ups, 2)

    def test_rendered_nonimage_change_is_rejected_before_stop_or_backup(self):
        self.stage()
        self.runner.preview_change = True
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'beyond image selection'):
            self.execute()
        self.assertFalse(self.report()['passed'])
        self.assertEqual(self.app_mutations(), [])
        self.assertFalse(any('pg_dump' in args for _, args, _ in self.runner.calls))
        self.assertNotIn('rollbackAttempted', self.report())
        self.assert_config_unchanged()

    def test_unhealthy_database_preflight_leaves_selection_and_app_untouched(self):
        self.stage()
        self.runner.database_healthy = False
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Database must be healthy'):
            self.execute()
        self.assertEqual(self.app_mutations(), [])
        self.assertNotIn('rollbackAttempted', self.report())
        self.assert_config_unchanged()

    def test_existing_container_config_drift_is_rejected_before_stop(self):
        self.stage()
        self.runner.config_hash_mismatch = 'postgres'
        with self.assertRaises(PUBLIC.ReleaseError):
            self.execute()
        self.assertFalse(self.report()['passed'])
        self.assertEqual(self.app_mutations(), [])
        self.assertNotIn('rollbackAttempted', self.report())
        self.assert_config_unchanged()

    def test_environment_without_trailing_newline_updates_exact_selection(self):
        (self.config / 'app.env').write_text(f'RUN_MIGRATIONS=false\nSIMPLESTCHAT_IMAGE={OLD_IMAGE}')
        _, candidate = PUBLIC.candidate_selection(self.runner, NEW_IMAGE, {'serverImage': OLD_IMAGE})
        self.assertEqual(candidate.decode(), f'RUN_MIGRATIONS=false\nSIMPLESTCHAT_IMAGE={NEW_IMAGE}\n')
        self.assertNotIn(NEW_IMAGE, (self.config / 'app.env').read_text(), 'Preview cannot alter installed selection')

    def test_failed_validation_container_retains_journal_and_blocks_another_attempt(self):
        self.runner.validation_exit = b'1'
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Packaged migration validation failed'):
            self.execute('stage')
        self.assertFalse(self.report()['passed'])
        state = json.loads((self.root / 'release-state.json').read_text())
        self.assertFalse(state['finalized'])
        self.assertEqual(state['phase'], 'validate_image')
        self.assertTrue((self.runner.attempt / 'validation-name.txt').is_file())
        calls_before = list(self.runner.calls)
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'unfinished release'):
            self.execute('stage')
        self.assertEqual(self.runner.calls, calls_before)
        self.assertEqual(self.app_mutations(), [])
        self.assert_config_unchanged()

    def reboot_state(self, *, phase='await_reboot', boot='11111111-1111-1111-1111-111111111111'):
        (self.root / 'release-state.json').write_text(json.dumps({
            'schemaVersion': 1, 'finalized': False, 'phase': phase, 'bootId': boot,
        }))

    def test_reboot_recovery_cannot_bypass_an_unfinished_benchmark(self):
        self.reboot_state()
        (self.work / 'current.json').write_text('{"schemaVersion":1,"finalized":false}')
        for options in ({'after_reboot': True}, {'cancel_reboot': True}):
            with self.subTest(options=options), patch.object(PUBLIC, 'boot_id') as boot:
                with self.assertRaisesRegex(PUBLIC.ReleaseError, 'unfinished benchmark'), PUBLIC.workload_lock(**options):
                    self.fail('A reboot recovery action cannot bypass benchmark cleanup')
                boot.assert_not_called()
        self.assertEqual(self.runner.calls, [])

    def test_ordinary_release_cannot_bypass_a_prepared_reboot(self):
        self.reboot_state()
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'unfinished release'), PUBLIC.workload_lock():
            self.fail('Only explicit recovery may enter a pending reboot journal')
        self.assertEqual(self.runner.calls, [])

    def test_after_reboot_requires_a_prepared_journal_and_changed_boot_identity(self):
        before = '11111111-1111-1111-1111-111111111111'
        after = '22222222-2222-2222-2222-222222222222'
        self.reboot_state(boot=before)
        with patch.object(PUBLIC, 'boot_id', return_value=after), PUBLIC.workload_lock(after_reboot=True):
            pass
        for phase, boot, current in (
            ('await_reboot', before, before), ('replace_application', before, after),
            ('await_reboot', 'invalid-boot', after),
        ):
            self.reboot_state(phase=phase, boot=boot)
            with self.subTest(phase=phase, boot=boot, current=current), patch.object(PUBLIC, 'boot_id', return_value=current):
                with self.assertRaisesRegex(PUBLIC.ReleaseError, 'unfinished release'), PUBLIC.workload_lock(after_reboot=True):
                    self.fail('Unexpected release journal cannot use the reboot exception')

    def test_cancel_reboot_requires_the_same_boot_and_exclusive_recovery_mode(self):
        before = '11111111-1111-1111-1111-111111111111'
        self.reboot_state(boot=before)
        with patch.object(PUBLIC, 'boot_id', return_value=before), PUBLIC.workload_lock(cancel_reboot=True):
            pass
        with patch.object(PUBLIC, 'boot_id', return_value='22222222-2222-2222-2222-222222222222'):
            with self.assertRaisesRegex(PUBLIC.ReleaseError, 'unfinished release'), PUBLIC.workload_lock(cancel_reboot=True):
                self.fail('Cancellation cannot cross a reboot')
        with self.assertRaisesRegex(PUBLIC.ReleaseError, 'Choose one'), PUBLIC.workload_lock(after_reboot=True, cancel_reboot=True):
            self.fail('Conflicting recovery modes must be rejected')


class PublicRunnerTests(unittest.TestCase):
    """Exercise private command I/O and cleanup with an in-memory child model."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='simplestchat-release-runner-test.')
        self.directory = Path(self.temporary.name)
        self.runner = PUBLIC.Runner(self.directory)

    def tearDown(self):
        self.temporary.cleanup()

    def test_backup_streams_through_files_without_loading_command_output(self):
        source = self.directory / 'input.dump'
        target = self.directory / 'output.dump'
        source.write_bytes(b'fixture database archive')
        child = Mock(returncode=0)
        child.poll.return_value = 0
        captured = {}

        def spawn(_args, **kwargs):
            captured.update(kwargs)
            kwargs['stdout'].write(kwargs['stdin'].read())
            return child

        with patch.object(PUBLIC.subprocess, 'Popen', side_effect=spawn):
            result = self.runner.run(['fixture-no-execution'], input_path=source, output_path=target)
        self.assertEqual(result, b'')
        self.assertEqual(target.read_bytes(), source.read_bytes())
        self.assertTrue(captured['stdin'].closed)
        self.assertTrue(captured['stdout'].closed)
        self.assertEqual(captured['env'], PUBLIC.ENV)
        self.assertTrue(captured['start_new_session'])

    def test_oversized_inspection_is_retained_but_not_returned(self):
        child = Mock(returncode=0)
        child.poll.return_value = 0

        def spawn(_args, **kwargs):
            kwargs['stdout'].write(b'x' * (2 * 1024 * 1024 + 1))
            return child

        with patch.object(PUBLIC.subprocess, 'Popen', side_effect=spawn):
            with self.assertRaisesRegex(PUBLIC.ReleaseError, 'exceeded its bound'):
                self.runner.run(['fixture-no-execution'])
        self.assertEqual((self.directory / '001.stdout').stat().st_size, 2 * 1024 * 1024 + 1)

    def test_timeout_cleanup_falls_back_to_owned_child_without_group_probes(self):
        child = Mock(pid=12345, returncode=None)
        child.communicate.side_effect = subprocess.TimeoutExpired('fixture', 1)
        child.poll.return_value = None
        with patch.object(PUBLIC.subprocess, 'Popen', return_value=child), \
                patch.object(PUBLIC.os, 'killpg', side_effect=PermissionError('fixture')) as group:
            with self.assertRaises(subprocess.TimeoutExpired):
                self.runner.run(['fixture-no-execution'], timeout=1)
        group.assert_called_once_with(12345, signal.SIGTERM)
        child.send_signal.assert_called_once_with(signal.SIGTERM)
        child.wait.assert_called_once_with(timeout=10)


if __name__ == '__main__':
    unittest.main()
