import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../../build/with-test-server.sh', import.meta.url), 'utf8');
const profile = source.match(
  /^if \[\[ "\$\{PRESENTATION_E2E:-\}" == 1 \]\]; then\n[\s\S]*?^fi$/m,
)?.[0];
assert.ok(profile, 'the presentation profile is an isolated explicit opt-in block');

function environment(value) {
  // Evaluate only the extracted environment-array block. This never executes
  // the wrapper, a test server, a database, native media or browser code.
  return execFileSync(
    '/bin/bash',
    [
      '-c',
      `set -euo pipefail\ntest_metrics_environment=("PATH=/owned-test-bin")\n${profile}\nprintf '%s' "\${test_metrics_environment[*]}"`,
    ],
    {
      encoding: 'utf8',
      env: {
        PATH: process.env.PATH,
        PRESENTATION_E2E: value,
        RUST_LOG: 'untrusted-operator-filter=trace',
      },
    },
  );
}

test('owned presentation diagnostics use only the fixed bandwidth log profile', () => {
  assert.equal(
    environment('1'),
    'PATH=/owned-test-bin RUST_LOG=simplestChat=info,mediasoup=warn,sqlx::pool=warn,simplestChat::signaling::connection=debug,simplestChat::media::transport_manager=debug',
  );
  assert.ok(!profile.includes('${RUST_LOG'));
  assert.ok(!profile.includes('$RUST_LOG'));
  assert.match(source, /exec env -i "\$\{test_metrics_environment\[@\]\}"/);
});

test('presentation diagnostics stay disabled outside exact explicit opt-in', () => {
  for (const value of ['', '0', 'true', '01'])
    assert.equal(environment(value), 'PATH=/owned-test-bin');
});
