"""Offline controller identity, credential isolation and bounded protocol checks."""

from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

PROJECT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('release_fetch_controller_tests', PROJECT / 'build/fetch-release.py')
FETCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FETCH)
REVISION = 'a' * 40
TOKEN = 'PRIVATE_GITHUB_TOKEN_SENTINEL'
URL = 'https://productionresultssa6.blob.core.windows.net/path/artifact.zip?sig=PRIVATE_URL_SENTINEL'


def envelope():
    return {'schemaVersion': 1, 'repository': 'owner/repo', 'artifactId': 123, 'buildRunId': 456,
            'ciRunId': 789, 'revision': REVISION, 'artifactZipBytes': 12345, 'zipSha256': 'b' * 64}


def receipt(**changes):
    return dict({'schemaVersion': 1, 'status': 'complete', 'passed': True, 'settled': True,
                 'finalized': True, 'phase': 'complete', 'failureClass': None,
                 'evidence': f'/srv/simplestchat-public/releases/{REVISION}/download.fixture',
                 'revision': REVISION, 'artifactId': 123, 'zipSha256': 'b' * 64,
                 'artifactZipBytes': 12345, 'downloadedBytes': 12345,
                 'archiveSha256': 'c' * 64, 'manifestSha256': 'd' * 64, 'elapsedSeconds': 1.25}, **changes)


def api_records():
    artifact = {'id': 123, 'expired': False, 'name': f'simplestchat-production-{REVISION}',
                'size_in_bytes': 12345, 'digest': 'sha256:' + 'b' * 64,
                'workflow_run': {'id': 456, 'head_sha': REVISION, 'repository_id': 42, 'head_repository_id': 42}}
    common = {'head_sha': REVISION, 'status': 'completed', 'conclusion': 'success',
              'repository': {'full_name': 'owner/repo'}, 'head_repository': {'full_name': 'owner/repo'}}
    return [artifact,
            dict(common, id=456, name='Build production release artifact',
                 path='.github/workflows/release-artifact.yml', event='workflow_dispatch'),
            dict(common, id=789, name='CI', path='.github/workflows/ci.yml', event='push')]


class ControllerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='simplestchat-fetch-controller.')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.identity = self.root / 'private key'
        self.identity.write_text('disposable nonfunctional key fixture\n')
        self.identity.chmod(0o600)
        self.argv = ['--repository', 'owner/repo', '--artifact-id', '123', '--revision', REVISION,
                     '--ci-run', '789', '--host', 'chat.example.test', '--user', 'root',
                     '--identity', str(self.identity), '--output-parent', str(self.root)]
        self.args = FETCH.options(self.argv)

    def run_main(self, receiver, *, url=URL):
        output = io.StringIO()
        with patch.object(FETCH, 'verified_envelope', return_value=envelope()), \
             patch.object(FETCH, 'Receiver', return_value=receiver) as create, \
             patch.object(FETCH, 'download_url', return_value=url) as lookup, redirect_stdout(output):
            code = FETCH.main(self.argv)
        report = json.loads(output.getvalue())
        create.assert_called_once()
        receiver.close.assert_called_once()
        return code, report, lookup

    def test_options_reject_unsafe_destinations_and_require_a_private_owned_regular_key(self):
        for option, value in (('--repository', '../repo'), ('--repository', 'owner/.repo'),
                              ('--revision', 'main'), ('--artifact-id', '0'), ('--ci-run', '01'),
                              ('--port', '65536'), ('--port', '1.5'), ('--host', '2001:db8::1'),
                              ('--host', '-oProxyCommand=command'), ('--user', 'root;command'),
                              ('--identity', '~/.ssh/key'), ('--output-parent', str(self.root / '..' / 'results'))):
            arguments = list(self.argv)
            if option in arguments:
                arguments[arguments.index(option) + 1] = value
            else:
                arguments.extend((option, value))
            with self.subTest(option=option, value=value), redirect_stderr(io.StringIO()), \
                 self.assertRaises((FETCH.FetchError, SystemExit)):
                FETCH.options(arguments)
        for mode in (0o644, 0o666, 0o700):
            self.identity.chmod(mode)
            with self.subTest(mode=mode), self.assertRaises(FETCH.FetchError):
                FETCH.options(self.argv)
        self.identity.chmod(0o400)
        self.assertEqual(FETCH.options(self.argv).identity, str(self.identity))
        link = self.root / 'key-link'
        link.symlink_to(self.identity)
        arguments = list(self.argv)
        arguments[arguments.index('--identity') + 1] = str(link)
        with self.assertRaises(FETCH.FetchError):
            FETCH.options(arguments)

    def test_help_is_successful_without_network_or_evidence(self):
        output = io.StringIO()
        with patch.object(FETCH, 'gh') as github, patch.object(FETCH, 'Receiver') as remote, redirect_stdout(output):
            self.assertEqual(FETCH.main(['--help']), 0)
        github.assert_not_called()
        remote.assert_not_called()
        self.assertIn('usage:', output.getvalue())
        self.assertEqual(list(self.root.iterdir()), [self.identity])

    def test_exact_artifact_build_and_ci_run_are_required(self):
        with patch.object(FETCH, 'api', side_effect=api_records()) as api:
            self.assertEqual(FETCH.verified_envelope(self.args), envelope())
        self.assertEqual([call.args[0] for call in api.call_args_list], [
            'repos/owner/repo/actions/artifacts/123', 'repos/owner/repo/actions/runs/456',
            'repos/owner/repo/actions/runs/789'])
        cases = [(0, ('id',), True), (0, ('expired',), True), (0, ('name',), 'other'),
                 (0, ('size_in_bytes',), True), (0, ('size_in_bytes',), FETCH.MAX_ZIP + 1),
                 (0, ('digest',), None), (0, ('workflow_run', 'head_sha'), 'e' * 40),
                 (0, ('workflow_run', 'id'), True), (0, ('workflow_run', 'head_repository_id'), True)]
        for index in (1, 2):
            cases.extend((index, (key,), value) for key, value in (
                ('id', 1), ('name', 'other'), ('path', '.github/workflows/other.yml'),
                ('event', 'pull_request'), ('head_sha', 'e' * 40), ('status', 'in_progress'), ('conclusion', 'failure')))
            cases.extend((index, (key, 'full_name'), 'fork/repo') for key in ('repository', 'head_repository'))
        for index, path, value in cases:
            records = deepcopy(api_records())
            target = records[index]
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.subTest(index=index, path=path, value=value), \
                 patch.object(FETCH, 'api', side_effect=records), self.assertRaises(FETCH.FetchError):
                FETCH.verified_envelope(self.args)

    def test_api_is_github_com_only_and_rejects_ambiguous_json(self):
        with patch.object(FETCH, 'gh', return_value=b'{"id":123}') as github:
            self.assertEqual(FETCH.api('repos/owner/repo/actions/artifacts/123'), {'id': 123})
        arguments = github.call_args.args[0]
        self.assertEqual(arguments[:3], ['api', '--hostname', 'github.com'])
        for data in (b'{"id":1,"id":2}', b'{"value":NaN}', b'{"value":Infinity}'):
            with self.subTest(data=data), self.assertRaises(FETCH.FetchError):
                FETCH.decoded(data)
        with patch.object(FETCH.subprocess, 'run', return_value=Mock(stdout=b'x' * (FETCH.MAX_LINE + 1))):
            with self.assertRaisesRegex(FETCH.FetchError, 'github_response_too_large'):
                FETCH.gh(['api', 'fixture'])

    def test_storage_urls_require_one_trusted_https_endpoint_without_credentials_or_fragments(self):
        self.assertEqual(FETCH.storage_url(URL), URL)
        for value in (URL.replace('https:', 'http:'), URL.replace('.net/', '.net.evil.test/'),
                      URL.replace('https://', 'https://user:password@'), URL.replace('.net/', '.net:443/'),
                      URL + '#fragment', URL.replace('?sig=PRIVATE_URL_SENTINEL', ''), URL + '\n',
                      URL.replace('productionresultssa6.blob.core.windows.net', '127.0.0.1'),
                      'file:///tmp/archive.zip', 'x' * 12289):
            with self.subTest(value=value), self.assertRaises(FETCH.FetchError):
                FETCH.storage_url(value)

    def test_token_stays_in_api_memory_and_authenticated_redirect_is_not_followed(self):
        response = Mock(status=302)
        response.getheaders.return_value = [('Location', URL)]
        connection = Mock()
        connection.getresponse.return_value = response
        with patch.object(FETCH, 'gh', return_value=(TOKEN + '\n').encode()) as github, \
             patch.object(FETCH.http.client, 'HTTPSConnection', return_value=connection) as connect:
            self.assertEqual(FETCH.download_url(self.args), URL)
        github.assert_called_once_with(['auth', 'token', '--hostname', 'github.com'])
        self.assertEqual(connect.call_args.args, ('api.github.com',))
        self.assertEqual(connection.request.call_args.args,
                         ('GET', '/repos/owner/repo/actions/artifacts/123/zip'))
        self.assertEqual(connection.request.call_args.kwargs['headers']['Authorization'], 'Bearer ' + TOKEN)
        connection.request.assert_called_once()
        connection.close.assert_called_once()
        for status, headers in ((200, [('Location', URL)]), (307, [('Location', URL)]),
                                (302, []), (302, [('Location', URL), ('location', URL)])):
            response.status, response.getheaders.return_value = status, headers
            with self.subTest(status=status, headers=headers), patch.object(FETCH, 'gh', return_value=TOKEN.encode()), \
                 patch.object(FETCH.http.client, 'HTTPSConnection', return_value=connection), self.assertRaises(FETCH.FetchError):
                FETCH.download_url(self.args)

    def test_ssh_argv_and_environment_exclude_credentials_and_disable_unsupported_options(self):
        with patch.dict(os.environ, {'GH_TOKEN': TOKEN, 'GITHUB_TOKEN': TOKEN, 'PRIVATE_URL': URL}):
            environment = FETCH.ssh_environment()
            arguments = FETCH.ssh_arguments(self.args)
        combined = json.dumps([arguments, environment])
        self.assertNotIn(TOKEN, combined)
        self.assertNotIn(URL, combined)
        self.assertNotIn('GH_TOKEN', environment)
        self.assertEqual(set(environment) - {'PATH', 'HOME', 'USER', 'LOGNAME', 'SSH_AUTH_SOCK', 'LC_ALL'}, set())
        for setting in ('StrictHostKeyChecking=yes', 'ForwardAgent=no', 'SendEnv=-*', 'ProxyCommand=none',
                        'ProxyJump=none', 'PermitLocalCommand=no', 'ClearAllForwardings=yes',
                        'ControlMaster=no', 'ControlPath=none', 'PasswordAuthentication=no'):
            self.assertIn(setting, arguments)
        self.assertEqual(arguments[-1], '/usr/bin/python3 -B ' + FETCH.RECEIVER)
        self.args.user = 'deploy'
        self.assertEqual(FETCH.ssh_arguments(self.args)[-1], 'sudo -n -- /usr/bin/python3 -B ' + FETCH.RECEIVER)

    def test_receipt_projects_only_valid_identity_bound_fields(self):
        self.assertEqual(FETCH.verified_receipt(receipt(secret=TOKEN, url=URL), envelope()), receipt())
        for key, value in (('schemaVersion', True), ('artifactId', True), ('revision', 'e' * 40),
                           ('zipSha256', 'e' * 64), ('artifactZipBytes', 1), ('passed', 1),
                           ('evidence', '/tmp/download.fixture'), ('evidence', None),
                           ('elapsedSeconds', float('nan')), ('elapsedSeconds', 331),
                           ('downloadedBytes', True), ('downloadedBytes', 12344),
                           ('archiveSha256', None), ('manifestSha256', URL),
                           ('phase', 'download'), ('settled', False), ('finalized', False), ('failureClass', 'error')):
            with self.subTest(key=key, value=value), self.assertRaises(FETCH.FetchError):
                FETCH.verified_receipt(receipt(**{key: value}), envelope())
        early = receipt(status='failed', passed=False, settled=False, finalized=False, phase='preflight',
                        failureClass='unfinished_operation', evidence=None, downloadedBytes=0,
                        archiveSha256=None, manifestSha256=None)
        self.assertEqual(FETCH.verified_receipt(early, envelope()), early)

    def test_ready_precedes_url_and_success_evidence_never_contains_secrets(self):
        events = []
        remote = Mock()
        messages = iter([{'schemaVersion': 1, 'status': 'ready'}, receipt(secret=TOKEN, url=URL)])
        remote.send.side_effect = lambda value: events.append(('send', value))
        remote.receive.side_effect = lambda _seconds: (events.append(('receive', None)), next(messages))[1]
        remote.finish.return_value = 0
        with patch.object(FETCH, 'verified_envelope', return_value=envelope()), \
             patch.object(FETCH, 'Receiver', return_value=remote), \
             patch.object(FETCH, 'download_url', side_effect=lambda _args: (events.append(('url', None)), URL)[1]), \
             redirect_stdout(io.StringIO()) as output:
            self.assertEqual(FETCH.main(self.argv), 0)
        self.assertEqual([event[0] for event in events], ['send', 'receive', 'url', 'send', 'receive'])
        self.assertEqual(events[0][1], envelope())
        self.assertEqual(events[3][1], {'url': URL})
        report = json.loads(output.getvalue())
        evidence = Path(report['evidence'])
        self.assertEqual(evidence.parent, self.root)
        self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
        for path in evidence.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertNotIn(TOKEN, path.read_text())
            self.assertNotIn(URL, path.read_text())
        self.assertNotIn(TOKEN, output.getvalue())
        self.assertNotIn(URL, output.getvalue())
        self.assertEqual(json.loads((evidence / 'artifact.json').read_text()), envelope())
        self.assertEqual(json.loads((evidence / 'outcome.json').read_text()), report)
        self.assertTrue(report['passed'] and report['remoteSettled'])
        self.assertEqual([call.args[0] for call in remote.receive.call_args_list], [45, 315])

    def test_failed_preflight_never_requests_url_and_is_not_retried(self):
        remote = Mock()
        remote.receive.return_value = receipt(status='failed', passed=False, settled=False, finalized=False,
            phase='preflight', failureClass='unfinished_operation', evidence=None, downloadedBytes=0,
            archiveSha256=None, manifestSha256=None)
        remote.finish.return_value = 1
        code, report, lookup = self.run_main(remote)
        self.assertEqual(code, 1)
        self.assertFalse(report['passed'] or report['remoteSettled'])
        self.assertEqual(report['remote']['failureClass'], 'unfinished_operation')
        lookup.assert_not_called()
        remote.send.assert_called_once_with(envelope())

    def test_failed_github_gate_never_starts_ssh_or_requests_a_url(self):
        with patch.object(FETCH, 'verified_envelope', side_effect=OSError(TOKEN + URL)), \
             patch.object(FETCH, 'Receiver') as remote, patch.object(FETCH, 'download_url') as lookup, \
             redirect_stdout(io.StringIO()) as output:
            self.assertEqual(FETCH.main(self.argv), 1)
        remote.assert_not_called()
        lookup.assert_not_called()
        report = json.loads(output.getvalue())
        self.assertFalse(report['passed'] or report['remoteSettled'])
        self.assertNotIn(TOKEN, output.getvalue())
        self.assertNotIn(URL, output.getvalue())
        self.assertEqual({path.name for path in Path(report['evidence']).iterdir()}, {'outcome.json'})

    def test_nonzero_exit_cannot_turn_a_passed_looking_receipt_into_success(self):
        remote = Mock()
        remote.receive.side_effect = [{'schemaVersion': 1, 'status': 'ready'}, receipt()]
        remote.finish.return_value = 42
        code, report, lookup = self.run_main(remote)
        self.assertEqual(code, 1)
        self.assertFalse(report['passed'])
        self.assertEqual(report['failureClass'], 'remote_fetch_failed')
        lookup.assert_called_once()

    def test_missing_receipt_or_unconfirmed_exit_never_claims_remote_settlement(self):
        for failure_at in ('receive', 'finish'):
            remote = Mock()
            remote.receive.side_effect = [{'schemaVersion': 1, 'status': 'ready'}, receipt()]
            if failure_at == 'receive':
                remote.receive.side_effect = [{'schemaVersion': 1, 'status': 'ready'}, OSError(TOKEN + URL)]
            else:
                remote.finish.side_effect = subprocess.TimeoutExpired('ssh', 15, stderr=(TOKEN + URL).encode())
            with self.subTest(failure_at=failure_at):
                code, report, lookup = self.run_main(remote)
                self.assertEqual(code, 1)
                self.assertFalse(report['passed'] or report['remoteSettled'])
                self.assertNotIn(TOKEN, json.dumps(report))
                self.assertNotIn(URL, json.dumps(report))
                lookup.assert_called_once()

    def test_real_local_subprocess_exercises_line_protocol_exit_and_extra_output(self):
        # This replaces SSH with an owned Python child: no network or daemon.
        source = '''import json, sys
json.loads(sys.stdin.readline())
print(json.dumps({"schemaVersion": 1, "status": "ready"}), flush=True)
assert set(json.loads(sys.stdin.readline())) == {"url"}
print(sys.argv[1], flush=True)
if sys.argv[2] == "extra": print("unexpected extra output", flush=True)
sys.exit(42 if sys.argv[2] == "failed" else 0)
'''
        for mode in ('complete', 'failed', 'extra'):
            with self.subTest(mode=mode), patch.object(FETCH, 'ssh_arguments', return_value=[
                sys.executable, '-u', '-c', source, json.dumps(receipt()), mode,
            ]):
                remote = FETCH.Receiver(self.args)
                try:
                    remote.send(envelope())
                    self.assertEqual(remote.receive(3), {'schemaVersion': 1, 'status': 'ready'})
                    remote.send({'url': URL})
                    self.assertEqual(remote.receive(3), receipt())
                    if mode == 'extra':
                        with self.assertRaisesRegex(FETCH.FetchError, 'receiver_extra_output'):
                            remote.finish()
                    else:
                        self.assertEqual(remote.finish(), 42 if mode == 'failed' else 0)
                finally:
                    remote.close()

    def test_real_local_subprocess_eof_and_truncated_receipts_fail_closed(self):
        for message in ('', '{"schemaVersion":1', 'not-json\n'):
            source = 'import sys; sys.stdin.readline(); sys.stdout.write(sys.argv[1]); sys.stdout.flush()'
            with self.subTest(message=message), patch.object(FETCH, 'ssh_arguments', return_value=[
                sys.executable, '-u', '-c', source, message,
            ]):
                remote = FETCH.Receiver(self.args)
                try:
                    self.assertEqual(os.getpgid(remote.process.pid), remote.process.pid)
                    remote.send(envelope())
                    with self.assertRaises((FETCH.FetchError, json.JSONDecodeError)):
                        remote.receive(3)
                finally:
                    remote.close()
                self.assertIsNotNone(remote.process.poll())


if __name__ == '__main__':
    unittest.main()
