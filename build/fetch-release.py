#!/usr/bin/env python3
"""Fetch a reviewed GitHub release artifact directly onto a prepared public VPS.

GitHub credentials remain on this controller. The installed receiver requests a
short-lived URL over SSH stdin only after its locked preflight is ready. This
command neither stages an image nor deploys/restarts services. Failure evidence
is private and retained; an uncertain remote outcome requires inspection, not an
automatic retry. Python 3.12+, authenticated gh, and OpenSSH are required.
"""

import argparse
from datetime import datetime, timezone
import http.client
import json
import os
from pathlib import Path
import re
import selectors
import signal
import ssl
import stat
import subprocess
import tempfile
import time
from urllib.parse import urlsplit

MAX_ZIP = 2 * 1024**3
MAX_LINE = 65536
RECEIVER = '/usr/local/libexec/simplestchat-public/fetch-release.py'


class FetchError(Exception):
    """Only fixed, non-secret failure codes may cross the CLI boundary."""


def require(condition, code):
    if not condition:
        raise FetchError(code)


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, 'duplicate_json_key')
        value[key] = item
    return value


def decoded(data):
    return json.loads(data, object_pairs_hook=unique_object,
                      parse_constant=lambda _value: (_ for _ in ()).throw(FetchError('invalid_json')))


def positive(value):
    return type(value) is int and 0 < value <= 2**53 - 1


def options(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repository', 'artifact-id', 'revision', 'ci-run', 'host', 'user', 'identity', 'output-parent'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--port', default='22')
    args = parser.parse_args(argv)
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}', args.repository),
            'invalid_repository')
    require(re.fullmatch(r'[a-f0-9]{40}', args.revision), 'invalid_revision')
    for name in ('artifact_id', 'ci_run', 'port'):
        value = getattr(args, name)
        require(re.fullmatch(r'[1-9][0-9]{0,15}', value), 'invalid_numeric_option')
        setattr(args, name, int(value))
        require(positive(getattr(args, name)), 'invalid_numeric_option')
    require(args.port <= 65535, 'invalid_port')
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,252}', args.host), 'invalid_host')
    require(re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', args.user), 'invalid_user')
    identity = Path(args.identity)
    require(identity.is_absolute() and not identity.is_symlink(), 'invalid_identity')
    metadata = identity.stat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid()
            and stat.S_IMODE(metadata.st_mode) in (0o400, 0o600), 'invalid_identity')
    parent = Path(args.output_parent)
    require(parent.is_absolute() and parent.resolve() == parent
            and (parent.is_dir() if parent.exists() else parent.parent.is_dir()), 'invalid_output_parent')
    return args


def gh(arguments):
    result = subprocess.run(['gh', *arguments], stdin=subprocess.DEVNULL, capture_output=True,
                            timeout=20, check=True)
    require(len(result.stdout) <= MAX_LINE, 'github_response_too_large')
    return result.stdout


def api(endpoint):
    return decoded(gh(['api', '--hostname', 'github.com', '-H', 'Accept: application/vnd.github+json',
                       '-H', 'X-GitHub-Api-Version: 2022-11-28', endpoint]))


