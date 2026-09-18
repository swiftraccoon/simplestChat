#!/usr/bin/env python3
"""Deploy clean HEAD using its successful push CI image, then run public smoke.

Requires Python 3.12+, authenticated GitHub CLI, Ansible and Node 22.12+ on the
controller. This command deploys to ONE prepared inventory host and writes one
labeled public smoke message. It never pushes, dispatches CI, builds an image,
retries a failed release, or bypasses the existing release helper's checks.
"""

import argparse
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import tempfile
import time
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]


def load_helper(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'build' / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


BUILD = load_helper('deploy_build_helpers', 'build-release.py')
FETCH = load_helper('deploy_fetch_helpers', 'fetch-release.py')


class DeployError(Exception):
    """Fixed failure codes only; raw inventory or subprocess output stays private."""


def require(condition, code):
    if not condition:
        raise DeployError(code)


def options(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inventory', required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--origin', required=True)
    parser.add_argument('--limit', help='Exact inventory hostname; patterns are not supported')
    parser.add_argument('--room', default='lobby')
    parser.add_argument('--wait-seconds', type=int, default=3600)
    parser.add_argument('--install-helpers', action='store_true',
                        help='Explicitly reconcile release helpers instead of requiring their exact installed hashes')
    parser.add_argument('--ansible-playbook', help='Controller executable; defaults to local .venv, then PATH')
    args = parser.parse_args(argv)
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}', args.repository),
            'invalid_repository')
    require(args.limit is None or re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,252}', args.limit), 'invalid_limit')
    require(re.fullmatch(r'[A-Za-z0-9_-]{1,128}', args.room), 'invalid_room')
    require(0 <= args.wait_seconds <= 7200, 'invalid_wait_seconds')
    origin = urlsplit(args.origin)
    require(origin.scheme == 'https' and origin.hostname and not origin.username and not origin.password
            and origin.path in ('', '/') and not origin.query and not origin.fragment
            and origin.port in (None, 443)
            and re.fullmatch(r'https://[a-z0-9.-]+(?::443)?/?', args.origin), 'invalid_origin')
    args.origin = 'https://' + origin.hostname
    args.inventory = str(Path(args.inventory).expanduser().resolve(strict=True))
    require(Path(args.inventory).is_file(), 'invalid_inventory')
    return args


def controller_tools(args, root):
    local = root / 'ops/ansible/.venv/bin/ansible-playbook'
    playbook = args.ansible_playbook or (str(local) if local.is_file() else shutil.which('ansible-playbook'))
    require(playbook is not None, 'ansible_not_found')
    playbook = Path(playbook).absolute()
    inventory = playbook.with_name('ansible-inventory')
    require(playbook.is_file() and os.access(playbook, os.X_OK)
            and inventory.is_file() and os.access(inventory, os.X_OK), 'ansible_not_found')
    node = shutil.which('node')
    require(node is not None, 'node_not_found')
    return str(playbook), str(inventory), node


def ansible_environment(root):
    # Use the reviewed configuration, including host-key checks. GitHub auth is
    # available to the controller's delegated fetch, never passed as extra vars.
    environment = {key: value for key, value in os.environ.items() if not key.startswith('ANSIBLE_')}
    environment.update(ANSIBLE_CONFIG=str(root / 'ops/ansible/ansible.cfg'),
                       ANSIBLE_HOST_KEY_CHECKING='True', ANSIBLE_RETRY_FILES_ENABLED='False')
    return environment


def checkout_identity(runner, root, repository):
    revision = BUILD.clean_revision(runner, root)
    _, remote = runner.run(['git', 'remote', 'get-url', 'origin'], cwd=root)
    match = re.fullmatch(r'(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)'
                         r'([A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}?)(?:\.git)?', remote)
    require(match is not None and match[1].lower() == repository.lower(), 'repository_origin_mismatch')
    return revision


