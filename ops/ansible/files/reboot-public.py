#!/usr/bin/env python3
"""Prepare or recover an explicitly requested reboot of one public chat host.

This helper never reboots the host. Preparation records private, durable
evidence and stops only the existing public application's three containers.
After a changed Linux boot ID, recovery starts those same containers in order.
No image build, pull, container recreation, migration or account seeding occurs.
An unsuccessful reboot request can be cancelled on the original boot only;
successful service recovery does not turn that reboot into a successful one.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sys
import tempfile
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location('public_release', Path(__file__).with_name('release-public.py'))
PUBLIC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLIC)

SERVICES = ('simplestchat', 'caddy', 'postgres')
CONFIGURATION = {
    **{name: 0o600 for name in PUBLIC.SELECTION},
    'proxy.env': 0o600,
    'compose.base.yml': 0o644,
    'Caddyfile': 0o644,
    'pg_hba.conf': 0o644,
}
UUID = re.compile(r'[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}')
HEX = re.compile(r'[a-f0-9]{64}')


def configuration():
    """Bind recovery to protected configuration, including mounted proxy/HBA files."""
    result = {}
    for name, mode in CONFIGURATION.items():
        path = PUBLIC.CONFIG / name
        PUBLIC.protected(path, modes=(mode,), limit=1024 * 1024)
        result[name] = PUBLIC.sha256_file(path)
    return result


def container(runner, service):
    """Inspect exactly one existing project container, whether running or stopped."""
    identifier = runner.compose('ps', '--all', '--quiet', service, timeout=10).decode().strip()
    PUBLIC.require(HEX.fullmatch(identifier), f'Exactly one existing {service} container is required')
    value = json.loads(runner.docker('inspect', '--format',
        '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},'
        '"configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}', identifier, timeout=10))
    PUBLIC.require(value['id'] == identifier and PUBLIC.ID.fullmatch(value['image'])
                   and isinstance(value['configHash'], str) and HEX.fullmatch(value['configHash']),
                   f'Invalid {service} container identity')
    PUBLIC.require(value['state'].get('OOMKilled') is False, f'{service} was OOM-killed')
    return value


def identity(value):
    return {key: value[key] for key in ('id', 'image', 'configHash')}


def selection(runner):
    value = json.loads((PUBLIC.CONFIG / 'images.json').read_text())
    PUBLIC.require(isinstance(value, dict) and isinstance(value.get('revision'), str)
                   and re.fullmatch(r'[a-f0-9]{40}', value['revision'])
                   and isinstance(value.get('serverImage'), str) and PUBLIC.ID.fullmatch(value['serverImage']),
                   'Invalid deployed image selection')
    PUBLIC.require(PUBLIC.image_identity(runner, value['serverImage'], value['revision']) == value['serverImage'],
                   'Selected application image is unavailable')
    return value


def capture(runner):
    """Prove a healthy, unchanged selection before taking the public service down."""
    help_text = runner.compose('start', '--help', timeout=10).decode()
    PUBLIC.require(re.search(r'(?m)^\s+--wait\s', help_text)
                   and re.search(r'(?m)^\s+--wait-timeout\s', help_text),
                   'Installed Compose must support start --wait and --wait-timeout before reboot preparation')
    hashes = configuration()
    selected = selection(runner)
    containers = {service: container(runner, service) for service in SERVICES}
    for service, value in containers.items():
        PUBLIC.require(value['state'].get('Running') is True, f'{service} must be running')
        fields = runner.compose('config', '--hash', service, timeout=10).decode().split()
        PUBLIC.require(fields == [service, value['configHash']], 'Running configuration differs from disk')
    PUBLIC.require(containers['simplestchat']['image'] == selected['serverImage'], 'Application image differs from selection')
    for service, key in (('postgres', 'postgresImage'), ('caddy', 'caddyImage')):
        selector = selected.get(key)
        PUBLIC.require(isinstance(selector, str) and re.fullmatch(r'[^\s]+@sha256:[a-f0-9]{64}', selector),
                       'Dependency images must be checksum-pinned')
        image = runner.docker('image', 'inspect', '--format', '{{.Id}}', selector, timeout=10).decode().strip()
        PUBLIC.require(image == containers[service]['image'], f'{service} image differs from selection')
    PUBLIC.require(containers['postgres']['state'].get('Health', {}).get('Status') == 'healthy', 'Database must be healthy')
    PUBLIC.require(not runner.compose('--profile', 'maintenance', 'ps', '--all', '--quiet', 'migrate', timeout=10).strip(),
                   'Inspect retained migration container first')
    resolved = json.loads(runner.compose('config', '--format', 'json', timeout=10))
    application = resolved['services']['simplestchat']
    PUBLIC.require(application['environment']['RUN_MIGRATIONS'] == 'false', 'Runtime migrations must remain disabled')
    PUBLIC.require(all(resolved['services'][service]['restart'] == 'unless-stopped' for service in SERVICES),
                   'Prepared reboot requires the existing unless-stopped policies')
    origin = application['environment']['WEBAUTHN_ORIGIN']
    PUBLIC.require(isinstance(origin, str) and re.fullmatch(r'https://[a-z0-9.-]+', origin), 'Unexpected public origin')
    PUBLIC.ready(runner, seconds=3)
    PUBLIC.ready(runner, origin=origin, seconds=3)
    return {'schemaVersion': 1, 'bootId': PUBLIC.boot_id(), 'configuration': hashes, 'selection': selected,
            'containers': {service: identity(value) for service, value in containers.items()}, 'origin': origin,
            'resolvedSha256': hashlib.sha256(json.dumps(resolved, sort_keys=True).encode()).hexdigest()}


def validate(runner, saved):
    """Reject changed files or containers instead of rebuilding/recreating anything."""
    PUBLIC.require(configuration() == saved['configuration'], 'Configuration changed after reboot preparation')
    PUBLIC.require(selection(runner) == saved['selection'], 'Image selection changed after reboot preparation')
    resolved = json.loads(runner.compose('config', '--format', 'json', timeout=10))
    digest = hashlib.sha256(json.dumps(resolved, sort_keys=True).encode()).hexdigest()
    PUBLIC.require(digest == saved['resolvedSha256'], 'Resolved configuration changed after reboot preparation')
    values = {}
    for service in SERVICES:
        values[service] = container(runner, service)
        PUBLIC.require(identity(values[service]) == saved['containers'][service], f'{service} container changed after preparation')
    return values


def write_state(runner, saved, phase):
    PUBLIC.atomic(PUBLIC.ROOT / 'release-state.json', {
        'schemaVersion': 1, 'action': 'reboot', 'attempt': str(runner.attempt), 'finalized': False,
        'phase': phase, 'bootId': saved['bootId'], 'identitySha256': PUBLIC.sha256_file(runner.attempt / 'identity.json'),
    })


def start(runner, saved):
    """Start existing containers in dependency order with bounded readiness gates."""
    validate(runner, saved)
    runner.compose('start', '--wait', '--wait-timeout', '180', 'postgres', timeout=195)
    database = container(runner, 'postgres')
    PUBLIC.require(database['state'].get('Running') is True
                   and database['state'].get('Health', {}).get('Status') == 'healthy', 'Database did not become healthy')
    runner.compose('start', 'simplestchat', timeout=30)
    PUBLIC.ready(runner, seconds=45)
    runner.compose('start', 'caddy', timeout=30)
    PUBLIC.ready(runner, origin=saved['origin'], seconds=45)
    values = validate(runner, saved)
    PUBLIC.require(all(value['state'].get('Running') is True for value in values.values()), 'A recovered service is not running')


def prepare(runner, report):
    saved = capture(runner)
    PUBLIC.atomic(runner.attempt / 'identity.json', saved)
    report.update(bootId=saved['bootId'], revision=saved['selection']['revision'])
    write_state(runner, saved, 'stop_for_reboot')
    try:
        report.update(phase='stop_for_reboot', interruptionStartedAt=PUBLIC.timestamp())
        for service in SERVICES:
            grace = '60' if service == 'postgres' else '30'
            runner.compose('stop', '--timeout', grace, service, timeout=int(grace) + 15)
            stopped = container(runner, service)
            PUBLIC.require(identity(stopped) == saved['containers'][service]
                           and stopped['state'].get('Running') is False and stopped['state'].get('ExitCode') == 0,
                           f'{service} did not stop cleanly')
        report['phase'] = 'await_reboot'
        # Publish the evidence before admitting post-boot recovery via the journal.
        PUBLIC.atomic(runner.attempt / 'outcome.json', report)
        write_state(runner, saved, 'await_reboot')
    except BaseException:
        report['recoveryAttempted'] = True
        try:
            PUBLIC.require(PUBLIC.boot_id() == saved['bootId'], 'Boot changed during preparation')
            start(runner, saved)
            report.update(recoveryPassed=True, phase='prepare_failed_recovered', interruptionFinishedAt=PUBLIC.timestamp())
            PUBLIC.journal(runner, True, 'reboot_prepare_failed_recovered')
        except BaseException:
            report['recoveryPassed'] = False
        raise


def pending(action):
    """Load only the exact protected attempt selected by the persistent journal."""
    path = PUBLIC.ROOT / 'release-state.json'
    PUBLIC.protected(path, limit=16384)
    record = json.loads(path.read_text())
    PUBLIC.require(isinstance(record, dict) and type(record.get('schemaVersion')) is int
                   and record['schemaVersion'] == 1 and record.get('finalized') is False
                   and record.get('action') == 'reboot' and record.get('phase') == 'await_reboot',
                   'No unfinished prepared reboot is available')
    PUBLIC.require(isinstance(record.get('bootId'), str) and UUID.fullmatch(record['bootId']), 'Invalid prepared boot identity')
    PUBLIC.require((PUBLIC.boot_id() != record['bootId']) if action == 'resume' else (PUBLIC.boot_id() == record['bootId']),
                   'Resume requires a changed boot; cancellation requires the original boot')
    PUBLIC.require(isinstance(record.get('attempt'), str), 'Missing prepared attempt path')
    attempt = Path(record['attempt'])
    PUBLIC.require(attempt.parent == PUBLIC.ROOT / 'results' and re.fullmatch(r'reboot\.[a-z0-9_]+', attempt.name)
                   and str(attempt) == record['attempt'], 'Unexpected prepared attempt path')
    PUBLIC.protected(attempt, directory=True, modes=(0o700,))
    snapshot = attempt / 'identity.json'
    PUBLIC.protected(snapshot, limit=65536)
    PUBLIC.require(isinstance(record.get('identitySha256'), str) and HEX.fullmatch(record['identitySha256'])
                   and PUBLIC.sha256_file(snapshot) == record['identitySha256'], 'Prepared identity evidence changed')
    saved = json.loads(snapshot.read_text())
    PUBLIC.require(isinstance(saved, dict) and type(saved.get('schemaVersion')) is int and saved['schemaVersion'] == 1
                   and saved.get('bootId') == record['bootId'], 'Prepared identity does not match the journal')
    report_path = attempt / 'outcome.json'
    PUBLIC.protected(report_path, limit=65536)
    report = json.loads(report_path.read_text())
    PUBLIC.require(isinstance(report, dict) and report.get('action') == 'reboot' and report.get('passed') is False
                   and report.get('phase') == 'await_reboot' and report.get('bootId') == saved['bootId'],
                   'Prepared outcome does not match the journal')
    return attempt, saved, report


def recover(runner, saved, report, action, attempt):
    phase = 'resume_after_reboot' if action == 'resume' else 'cancel_before_reboot'
    journal_runner = SimpleNamespace(attempt=attempt)
    write_state(journal_runner, saved, phase)
    report.update(phase=phase, recoveryStartedAt=PUBLIC.timestamp(), recoveryBootId=PUBLIC.boot_id())
    start(runner, saved)
    report.update(recoveryPassed=True, interruptionFinishedAt=PUBLIC.timestamp())
    if action == 'resume':
        report.update(passed=True, phase='complete')
        PUBLIC.journal(journal_runner, True, 'reboot_complete')
    else:
        report.update(passed=False, phase='cancelled', failure='Requested reboot did not complete; original-boot services recovered')
        PUBLIC.journal(journal_runner, True, 'reboot_cancelled')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'resume', 'cancel'))
    arguments = parser.parse_args()
    PUBLIC.require(os.geteuid() == 0, 'Run as root on the prepared public host')
    os.umask(0o077)
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda _signum, _frame: (_ for _ in ()).throw(PUBLIC.ReleaseError('Reboot operation interrupted')))
    with PUBLIC.workload_lock(after_reboot=arguments.action == 'resume', cancel_reboot=arguments.action == 'cancel'):
        PUBLIC.protected(PUBLIC.ROOT / 'results', directory=True, modes=(0o700,))
        if arguments.action == 'prepare':
            attempt = Path(tempfile.mkdtemp(prefix='reboot.', dir=PUBLIC.ROOT / 'results'))
            report = {'action': 'reboot', 'startedAt': PUBLIC.timestamp(), 'passed': False, 'phase': 'preflight'}
            runner = PUBLIC.Runner(attempt)
            saved = None
        else:
            attempt, saved, report = pending(arguments.action)
            # A distinct evidence directory prevents overwriting preparation output.
            evidence = attempt / arguments.action
            evidence.mkdir(mode=0o700)
            runner = PUBLIC.Runner(evidence)
        try:
            if arguments.action == 'prepare':
                prepare(runner, report)
            else:
                recover(runner, saved, report, arguments.action, attempt)
        except BaseException as error:
            report.update(passed=False, failure=str(error) if isinstance(error, PUBLIC.ReleaseError) else type(error).__name__)
            raise
        finally:
            report['updatedAt'] = PUBLIC.timestamp()
            PUBLIC.atomic(attempt / 'outcome.json', report)
            print(json.dumps({'action': arguments.action, 'phase': report['phase'], 'passed': report['passed'], 'evidence': str(attempt)}))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'Reboot operation failed ({type(error).__name__}); inspect retained private evidence.', file=sys.stderr)
        sys.exit(1)
