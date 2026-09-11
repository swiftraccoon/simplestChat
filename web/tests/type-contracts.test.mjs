import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

test('the installed production TypeScript compiler enforces positive and negative API/social contracts', async (t) => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-types.'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const config = path.join(temporary, 'tsconfig.json');
  await writeFile(
    config,
    JSON.stringify({
      extends: fileURLToPath(new URL('../tsconfig.app.json', import.meta.url)),
      compilerOptions: {
        incremental: true,
        tsBuildInfoFile: path.join(temporary, 'types.tsbuildinfo'),
        // Keep the same Vite ambient types; resolve them from the checkout rather
        // than the isolated temporary configuration's node_modules directory.
        types: [fileURLToPath(new URL('../node_modules/vite/client.d.ts', import.meta.url))],
      },
      files: [fileURLToPath(new URL('./type-contracts.ts', import.meta.url))],
      include: [],
    }),
  );
  const result = spawnSync(
    process.execPath,
    [
      fileURLToPath(new URL('../node_modules/typescript/bin/tsc', import.meta.url)),
      '--project',
      config,
      '--pretty',
      'false',
    ],
    { encoding: 'utf8', timeout: 30_000 },
  );
  assert.equal(result.error, undefined, result.stderr);
  assert.equal(result.signal, null, result.stderr);
  assert.equal(result.status, 0, result.stdout + result.stderr);
});