def selected_host(args, inventory_tool, root, environment):
    # Inventory can contain secrets. Inspect through an unlogged bounded runner,
    # and retain only the selected host's resolved variables. They may later be
    # frozen in a private one-host inventory, but are never printed or logged.
    _, encoded = BUILD.Runner().run([inventory_tool, '-i', args.inventory, '--list'],
                                   cwd=root, env=environment, timeout=30)
    inventory = FETCH.decoded(encoded)
    require(isinstance(inventory, dict), 'invalid_inventory_graph')

    def hosts(group, seen):
        require(group not in seen and isinstance(inventory.get(group), dict), 'invalid_inventory_graph')
        value = inventory[group]
        direct, children = value.get('hosts', []), value.get('children', [])
        require(isinstance(direct, list) and isinstance(children, list)
                and all(isinstance(item, str) for item in direct + children), 'invalid_inventory_graph')
        result = set(direct)
        for child in children:
            result.update(hosts(child, seen | {group}))
        return result

    targets = hosts('benchmark_hosts', set())
    if args.limit is not None:
        require(args.limit in targets, 'inventory_target_not_found')
        targets = {args.limit}
    require(len(targets) == 1, 'select_exactly_one_inventory_host')
    host = next(iter(targets))
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,252}', host), 'invalid_inventory_host')
    variables = inventory.get('_meta', {}).get('hostvars', {}).get(host, {})
    require(variables.get('scpub_domain') == urlsplit(args.origin).hostname, 'inventory_smoke_origin_mismatch')
    return host, variables


def require_ci_identity(run, repository, revision, run_id=None):
    require(isinstance(run, dict) and FETCH.positive(run.get('id'))
            and (run_id is None or run['id'] == run_id)
            and run.get('name') == 'CI' and run.get('path') == '.github/workflows/ci.yml'
            and run.get('event') == 'push' and run.get('head_branch') == 'main'
            and run.get('head_sha') == revision
            and run.get('repository', {}).get('full_name', '').lower() == repository.lower()
            and run.get('head_repository', {}).get('full_name', '').lower() == repository.lower(),
            'ci_identity_mismatch')


def select_ci_artifact(args, revision):
    runs = FETCH.api(f'repos/{args.repository}/actions/workflows/ci.yml/runs'
                     f'?event=push&branch=main&head_sha={revision}&per_page=1')
    require(isinstance(runs, dict) and isinstance(runs.get('workflow_runs'), list), 'ci_response_invalid')
    require(len(runs['workflow_runs']) == 1, 'ci_missing_push_required')
    run = runs['workflow_runs'][0]
    require_ci_identity(run, args.repository, revision)
    run_id = run['id']
    deadline = time.monotonic() + args.wait_seconds
    while True:
        # Pin the selected run throughout waiting; never fall back to an older
        # successful run when the latest run fails, and never trigger a rerun.
        run = FETCH.api(f'repos/{args.repository}/actions/runs/{run_id}')
        require_ci_identity(run, args.repository, revision, run_id)
        if run.get('status') == 'completed':
            require(run.get('conclusion') == 'success', 'ci_failed')
            break
        require(run.get('status') in ('queued', 'in_progress', 'pending', 'waiting', 'requested'), 'ci_status_invalid')
        remaining = deadline - time.monotonic()
        require(remaining > 0, 'ci_wait_timeout')
        print(f'Waiting for CI run {run_id}; at most {int(remaining)} seconds remain.', flush=True)
        time.sleep(min(15, remaining))
    artifacts = FETCH.api(f'repos/{args.repository}/actions/runs/{run_id}/artifacts'
                          f'?name=simplestchat-production-{revision}&per_page=100')
    require(isinstance(artifacts, dict) and isinstance(artifacts.get('artifacts'), list)
            and artifacts.get('total_count') == 1 and len(artifacts['artifacts']) == 1,
            'ci_artifact_missing_or_ambiguous')
    artifact = artifacts['artifacts'][0]
    require(isinstance(artifact, dict) and FETCH.positive(artifact.get('id')), 'ci_artifact_identity_invalid')
    selection = argparse.Namespace(repository=args.repository, revision=revision,
                                   ci_run=run_id, artifact_id=artifact['id'])
    envelope = FETCH.verified_envelope(selection)
    require(envelope['buildRunId'] == run_id and envelope['ciRunId'] == run_id,
            'ci_artifact_identity_invalid')
    return envelope


