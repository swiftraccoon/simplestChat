import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));

// The orchestrator's owned-checkout, engine and receipt regressions now execute
// its actual Rust modules in crates/ci/src/local/ci_local_tests.rs.
test('container helper entrypoints reject ordinary host invocation before starting tools or sockets', () => {
  for (const helper of ['ci-local-docker.sh', 'ci-local-postgres.sh']) {
    const result = spawnSync('/bin/bash', [path.join(root, 'build', helper)], {
      env: { PATH: process.env.PATH }, encoding: 'utf8', timeout: 5000,
    });
    assert.equal(result.status, 2);
    assert.match(result.stderr, /only for disposable local CI runners/);
  }
});

async function relayFixture(t, relay) {
  const { mkdir, mkdtemp, readFile, rm, writeFile } = await import('node:fs/promises');
  await mkdir(path.join(root, 'results'), { recursive: true });
  const directory = await mkdtemp(path.join(root, 'results/ci-p.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = path.join(directory, 'bin');
  const temporary = path.join(directory, 'tmp');
  await mkdir(bin);
  await mkdir(temporary);
  await writeFile(path.join(directory, 'container'), 'fixture');
  const calls = path.join(directory, 'calls');
  await writeFile(calls, '');
  // The real helper with its fixed container marker redirected into the fixture.
  const source = (await readFile(path.join(root, 'build/ci-local-postgres.sh'), 'utf8'))
    .replaceAll('/.dockerenv', path.join(directory, 'container'));
  const helper = path.join(directory, 'helper.sh');
  await writeFile(helper, source);
  await writeFile(path.join(bin, 'uname'), '#!/bin/sh\necho Linux\n', { mode: 0o755 });
  await writeFile(path.join(bin, 'cargo'), `#!/bin/sh
printf '%s\\n' "$*" >> '${calls}'
if [ "$1" = build ]; then exit ${relay.build ?? 0}; fi
${relay.script}
`, { mode: 0o755 });
  return {
    run(port = '5433') {
      return spawnSync('/bin/bash', [helper], {
        cwd: root, encoding: 'utf8', timeout: 20_000,
        env: { PATH: `${bin}:${process.env.PATH}`, ACT: 'true', LOCAL_CI_DISPOSABLE: '1',
          CI_LOCAL_POSTGRES_PORT: port, RUNNER_TEMP: temporary },
      });
    },
    async calls() { return (await readFile(calls, 'utf8')).split('\n').filter(Boolean); },
    ready: path.join(temporary, 'local-ci-postgres/ready'),
  };
}

test('the PostgreSQL relay is this checkout\'s CI command, built before its readiness wait', async t => {
  const f = await relayFixture(t, { script: 'for last; do :; done\nprintf "ready\\n" > "$last"' });
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Started the job-local PostgreSQL relay on port 5433\./);
  assert.deepEqual(await f.calls(), [
    'build --locked -q -p simplestchat-ci --bin simplestchat-ci',
    `run --locked -q -p simplestchat-ci -- local-postgres-relay 5433 ${f.ready}`,
  ]);
});

test('a relay that refuses to start is reported with its own words and never ready', async t => {
  const f = await relayFixture(t, { script: 'echo "The CI service gateway must be private" >&2\nexit 1' });
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /The CI service gateway must be private/);
  assert.match(result.stderr, /did not become ready/);
});

test('a relay that cannot be built is never started', async t => {
  const f = await relayFixture(t, { build: 101, script: 'exit 0' });
  const result = f.run();
  assert.equal(result.status, 101);
  assert.deepEqual(await f.calls(), ['build --locked -q -p simplestchat-ci --bin simplestchat-ci']);
  for (const port of ['5431', '5436', '15432']) {
    const refused = (await relayFixture(t, { script: 'exit 0' })).run(port);
    assert.equal(refused.status, 2);
    assert.match(refused.stderr, /declared CI port/);
  }
});
