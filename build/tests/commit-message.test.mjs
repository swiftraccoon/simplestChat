import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const hook = fileURLToPath(new URL('../../.githooks/commit-msg', import.meta.url));

test('commit hook accepts conventional subjects and rejects missing or malformed prefixes', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-commit-message.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const message = path.join(directory, 'message');
  for (const subject of [
    'fix(web): prevent overlapping controls',
    'ci: check the release commit subject',
    'feat(api)!: require the new protocol',
    'ops: wait for empty rooms before replacing the app',
  ]) {
    await writeFile(message, `${subject}\n\nExplain the change.\n`);
    const result = spawnSync(hook, [message], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
  }
  for (const subject of [
    'Keep header controls from overlapping at narrow widths',
    'Fix registration theme, passkey guidance, and room navigation',
    'fix(web):',
    'fix(web):   ',
    'fix(web):missing space',
    'fix(): missing scope',
    '',
  ]) {
    await writeFile(message, `${subject}\n`);
    const result = spawnSync(hook, [message], { encoding: 'utf8' });
    assert.equal(result.status, 1, subject);
    assert.match(result.stderr, /Commit subject must use/);
  }
});

test('Git rejects an invalid subject before creating a commit', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-commit-hook.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const git = (...args) => spawnSync('git', args, {
    cwd: directory,
    encoding: 'utf8',
    env: {
      PATH: process.env.PATH,
      GIT_CONFIG_GLOBAL: '/dev/null',
      GIT_CONFIG_NOSYSTEM: '1',
    },
  });
  for (const args of [
    ['init', '--quiet'],
    ['config', 'core.hooksPath', path.dirname(hook)],
    ['config', 'user.name', 'Commit Hook Test'],
    ['config', 'user.email', 'commit-hook@example.test'],
  ]) {
    const result = git(...args);
    assert.equal(result.status, 0, result.stderr);
  }
  const rejected = git('commit', '--allow-empty', '-m', 'Invalid subject');
  assert.equal(rejected.status, 1);
  assert.match(rejected.stderr, /Commit subject must use/);
  assert.notEqual(git('rev-parse', '--verify', 'HEAD').status, 0);
  const accepted = git('commit', '--allow-empty', '-m', 'test: verify commit enforcement');
  assert.equal(accepted.status, 0, accepted.stderr);
  assert.equal(git('log', '-1', '--format=%s').stdout.trim(), 'test: verify commit enforcement');
});