def execute(args, root=ROOT):
    report = {'schemaVersion': 1, 'operation': 'deploy', 'passed': False, 'phase': 'preflight',
              'deployed': False, 'remoteOutcome': 'not_started', 'failureClass': None,
              'startedAt': datetime.now(timezone.utc).isoformat()}
    directory = None
    started = time.monotonic()
    try:
        inspector = BUILD.Runner()
        revision = checkout_identity(inspector, root, args.repository)
        report.update(repository=args.repository, revision=revision)
        playbook, inventory_tool, node = controller_tools(args, root)
        environment = ansible_environment(root)
        target = selected_host(args, inventory_tool, root, environment)
        host, hostvars = target
        _, version = inspector.run([node, '--version'], cwd=root)
        match = re.fullmatch(r'v(\d+)\.(\d+)\.\d+', version)
        require(match and (int(match[1]), int(match[2])) >= (22, 12), 'node_version_unsupported')
        _, ignored = inspector.run(['git', 'check-ignore', '--', str(root / 'results/deploy.evidence')], cwd=root)
        require(bool(ignored), 'results_must_be_git_ignored')
        parent = root / 'results'
        require(not parent.is_symlink(), 'invalid_evidence_parent')
        parent.mkdir(mode=0o700, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix='deploy.', dir=parent))
        report['evidence'] = str(directory)
        runner = BUILD.Runner(directory)
        report['phase'] = 'ci'
        envelope = select_ci_artifact(args, revision)
        BUILD.write_json(directory / 'selection.json', envelope)
        report.update(ciRunId=envelope['ciRunId'], artifactId=envelope['artifactId'])
        require(selected_host(args, inventory_tool, root, environment) == target, 'inventory_target_changed')
        # Ansible --limit accepts patterns and a host alias can equal a group
        # name. A frozen one-host inventory makes the destination unambiguous
        # and prevents a later original-inventory edit from redirecting it.
        frozen_inventory = directory / 'inventory.json'
        BUILD.write_json(frozen_inventory, {'benchmark_hosts': {'hosts': {host: hostvars}}})
        extra = {'scpub_release_repository': args.repository, 'scpub_release_artifact_id': envelope['artifactId'],
                 'scpub_release_expected_revision': revision, 'scpub_release_ci_run': envelope['ciRunId'],
                 'scpub_release_prepared': not args.install_helpers, 'scpub_release_deploy': True}
        require(checkout_identity(inspector, root, args.repository) == revision, 'checkout_changed')
        report.update(phase='deploy', remoteOutcome='inspect_if_interrupted')
        runner.run([playbook, '-i', str(frozen_inventory), str(root / 'ops/ansible/release.yml'),
                    '--limit', host, '--extra-vars', json.dumps(extra)], cwd=root, env=environment,
                   timeout=2400, capture=False)
        report.update(deployed=True, remoteOutcome='release_succeeded', phase='public_smoke')
        _, output = runner.run([node, str(root / 'build/public-smoke.mjs'), '--origin', args.origin,
                                '--room', args.room], cwd=root, env=FETCH.ssh_environment(), timeout=60)
        smoke = FETCH.decoded(output)
        require(isinstance(smoke, dict) and smoke.get('passed') is True
                and smoke.get('origin') == args.origin and smoke.get('room') == args.room, 'public_smoke_failed')
        report.update(passed=True, phase='complete')
    except (Exception, KeyboardInterrupt) as error:
        report['failureClass'] = str(error) if isinstance(error, (DeployError, FETCH.FetchError)) else 'controller_step_failed'
    finally:
        report['elapsedSeconds'] = round(time.monotonic() - started, 3)
        if directory is not None:
            try:
                BUILD.write_json(directory / 'outcome.json', report)
            except OSError:
                report.update(passed=False, failureClass='outcome_write_failed')
    return report


def main(argv=None):
    try:
        args = options(argv)
    except SystemExit as error:
        return error.code
    except (DeployError, OSError, ValueError):
        print(json.dumps({'passed': False, 'phase': 'options', 'failureClass': 'invalid_options'}))
        return 1

    def interrupted(_signum, _frame):
        raise DeployError('interrupted')

    previous = {number: signal.signal(number, interrupted) for number in (signal.SIGINT, signal.SIGTERM)}
    previous_umask = os.umask(0o077)
    try:
        report = execute(args)
    finally:
        os.umask(previous_umask)
        for number, handler in previous.items():
            signal.signal(number, handler)
    print(json.dumps(report), flush=True)
    if not report['passed']:
        if report['failureClass'] == 'ci_missing_push_required':
            print('No push CI exists for this clean HEAD on main. Push the reviewed commit explicitly, then rerun.')
        else:
            print('Stopped without retry. Inspect the retained evidence and any remote release journal before another attempt.')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