def verified_envelope(args):
    """Require the exact reviewed commit, successful workflows, and API digest."""
    artifact = api(f'repos/{args.repository}/actions/artifacts/{args.artifact_id}')
    require(isinstance(artifact, dict) and artifact.get('id') == args.artifact_id
            and type(artifact.get('id')) is int and artifact.get('expired') is False
            and artifact.get('name') == f'simplestchat-production-{args.revision}', 'artifact_identity_mismatch')
    size = artifact.get('size_in_bytes')
    digest = artifact.get('digest')
    require(positive(size) and size <= MAX_ZIP and isinstance(digest, str)
            and re.fullmatch(r'sha256:[a-f0-9]{64}', digest), 'artifact_digest_or_size_missing')
    association = artifact.get('workflow_run')
    require(isinstance(association, dict) and positive(association.get('id'))
            and association.get('head_sha') == args.revision
            and positive(association.get('repository_id')) and positive(association.get('head_repository_id'))
            and association.get('repository_id') == association.get('head_repository_id'), 'artifact_run_mismatch')
    build_run = api(f'repos/{args.repository}/actions/runs/{association["id"]}')
    require(isinstance(build_run, dict), 'workflow_identity_or_success_mismatch')
    from_ci = build_run.get('path') == '.github/workflows/ci.yml'
    if from_ci:
        # A normal CI artifact is acceptable only from the exact successful
        # trusted push run supplied as the CI gate, never a PR or a second run.
        require(association['id'] == args.ci_run and build_run.get('head_branch') == 'main',
                'artifact_ci_run_mismatch')
        expected_build = ('CI', '.github/workflows/ci.yml', 'push')
        runs = [(association['id'], build_run, *expected_build)]
    else:
        expected_build = ('Build production release artifact', '.github/workflows/release-artifact.yml', 'workflow_dispatch')
        ci_run = api(f'repos/{args.repository}/actions/runs/{args.ci_run}')
        runs = [(association['id'], build_run, *expected_build),
                (args.ci_run, ci_run, 'CI', '.github/workflows/ci.yml', 'push')]
    for run_id, run, name, path, event in runs:
        require(isinstance(run, dict) and type(run.get('id')) is int and run['id'] == run_id
                and run.get('name') == name and run.get('path') == path and run.get('event') == event
                and run.get('head_sha') == args.revision and run.get('status') == 'completed'
                and run.get('conclusion') == 'success'
                and run.get('repository', {}).get('full_name', '').lower() == args.repository.lower()
                and run.get('head_repository', {}).get('full_name', '').lower() == args.repository.lower(),
                'workflow_identity_or_success_mismatch')
    return {'schemaVersion': 1, 'repository': args.repository, 'artifactId': args.artifact_id,
            'buildRunId': association['id'], 'ciRunId': args.ci_run, 'revision': args.revision,
            'artifactZipBytes': size, 'zipSha256': digest.removeprefix('sha256:')}


def storage_url(value):
    require(isinstance(value, str) and 0 < len(value) <= 12288
            and all(32 < ord(char) < 127 for char in value), 'download_url_rejected')
    parsed = urlsplit(value)
    require(parsed.scheme == 'https' and re.fullmatch(r'[a-z0-9][a-z0-9-]{0,62}\.blob\.core\.windows\.net', parsed.netloc)
            and parsed.path.startswith('/') and parsed.query and not parsed.fragment, 'download_url_rejected')
    return value


def download_url(args):
    """Never follow an authenticated redirect or place credentials in argv."""
    token = gh(['auth', 'token', '--hostname', 'github.com']).decode().strip()
    require(0 < len(token) <= 4096 and all(32 < ord(char) < 127 for char in token), 'github_authentication_failed')
    connection = http.client.HTTPSConnection('api.github.com', timeout=20, context=ssl.create_default_context())
    try:
        connection.request('GET', f'/repos/{args.repository}/actions/artifacts/{args.artifact_id}/zip', headers={
            'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'simplestchat-release-fetch',
        })
        response = connection.getresponse()
        locations = [value for name, value in response.getheaders() if name.lower() == 'location']
        require(response.status == 302 and len(locations) == 1, 'github_download_redirect_missing')
        return storage_url(locations[0])
    finally:
        token = None
        connection.close()


def ssh_environment():
    # No GitHub/API tokens enter SSH, including through configured SendEnv rules.
    environment = {key: os.environ[key] for key in ('PATH', 'HOME', 'USER', 'LOGNAME', 'SSH_AUTH_SOCK') if key in os.environ}
    return dict(environment, LC_ALL='C')


