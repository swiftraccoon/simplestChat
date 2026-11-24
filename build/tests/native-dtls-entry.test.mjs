import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));

// The documented entry point starts the Rust controller from the checkout
// root, whatever directory it is called from, and takes no arguments.
test('the native DTLS entry point runs the CI controller from the checkout root', async t => {
  await mkdir(path.join(root, 'target'), { recursive: true });
  const directory = await mkdtemp(path.join(root, 'target', 'native-dtls-entry-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const calls = path.join(directory, 'calls');
  await writeFile(path.join(directory, 'cargo'), `#!/bin/sh\nprintf '%s|%s\\n' "$PWD" "$*" >> '${calls}'\nexit 7\n`,
    { mode: 0o755 });
  const env = { PATH: `${directory}:${process.env.PATH}` };
  const started = spawnSync('/bin/bash', [path.join(root, 'build/check-native-dtls.sh')], {
    cwd: directory, encoding: 'utf8', timeout: 10_000, env,
  });
  assert.equal(started.status, 7, started.stderr);
  assert.equal(await readFile(calls, 'utf8'),
    `${root.replace(/\/$/, '')}|run --locked -q -p simplestchat-ci -- native-dtls\n`);
  const usage = spawnSync('/bin/bash', [path.join(root, 'build/check-native-dtls.sh'), '--record-success'], {
    cwd: directory, encoding: 'utf8', timeout: 10_000, env,
  });
  assert.equal(usage.status, 2);
  assert.match(usage.stderr, /^Usage: build\/check-native-dtls\.sh$/m);
  assert.equal(await readFile(calls, 'utf8').then(text => text.split('\n').length), 2);
});
