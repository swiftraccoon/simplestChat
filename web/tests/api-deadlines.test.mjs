import assert from 'node:assert/strict';
import test from 'node:test';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

async function timedFixture(extra = {}) {
  const timers = new Map();
  let next = 0;
  const fixture = await uiFixture({
    setTimeout: (callback) => {
      timers.set(++next, callback);
      return next;
    },
    clearTimeout: (id) => timers.delete(id),
    ...extra,
  });
  return {
    ...fixture,
    expire: () => {
      for (const callback of [...timers.values()]) callback();
    },
    timers,
  };
}

for (const stage of ['headers', 'body']) {
  test(`HTTP read deadline owns stalled ${stage} and consumes a late completion`, async () => {
    const held = deferred();
    const f = await timedFixture(stage === 'headers' ? { fetch: () => held.promise } : {});
    f.state.response = { ok: true, status: 200, json: () => held.promise };
    const result = f.ui.api.ownRooms(null);
    const rejected = assert.rejects(result, /took too long/);
    await flush();
    f.expire();
    await rejected;
    held.resolve(stage === 'headers' ? { ok: true, status: 200, json: async () => [] } : []);
    await flush();
    assert.equal(f.timers.size, 0);
  });
}

test('caller cancellation aborts the underlying read and clears its deadline', async () => {
  let ownedSignal;
  const f = await timedFixture({
    fetch: (_path, options) => {
      ownedSignal = options.signal;
      return new Promise(() => {});
    },
  });
  const controller = new AbortController();
  const result = f.ui.api.rooms(new URLSearchParams(), controller.signal);
  const rejected = assert.rejects(result, { name: 'AbortError' });
  controller.abort();
  await rejected;
  assert.equal(ownedSignal.aborted, true);
  assert.equal(f.timers.size, 0);
});

test('mutation deadlines and transport failures remain uncertain and prevent an immediate busy retry', async () => {
  const held = deferred();
  const f = await timedFixture({ fetch: () => held.promise });
  const submit = f.document.createElement('button');
  const error = f.document.createElement('p');
  let attempts = 0;
  const perform = () => {
    attempts++;
    return f.ui.api.deleteRoom('owned', 'token');
  };
  const work = f.ui.busy(submit, error, perform);
  f.expire();
  await work;
  assert.equal(submit.disabled, true);
  assert.match(error.textContent, /may have completed/);
  await f.ui.busy(submit, error, perform);
  assert.equal(attempts, 1);
  held.resolve({ ok: true, status: 204 });
  await flush();
  assert.equal(submit.disabled, true);
});

for (const status of [400, 403, 409, 429, 408, 500, 503]) {
  test(`mutation HTTP ${status} preserves definite rejection versus uncertainty`, async () => {
    const f = await timedFixture();
    f.state.response = { ok: false, status, text: async () => '{"error":"Fixture failure"}' };
    await assert.rejects(
      f.ui.api.deleteRoom('owned', 'token'),
      status === 408 || status >= 500
        ? f.ui.ApiOutcomeUnknownError
        : (error) => error instanceof f.ui.ApiError && error.status === status,
    );
    assert.equal(f.timers.size, 0);
  });
}
