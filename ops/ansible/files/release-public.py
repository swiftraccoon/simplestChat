#!/usr/bin/env python3
"""Stage a trusted prebuilt release or replace only the public application.

No builds, pulls, migrations, account seeding, database restoration, or host
maintenance occur here. Persistent evidence and a workload lock make partial
updates visible. A failed candidate gets one bounded image/config rollback;
successful recovery never converts a failed release into a passed release.
"""

import argparse
from contextlib import contextmanager
from copy import deepcopy
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

from release_artifact import ArtifactError, sha256_file, validate_manifest, verify_archive

ROOT = Path('/srv/simplestchat-public')
CONFIG = Path('/etc/simplestchat-public')
WORK = Path('/run/simplestchat-bench')
ENV = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C'}
DOCKER = ['/usr/bin/docker', '--host', 'unix:///var/run/docker.sock']
SELECTION = ('compose.public.yml', 'app.env', 'images.json')
ID = re.compile(r'sha256:[a-f0-9]{64}')


class ReleaseError(RuntimeError):
    """A bounded release operation failed; inspect its private evidence."""


def timestamp():
    return datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def protected(path, *, directory=False, modes=(0o600,), limit=None):
    metadata = path.lstat()
    require(metadata.st_uid == 0 and stat.S_IMODE(metadata.st_mode) in modes,
            f'Unexpected ownership or permissions: {path}')
    require(stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode),
            f'Unexpected file type: {path}')
    if limit is not None:
        require(metadata.st_size <= limit, f'Oversized configuration: {path}')


def atomic(path, data):
    """Replace one private regular file durably; the journal covers the set."""
    if isinstance(data, dict):
        data = json.dumps(data, indent=2) + '\n'
    if isinstance(data, str):
        data = data.encode()
    descriptor, temporary = tempfile.mkstemp(prefix='.release-', dir=path.parent)
    with os.fdopen(descriptor, 'wb') as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


