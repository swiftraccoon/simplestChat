import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));

// The orchestrator's owned-checkout, engine and receipt regressions now execute
// its actual Rust modules in xtask/src/ci_local/ci_local_tests.rs.
test('container helper entrypoints reject ordinary host invocation before starting tools or sockets', () => {
  for (const helper of ['ci-local-docker.sh', 'ci-local-postgres.sh']) {
    const result = spawnSync('/bin/bash', [path.join(root, 'build', helper)], {
      env: { PATH: process.env.PATH }, encoding: 'utf8', timeout: 5000,
    });
    assert.equal(result.status, 2);
    assert.match(result.stderr, /only for disposable local CI runners/);
  }
});
