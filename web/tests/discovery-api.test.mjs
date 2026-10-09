import assert from 'node:assert/strict';
import test from 'node:test';
import { uiFixture } from './ui-fixture.mjs';

const account = '11111111-1111-4111-8111-111111111111';

test('discovery reads use fixed decoded account routes', async () => {
  const f = await uiFixture();
  const signal = new AbortController().signal;
  for (const [method, path, body] of [
    ['contacts', '/api/auth/contacts', { accountId: account, contacts: [] }],
    ['savedRooms', '/api/auth/saved-rooms', { rooms: [] }],
  ]) {
    f.state.response = { ok: true, status: 200, json: async () => body };
    assert.deepEqual(await f.ui.api[method]('owned-token', signal), body);
    const [url, options] = f.state.requests.at(-1);
    assert.equal(url, path);
    assert.equal(options.headers.Authorization, 'Bearer owned-token');
    assert.equal(options.method, 'GET');
    f.state.response.json = async () => ({});
    await assert.rejects(f.ui.api[method]('owned-token', signal), /invalid data/);
  }
});

test('discovery mutations encode identifiers and preserve exact payloads', async () => {
  const f = await uiFixture();
  const signal = new AbortController().signal;
  f.state.response = { ok: true, status: 204 };
  for (const [run, url, method, body] of [
    [
      () => f.ui.api.requestContact('token', account, signal),
      '/api/auth/contacts',
      'POST',
      { accountId: account },
    ],
    [
      () => f.ui.api.acceptContact('token', 'peer /?', signal),
      '/api/auth/contacts/peer%20%2F%3F',
      'PUT',
      undefined,
    ],
    [
      () => f.ui.api.removeContact('token', 'peer /?', signal),
      '/api/auth/contacts/peer%20%2F%3F',
      'DELETE',
      undefined,
    ],
    [
      () => f.ui.api.saveRoom('token', 'room /?', true, signal),
      '/api/auth/saved-rooms/room%20%2F%3F',
      'PUT',
      { favorite: true },
    ],
  ]) {
    await run();
    const [actual, options] = f.state.requests.at(-1);
    assert.equal(actual, url);
    assert.equal(options.method, method);
    assert.deepEqual(options.body ? JSON.parse(options.body) : undefined, body);
    assert.equal(options.headers.Authorization, 'Bearer token');
  }
});
