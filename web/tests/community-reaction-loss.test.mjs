import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('../e2e/community.cjs', import.meta.url), 'utf8');
const start = source.indexOf('          const reactionPath =');
const end = source.indexOf('          await sender.route(historyRoute, holdCanonical);', start);
assert.ok(start >= 0 && end > start);
const plain = (value) => JSON.parse(JSON.stringify(value));
function fixture() {
  return runInNewContext(
    `${source.slice(start, end)}; ({ setReaction, holdCanonical, historyRoute,
    release: () => releaseCanonical(), failure: () => reactionFailure, attempts: reactionAttempts })`,
    {
      assert: {
        ...assert,
        deepEqual: (actual, expected) => assert.deepEqual(plain(actual), plain(expected)),
      },
      base: 'https://owned.invalid',
      endpoint: '/api/auth/inbox/peer/messages',
      committed: { messageId: 'saved', sender: 'account' },
      URL,
    },
  );
}
function mutation(body = { emoji: '👍', present: true }) {
  const calls = [];
  return {
    calls,
    request: () => ({ postDataJSON: () => body }),
    async fetch() {
      calls.push('fetch');
      return {
        status: () => 200,
        json: async () => ({
          messageId: 'saved',
          reactions: [{ emoji: '👍', participantIds: ['account'] }],
        }),
      };
    },
    async abort() {
      calls.push('abort');
    },
    async fulfill() {
      calls.push('fulfill');
    },
  };
}

test('lost reaction response withholds independent confirmation until a second explicit presence request', async () => {
  const f = fixture();
  let continued = 0;
  const read = f.holdCanonical({
    request: () => ({ method: () => 'GET' }),
    continue: async () => {
      continued++;
    },
  });
  await Promise.resolve();
  assert.equal(continued, 0);
  const first = mutation();
  await f.setReaction(first);
  assert.deepEqual(first.calls, ['fetch', 'abort']);
  assert.equal(continued, 0, 'a lost receipt cannot release independent confirmation before retry');
  const second = mutation();
  await f.setReaction(second);
  await read;
  assert.deepEqual(second.calls, ['fetch', 'fulfill']);
  assert.deepEqual(plain(f.attempts), [
    { emoji: '👍', present: true },
    { emoji: '👍', present: true },
  ]);
  assert.equal(continued, 1);
  assert.equal(f.failure(), undefined);
});

test('failed reaction assertions stay owned by the step and cleanup releases pending history', async () => {
  const f = fixture();
  const bad = mutation({ emoji: '👍', present: false });
  await f.setReaction(bad);
  assert.ok(f.failure());
  assert.deepEqual(bad.calls, ['abort']);
  let continued = false;
  const pending = f.holdCanonical({
    request: () => ({ method: () => 'GET' }),
    continue: async () => {
      continued = true;
    },
  });
  f.release();
  await pending;
  assert.equal(continued, true);
  let fallback = false;
  await f.holdCanonical({
    request: () => ({ method: () => 'POST' }),
    fallback: async () => {
      fallback = true;
    },
  });
  assert.equal(fallback, true, 'unrelated mutations are never held');
  assert.equal(
    f.historyRoute(new URL('https://foreign.invalid/api/auth/inbox/peer/messages')),
    false,
  );
  assert.equal(
    f.historyRoute(new URL('https://owned.invalid/api/auth/inbox/peer/messages?resume=true')),
    true,
  );
});