def ssh_arguments(args):
    command = ['/usr/bin/python3', '-B', RECEIVER]
    if args.user != 'root':
        command = ['sudo', '-n', '--', *command]
    settings = ('BatchMode=yes', 'StrictHostKeyChecking=yes', 'ConnectTimeout=15',
                'ControlMaster=no', 'ControlPath=none', 'ForwardAgent=no', 'ForwardX11=no',
                'ClearAllForwardings=yes', 'PermitLocalCommand=no', 'ProxyCommand=none', 'ProxyJump=none',
                'IdentitiesOnly=yes', 'PasswordAuthentication=no', 'KbdInteractiveAuthentication=no',
                'SendEnv=-*', 'ServerAliveInterval=10', 'ServerAliveCountMax=3')
    return ['ssh', *[item for value in settings for item in ('-o', value)],
            '-i', args.identity, '-p', str(args.port), '-l', args.user, args.host, ' '.join(command)]


class Receiver:
    """Bound the two-message SSH protocol; never print raw remote output."""

    def __init__(self, args):
        self.process = subprocess.Popen(ssh_arguments(args), env=ssh_environment(), stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.buffer = b''
        self.bytes = 0

    def send(self, value):
        data = (json.dumps(value, separators=(',', ':')) + '\n').encode()
        require(len(data) <= 16384, 'request_too_large')
        self.process.stdin.write(data)
        self.process.stdin.flush()

    def receive(self, seconds):
        deadline = time.monotonic() + seconds
        while b'\n' not in self.buffer:
            remaining = deadline - time.monotonic()
            require(remaining > 0 and self.selector.select(remaining), 'receiver_response_timeout')
            chunk = os.read(self.process.stdout.fileno(), 4096)
            require(chunk, 'receiver_closed_without_receipt')
            self.bytes += len(chunk)
            require(self.bytes <= MAX_LINE, 'receiver_response_too_large')
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b'\n', 1)
        return decoded(line)

    def finish(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=15)
        # Wait for EOF as well as exit: a child must not retain the output pipe.
        deadline = time.monotonic() + 5
        while True:
            remaining = deadline - time.monotonic()
            require(remaining > 0 and self.selector.select(remaining), 'receiver_output_not_closed')
            chunk = os.read(self.process.stdout.fileno(), 4096)
            if not chunk:
                break
            self.buffer += chunk
            require(len(self.buffer) <= MAX_LINE, 'receiver_response_too_large')
        require(not self.buffer.strip(), 'receiver_extra_output')
        return code

    def close(self):
        self.selector.close()
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=5)
        try:
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        finally:
            self.process.stdout.close()


def verified_receipt(value, envelope):
    require(isinstance(value, dict) and type(value.get('schemaVersion')) is int and value['schemaVersion'] == 1
            and value.get('status') in ('complete', 'failed')
            and all(type(value.get(key)) is bool for key in ('passed', 'settled', 'finalized')),
            'receiver_receipt_invalid')
    for key in ('revision', 'artifactId', 'zipSha256', 'artifactZipBytes'):
        require(value.get(key) == envelope[key] and type(value.get(key)) is type(envelope[key]), 'receiver_identity_mismatch')
    evidence = value.get('evidence')
    if evidence is None:
        require(not value['passed'] and not value['settled'] and not value['finalized']
                and value.get('phase') == 'preflight' and value.get('downloadedBytes') == 0
                and value.get('archiveSha256') is None and value.get('manifestSha256') is None,
                'receiver_evidence_invalid')
    else:
        require(isinstance(evidence, str) and re.fullmatch(
            rf'/srv/simplestchat-public/releases/{envelope["revision"]}/download\.[A-Za-z0-9_-]+', evidence), 'receiver_evidence_invalid')
    elapsed = value.get('elapsedSeconds')
    require(type(elapsed) in (int, float) and 0 <= elapsed <= 330, 'receiver_timing_invalid')
    require(type(value.get('downloadedBytes')) is int and 0 <= value['downloadedBytes'] <= envelope['artifactZipBytes'],
            'receiver_byte_count_invalid')
    for key in ('archiveSha256', 'manifestSha256'):
        require(value.get(key) is None or isinstance(value[key], str) and re.fullmatch(r'[a-f0-9]{64}', value[key]),
                'receiver_digest_invalid')
    require(isinstance(value.get('phase'), str) and re.fullmatch(r'[a-z_]{1,40}', value['phase']), 'receiver_phase_invalid')
    failure = value.get('failureClass')
    require(failure is None or isinstance(failure, str) and re.fullmatch(r'[a-z_]{1,80}', failure), 'receiver_failure_invalid')
    if value['passed']:
        require(value['status'] == 'complete' and value['settled'] and value['finalized'] and failure is None
                and value['phase'] == 'complete' and value['downloadedBytes'] == envelope['artifactZipBytes']
                and value['archiveSha256'] is not None and value['manifestSha256'] is not None,
                'receiver_success_inconsistent')
    else:
        require(value['status'] == 'failed' and failure is not None, 'receiver_failure_inconsistent')
    return {key: value[key] for key in ('schemaVersion', 'status', 'passed', 'settled', 'phase', 'failureClass',
            'evidence', 'revision', 'artifactId', 'zipSha256', 'artifactZipBytes', 'downloadedBytes',
            'archiveSha256', 'manifestSha256', 'elapsedSeconds', 'finalized')}


