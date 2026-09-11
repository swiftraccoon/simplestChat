import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const web = fileURLToPath(new URL('..', import.meta.url));
const linter = path.join(web, 'node_modules/oxlint/bin/oxlint');
const ui = JSON.stringify(path.join(web, 'src/ui.ts'));

test('the real typed lint configuration rejects discarded and misused promises and unsafe JSON', async (t) => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-lint-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await mkdir(path.join(directory, 'src'));
  await symlink(path.join(web, 'node_modules'), path.join(directory, 'node_modules'), 'dir');
  await writeFile(
    path.join(directory, '.oxlintrc.json'),
    await readFile(path.join(web, '.oxlintrc.json')),
  );
  const config = JSON.parse(await readFile(path.join(web, 'tsconfig.app.json'), 'utf8'));
  delete config.compilerOptions.tsBuildInfoFile;
  await writeFile(path.join(directory, 'tsconfig.json'), JSON.stringify(config));
  const run = async (source) => {
    await writeFile(path.join(directory, 'src/probe.ts'), source);
    const result = spawnSync(
      process.execPath,
      [linter, '--type-aware', '--deny-warnings', '--format', 'json', 'src/probe.ts'],
      {
        cwd: directory,
        encoding: 'utf8',
        timeout: 20000,
      },
    );
    assert.equal(result.error, undefined, 'the linter must actually run');
    return { status: result.status, output: result.stdout + result.stderr };
  };
  const accepted = await run(`import { button, asyncButton } from ${ui};
    export const sync = button('Sync', () => {});
    export const owned = asyncButton('Owned', () => Promise.resolve(), (_error: unknown) => {});
    export function handled() { Promise.resolve().catch((_error: unknown) => {}); }
  `);
  assert.equal(accepted.status, 0, accepted.output);
  const rejected = await run(`import { button } from ${ui};
    export const discarded = () => void Promise.resolve();
    export function floating() { Promise.resolve(); }
    export const misused = button('Async', async () => {});
    export function unchecked(text: string) { const data = JSON.parse(text); return data; }
  `);
  assert.notEqual(rejected.status, 0, 'unsafe fixtures must fail the actual configured rules');
  for (const rule of [
    'no-void',
    'no-floating-promises',
    'no-misused-promises',
    'no-unsafe-assignment',
    'no-unsafe-return',
  ]) {
    assert.ok(rejected.output.includes(rule), `Expected ${rule}: ${rejected.output}`);
  }
});