class Runner:
    """Bound commands and retain output privately without printing secrets."""

    def __init__(self, attempt):
        self.attempt = attempt
        self.number = 0

    def run(self, args, *, timeout=30, input_data=None, input_path=None, output_path=None):
        self.number += 1
        prefix = self.attempt / f'{self.number:03d}'
        target = output_path or prefix.with_suffix('.stdout')
        source = input_path.open('rb') if input_path else None
        process = None
        try:
            with target.open('xb') as output, prefix.with_suffix('.stderr').open('xb') as error:
                process = subprocess.Popen(args, stdin=source or (subprocess.PIPE if input_data is not None else subprocess.DEVNULL),
                                           stdout=output, stderr=error, env=ENV, start_new_session=True)
                process.communicate(input_data, timeout=timeout)
        finally:
            if source:
                source.close()
            if process is not None and process.poll() is None:
                self.stop(process)
        require(process.returncode == 0, f'Command {self.number:03d} failed; inspect private output')
        if output_path:
            return b''
        require(target.stat().st_size <= 2 * 1024 * 1024, 'Command inspection output exceeded its bound')
        return target.read_bytes()

    @staticmethod
    def stop(process):
        def send(signum):
            try:
                os.killpg(process.pid, signum)
            except (ProcessLookupError, PermissionError):
                process.send_signal(signum)
        send(signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            send(signal.SIGKILL)
            process.wait(timeout=5)

    def docker(self, *args, **kwargs):
        return self.run([*DOCKER, *args], **kwargs)

    def compose(self, *args, filename=None, envfile=None, **kwargs):
        return self.docker('compose', '--project-name', 'simplestchat-public', '--project-directory', str(CONFIG),
                           '--env-file', str(envfile or CONFIG / 'app.env'), '-f', str(filename or CONFIG / 'compose.public.yml'),
                           *args, **kwargs)

    def container(self, service):
        value = self.compose('ps', '--status', 'running', '--quiet', service).decode().strip()
        require(re.fullmatch(r'[a-f0-9]{64}', value), f'Exactly one running {service} container is required')
        result = json.loads(self.docker('inspect', '--format',
            '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"restarts":{{.RestartCount}},'
            '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}', value))
        require(result['state']['Running'] and not result['state']['OOMKilled'], f'{service} is not running normally')
        return result


def stable_container(value):
    return value['id'], value['image'], value['state']['StartedAt'], value['restarts']


def boot_id():
    value = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    require(re.fullmatch(r'[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}', value), 'Invalid host boot identity')
    return value


@contextmanager
def workload_lock(*, after_reboot=False, cancel_reboot=False):
    require(not (after_reboot and cancel_reboot), 'Choose one reboot recovery action')
    protected(ROOT, directory=True, modes=(0o700,))
    protected(CONFIG, directory=True, modes=(0o700,))
    WORK.mkdir(mode=0o700, exist_ok=True)
    protected(WORK, directory=True, modes=(0o700,))
    try:
        descriptor = os.open(WORK / 'workload.lock', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(descriptor)
    except FileExistsError:
        pass
    protected(WORK / 'workload.lock')
    with (WORK / 'workload.lock').open('r+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        current = WORK / 'current.json'
        if current.exists() or current.is_symlink():
            protected(current, limit=16384)
            record = json.loads(current.read_text())
            require(type(record.get('schemaVersion')) is int and record['schemaVersion'] == 1 and record.get('finalized') is True,
                    'Resolve unfinished benchmark cleanup before releasing')
        state = ROOT / 'release-state.json'
        if state.exists() or state.is_symlink():
            protected(state, limit=16384)
            record = json.loads(state.read_text())
            prepared = record.get('phase') == 'await_reboot' and isinstance(record.get('bootId'), str) \
                and re.fullmatch(r'[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}', record['bootId'])
            resumed_boot = after_reboot and prepared and record['bootId'] != boot_id()
            cancelled_boot = cancel_reboot and prepared and record['bootId'] == boot_id()
            require(type(record.get('schemaVersion')) is int and record['schemaVersion'] == 1
                    and (record.get('finalized') is True or resumed_boot or cancelled_boot),
                    'Inspect and recover the unfinished release before starting another')
        yield


def image_identity(runner, selector, revision):
    value = json.loads(runner.docker('image', 'inspect', '--format',
        '{"id":{{json .Id}},"os":{{json .Os}},"architecture":{{json .Architecture}},"user":{{json .Config.User}},'
        '"labels":{{json .Config.Labels}},"cmd":{{json .Config.Cmd}},"entrypoint":{{json .Config.Entrypoint}}}', selector))
    require(isinstance(value['id'], str) and ID.fullmatch(value['id']), 'Missing local content-addressed image ID')
    require(value['os'] == 'linux' and value['architecture'] == 'amd64' and value['user'] == '10001:10001',
            'Unexpected image platform or runtime user')
    require(value['labels'].get('org.opencontainers.image.revision') == revision, 'Image revision mismatch')
    require(value['cmd'] == ['/app/simplestChat'] and value['entrypoint'] in (None, []), 'Unexpected image entrypoint')
    return value['id']


def packaged_migrations(runner, image):
    """Execute only the fixed checksum command in a bounded, isolated container."""
    name = 'scpub-release-validate-' + uuid.uuid4().hex
    journal(runner, False, 'validate_image')
    atomic(runner.attempt / 'validation-name.txt', name)
    container = runner.docker('create', '--name', name, '--network', 'none', '--pull', 'never',
        '--read-only', '--user', '10001:10001', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--memory', '128m', '--cpus', '0.5', '--pids-limit', '32', '--log-driver', 'local',
        '--log-opt', 'max-size=1m', '--log-opt', 'max-file=1', '--entrypoint', '/usr/bin/timeout', image,
        '--signal=TERM', '--kill-after=2s', '10s', '/bin/sh',
        '-c', 'for file in /app/migrations/*.sql; do sha384sum "$file" || exit; done').decode().strip()
    require(re.fullmatch(r'[a-f0-9]{64}', container), 'Uncertain validation container creation')
    runner.docker('start', container)
    require(runner.docker('wait', container, timeout=20).strip() == b'0', 'Packaged migration validation failed')
    lines = runner.docker('logs', container).decode().splitlines()
    result = {}
    for line in lines:
        match = re.fullmatch(r'([a-f0-9]{96})  /app/migrations/([0-9]+)_[A-Za-z0-9_]+\.sql', line)
        require(match is not None, 'Unexpected packaged migration output')
        version = str(int(match[2]))
        require(version not in result, 'Duplicate packaged migration version')
        result[version] = match[1]
    require(result, 'No packaged migrations')
    runner.docker('rm', container)
    journal(runner, True, 'image_validated')
    return result


def journal(runner, finalized, phase):
    atomic(ROOT / 'release-state.json', {'schemaVersion': 1, 'attempt': str(runner.attempt),
                                      'finalized': finalized, 'phase': phase})


def stage(runner, directory, manifest):
    verify_archive(directory / 'image.tar', manifest)
    selected = directory / 'staged.json'
    if selected.exists() or selected.is_symlink():
        protected(selected, limit=16384)
        prior = json.loads(selected.read_text())
        require(prior['manifestSha256'] == sha256_file(directory / 'release.json'), 'Staged release changed')
        require(image_identity(runner, prior['serverImage'], manifest['revision']) == prior['serverImage'], 'Staged image missing')
        return prior
    require(shutil.disk_usage(ROOT).free > (directory / 'image.tar').stat().st_size * 2 + 1024**3,
            'Insufficient free space to import image with safe headroom')
    journal(runner, False, 'import_image')
    runner.docker('image', 'load', '--input', str(directory / 'image.tar'), timeout=300)
    image = image_identity(runner, manifest['imageTag'], manifest['revision'])
    require(packaged_migrations(runner, image) == manifest['migrations'], 'Archive migration manifest differs from image')
    record = {'schemaVersion': 1, 'revision': manifest['revision'], 'serverImage': image,
              'manifestSha256': sha256_file(directory / 'release.json'), 'stagedAt': timestamp()}
    atomic(selected, record)
    journal(runner, True, 'staged')
    return record


def ledger(runner, database):
    lines = runner.docker('exec', '--user', '999:999', database, 'psql', '--no-psqlrc', '--set', 'ON_ERROR_STOP=on',
        '--host', '/run/simplestchat-postgres', '--username', 'postgres', '--dbname', 'simplestchat',
        '--tuples-only', '--no-align', '--field-separator', ' ', '--command',
        "SELECT version, success, encode(checksum, 'hex') FROM public._sqlx_migrations ORDER BY version").decode().splitlines()
    result = {}
    for line in lines:
        fields = line.split()
        require(len(fields) == 3 and re.fullmatch(r'[1-9][0-9]*', fields[0]) and fields[1] == 't'
                and re.fullmatch(r'[a-f0-9]{96}', fields[2]) and fields[0] not in result, 'Invalid migration ledger')
        result[fields[0]] = fields[2]
    require(result, 'Empty migration ledger')
    return result


def ready(runner, *, origin=None, seconds=30):
    deadline = time.monotonic() + seconds
    url = (origin or 'http://127.0.0.1:3000') + '/ready'
    while True:
        try:
            value = json.loads(runner.run(['/usr/bin/curl', '--disable', '--noproxy', '*', '--proto',
                '=https' if origin else '=http', '--fail', '--silent', '--show-error', '--max-time', '2', url], timeout=5))
            require(value.get('status') == 'ready', 'Application is not ready')
            return
        except (ReleaseError, json.JSONDecodeError):
            if time.monotonic() >= deadline:
                raise ReleaseError('Application readiness deadline exceeded') from None
            time.sleep(0.5)


def candidate_selection(runner, new_image, old):
    """Require the rendered service config to change only image selection."""
    compose = (CONFIG / 'compose.public.yml').read_text()
    needle = f'image: "{old["serverImage"]}"'
    require(compose.count(needle) == 2, 'Expected only app and migration image selections; reapply reviewed configuration')
    environment = (CONFIG / 'app.env').read_text()
    needle_env = f'SIMPLESTCHAT_IMAGE={old["serverImage"]}'
    require(environment.splitlines().count(needle_env) == 1, 'Current image environment differs from selection')
    preview = runner.attempt / 'candidate-compose.yml'
    preview_env = runner.attempt / 'candidate.env'
    atomic(preview, compose.replace(needle, f'image: "{new_image}"'))
    atomic(preview_env, '\n'.join(f'SIMPLESTCHAT_IMAGE={new_image}' if line == needle_env else line
                                 for line in environment.splitlines()) + '\n')
    before = json.loads(runner.compose('--profile', 'maintenance', 'config', '--format', 'json'))
    after = json.loads(runner.compose('--profile', 'maintenance', 'config', '--format', 'json', filename=preview, envfile=preview_env))
    expected = deepcopy(before)
    for service in ('simplestchat', 'migrate'):
        expected['services'][service]['image'] = new_image
    # app.env is also a service env_file; preview overrides only interpolation,
    # so its image metadata is changed when the real file is installed below.
    require(after == expected, 'Candidate changes configuration beyond image selection')
    require(before['services']['simplestchat']['environment']['RUN_MIGRATIONS'] == 'false', 'Runtime migrations must remain disabled')
    return preview.read_bytes(), preview_env.read_bytes()


def deploy(runner, manifest, staged, report):
    for filename in SELECTION:
        protected(CONFIG / filename, limit=1024 * 1024)
    old = json.loads((CONFIG / 'images.json').read_text())
    require(ID.fullmatch(old['serverImage']) and re.fullmatch(r'[a-f0-9]{40}', old['revision']), 'Invalid deployed identity')
    image_identity(runner, old['serverImage'], old['revision'])
    new_image = image_identity(runner, staged['serverImage'], manifest['revision'])
    require(old['serverImage'] != new_image, 'This image is already selected')
    app, database, proxy = (runner.container(service) for service in ('simplestchat', 'postgres', 'caddy'))
    for service, container in (('simplestchat', app), ('postgres', database), ('caddy', proxy)):
        fields = runner.compose('config', '--hash', service).decode().split()
        require(fields == [service, container['configHash']], 'Running configuration differs from disk; use reviewed maintenance')
    require(app['image'] == old['serverImage'], 'Running app differs from selected image')
    require(database['state'].get('Health', {}).get('Status') == 'healthy', 'Database must be healthy')
    require(not runner.compose('--profile', 'maintenance', 'ps', '--all', '--quiet', 'migrate').strip(), 'Inspect retained migration container first')
    ready(runner, seconds=3)
    require(ledger(runner, database['id']) == manifest['migrations'], 'Schema changes require the explicit maintenance deployment')
    require(packaged_migrations(runner, new_image) == manifest['migrations'], 'Candidate migration mismatch')
    preview, preview_env = candidate_selection(runner, new_image, old)
    config = json.loads(runner.compose('config', '--format', 'json'))
    origin = config['services']['simplestchat']['environment']['WEBAUTHN_ORIGIN']
    require(re.fullmatch(r'https://[a-z0-9.-]+', origin), 'Unexpected public origin')
    ready(runner, origin=origin, seconds=3)
    backup = runner.attempt / 'database-before.dump'
    require(shutil.disk_usage(ROOT).free > 1024**3, 'Insufficient backup headroom')
    journal(runner, False, 'live_backup')
    runner.docker('exec', '--user', '999:999', database['id'], 'timeout', '--signal=TERM', '--kill-after=2s', '45s',
        'pg_dump', '--host', '/run/simplestchat-postgres',
        '--username', 'postgres', '--dbname', 'simplestchat', '--format', 'custom', timeout=60, output_path=backup)
    require(backup.stat().st_size > 0, 'Empty live database backup')
    runner.docker('exec', '--interactive', '--user', '999:999', database['id'], 'timeout', '--signal=TERM', '--kill-after=2s', '15s',
        'pg_restore', '--list', input_path=backup)
    report['backupSha256'] = sha256_file(backup)
    journal(runner, True, 'backed_up')
    for filename in SELECTION:
        shutil.copyfile(CONFIG / filename, runner.attempt / ('before-' + filename))
    journal(runner, False, 'replace_application')
    replaced = False
    try:
        report['phase'] = 'replace_application'
        report['interruptionStartedAt'] = timestamp()
        replaced = True
        runner.compose('stop', '--timeout', '30', 'simplestchat', timeout=45)
        atomic(CONFIG / 'compose.public.yml', preview)
        atomic(CONFIG / 'app.env', preview_env)
        atomic(CONFIG / 'images.json', dict(old, revision=manifest['revision'], serverImage=new_image))
        runner.compose('up', '--detach', '--no-build', '--pull', 'never', '--no-deps', 'simplestchat', timeout=60)
        require(runner.container('simplestchat')['image'] == new_image, 'Replacement is not the staged image')
        ready(runner)
        ready(runner, origin=origin)
        require(stable_container(runner.container('postgres')) == stable_container(database)
                and stable_container(runner.container('caddy')) == stable_container(proxy), 'A retained dependency restarted during release')
        report['interruptionFinishedAt'] = timestamp()
        report['phase'] = 'complete'
        journal(runner, True, 'complete')
    except BaseException:
        report['rollbackAttempted'] = replaced
        if replaced:
            try:
                failed = runner.compose('ps', '--all', '--quiet', 'simplestchat').decode().strip()
                if re.fullmatch(r'[a-f0-9]{64}', failed):
                    runner.docker('inspect', '--format', '{"image":{{json .Image}},"state":{{json .State}}}', failed)
                runner.compose('logs', '--no-color', '--tail', '100', 'simplestchat')
            except (ReleaseError, subprocess.TimeoutExpired):
                pass
            try:
                runner.compose('stop', '--timeout', '30', 'simplestchat', timeout=45)
                for filename in SELECTION:
                    atomic(CONFIG / filename, (runner.attempt / ('before-' + filename)).read_bytes())
                runner.compose('up', '--detach', '--no-build', '--pull', 'never', '--no-deps', 'simplestchat', timeout=60)
                require(runner.container('simplestchat')['image'] == old['serverImage'], 'Rollback image mismatch')
                ready(runner)
                ready(runner, origin=origin)
                require(stable_container(runner.container('postgres')) == stable_container(database)
                        and stable_container(runner.container('caddy')) == stable_container(proxy), 'Dependency continuity changed')
                report['rollbackPassed'] = True
                journal(runner, True, 'rolled_back')
            except BaseException:
                report['rollbackPassed'] = False
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('stage', 'deploy'))
    parser.add_argument('revision')
    arguments = parser.parse_args()
    require(os.geteuid() == 0, 'Run as root on the prepared public host')
    require(re.fullmatch(r'[a-f0-9]{40}', arguments.revision), 'Use the exact release commit')
    os.umask(0o077)
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda _signum, _frame: (_ for _ in ()).throw(ReleaseError('Release interrupted')))
    with workload_lock():
        protected(ROOT / 'releases', directory=True, modes=(0o700,))
        directory = ROOT / 'releases' / arguments.revision
        protected(directory, directory=True, modes=(0o700,))
        for filename in ('release.json', 'image.tar'):
            protected(directory / filename)
        manifest = validate_manifest(directory / 'release.json')
        require(manifest['revision'] == arguments.revision, 'Release directory identity differs')
        protected(ROOT / 'results', directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix='release.', dir=ROOT / 'results'))
        report = {'action': arguments.action, 'revision': arguments.revision, 'startedAt': timestamp(), 'passed': False, 'phase': 'stage'}
        try:
            runner = Runner(attempt)
            staged = stage(runner, directory, manifest)
            if arguments.action == 'deploy':
                report['phase'] = 'preflight'
                deploy(runner, manifest, staged, report)
            else:
                report['phase'] = 'complete'
            report['passed'] = True
        except BaseException as error:
            report['failure'] = str(error) if isinstance(error, (ReleaseError, ArtifactError)) else type(error).__name__
            raise
        finally:
            report['finishedAt'] = timestamp()
            atomic(attempt / 'outcome.json', report)
            print(json.dumps({'evidence': str(attempt), **report}))


if __name__ == '__main__':
    try:
        main()
    except (ReleaseError, ArtifactError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f'Release failed ({type(error).__name__}); inspect private evidence before another attempt.', file=sys.stderr)
        sys.exit(1)