def save(path, value):
    with path.open('x', encoding='utf-8') as output:
        json.dump(value, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())


def main(argv=None):
    try:
        args = options(argv)
    except SystemExit as error:
        return error.code
    except (FetchError, OSError, ValueError):
        print(json.dumps({'passed': False, 'phase': 'options', 'failureClass': 'invalid_options'}))
        return 1
    began = time.monotonic()
    directory = None
    receiver = None
    report = {'schemaVersion': 1, 'operation': 'fetch_release_controller', 'passed': False,
              'phase': 'options', 'remoteSettled': False, 'remote': None, 'failureClass': None,
              'startedAt': datetime.now(timezone.utc).isoformat()}

    def interrupted(signum, _frame):
        raise FetchError('deadline_exceeded' if signum == signal.SIGALRM else 'interrupted')

    previous = {signum: signal.signal(signum, interrupted) for signum in (signal.SIGALRM, signal.SIGINT, signal.SIGTERM)}
    signal.alarm(420)
    previous_umask = os.umask(0o077)
    try:
        Path(args.output_parent).mkdir(mode=0o700, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix='release-fetch.', dir=args.output_parent))
        report['evidence'] = str(directory)
        report['phase'] = 'github_metadata'
        envelope = verified_envelope(args)
        save(directory / 'artifact.json', envelope)
        report['phase'] = 'receiver_preflight'
        receiver = Receiver(args)
        receiver.send(envelope)
        first = receiver.receive(45)
        if first == {'schemaVersion': 1, 'status': 'ready'} and type(first['schemaVersion']) is int:
            report['phase'] = 'download_link'
            receiver.send({'url': download_url(args)})
            report['phase'] = 'remote_fetch'
            final = receiver.receive(315)
        else:
            final = first
        report['remote'] = verified_receipt(final, envelope)
        code = receiver.finish()
        report['remoteSettled'] = report['remote']['settled']
        require(code == 0 and report['remote']['passed'] and report['remoteSettled'], 'remote_fetch_failed')
        report['phase'] = 'complete'
        report['passed'] = True
    except BaseException as error:
        report['passed'] = False
        report['failureClass'] = error.args[0] if isinstance(error, FetchError) else 'controller_operation_failed'
    finally:
        if receiver is not None:
            try:
                receiver.close()
            except BaseException:
                report.update(passed=False, failureClass='controller_cleanup_uncertain')
        report['elapsedSeconds'] = round(time.monotonic() - began, 3)
        try:
            if directory is not None:
                save(directory / 'outcome.json', report)
                for path in (directory, directory.parent):
                    descriptor = os.open(path, os.O_RDONLY)
                    try:
                        os.fsync(descriptor)
                    finally:
                        os.close(descriptor)
        except BaseException:
            report.update(passed=False, failureClass='controller_receipt_write_failed')
        signal.alarm(0)
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        os.umask(previous_umask)
        print(json.dumps(report), flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
