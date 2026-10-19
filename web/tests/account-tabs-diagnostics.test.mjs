import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('../e2e/auth-cross-tab.cjs', import.meta.url), 'utf8');
const start = source.indexOf('function accountTabsFailure(');
const end = source.indexOf('\nasync function startupRecovery(', start);
assert.ok(start >= 0 && end > start);
const diagnostic = runInNewContext(`${source.slice(start, end)}; accountTabsFailure`);
const plain = (value) => JSON.parse(JSON.stringify(value));

test('the departing account document keeps its final counters across sign-out reload', () => {
  const helperStart = source.indexOf('function installAccountUnloadReceipt(');
  const helperEnd = source.indexOf('\nfunction accountSession(', helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart);
  let pagehide;
  const values = new Map();
  const evidence = { captures: 0, joins: 0, hints: [], privateValue: 'owned-secret' };
  const install = runInNewContext(
    `${source.slice(helperStart, helperEnd)}; installAccountUnloadReceipt`,
    {
      window: {
        __crossTabEvidence: evidence,
        addEventListener(name, callback, options) {
          assert.equal(name, 'pagehide');
          assert.equal(options.once, true);
          pagehide = callback;
        },
      },
      sessionStorage: { setItem: (key, value) => values.set(key, value) },
    },
  );
  install('fixture-key');
  assert.equal(values.size, 0, 'the receipt must include changes made during sign out');
  evidence.hints.push({ version: 1, revision: 'a'.repeat(32) });
  evidence.captures = 1;
  pagehide();
  assert.deepEqual(JSON.parse(values.get('fixture-key')), {
    captures: 1,
    joins: 0,
    hints: [{ version: 1, revision: 'a'.repeat(32) }],
  });
  evidence.captures = 0;
  assert.equal(
    JSON.parse(values.get('fixture-key')).captures,
    1,
    'new document counters cannot erase the departing evidence',
  );
  assert.doesNotMatch(values.get('fixture-key'), /owned-secret|privateValue/);
});

test('account tab failures retain the exact fixture frame and fixed phase without account data', () => {
  const error = new Error('page.evaluate: Execution context was destroyed; owned-secret-token');
  error.stack =
    'owned-secret-path?token=owned-secret-token\n    at scenario (/private/owned-secret/auth-cross-tab.cjs:487:17)';
  const value = diagnostic(error, 'broadcast', 'rapid-room-hashes', 'firefox');
  assert.deepEqual(plain(value), {
    kind: 'account-tabs-failure',
    browser: 'firefox',
    check: 'broadcast',
    phase: 'rapid-room-hashes',
    sourceLine: 487,
    failure: 'execution-context-destroyed',
  });
  assert.doesNotMatch(JSON.stringify(value), /owned-secret|private/);
});

test('unknown check metadata and non-context errors receive fixed labels', () => {
  const value = diagnostic(
    new Error('owned-secret-email@example.test'),
    'owned-secret-check',
    'owned-secret-phase',
    'owned-secret-browser',
  );
  assert.deepEqual(plain(value), {
    kind: 'account-tabs-failure',
    browser: 'unknown',
    check: 'unknown',
    phase: 'unknown',
    sourceLine: null,
    failure: 'check-failed',
  });
  assert.doesNotMatch(JSON.stringify(value), /owned-secret/);
});
