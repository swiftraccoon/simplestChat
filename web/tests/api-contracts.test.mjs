import assert from 'node:assert/strict';
import test from 'node:test';
import { uiFixture } from './ui-fixture.mjs';

const profile = { id: 'account', display_name: 'Person', avatar_url: null, bio: '' };
const account = { ...profile, email: 'person@example.test', recovery_enabled: false };
const room = {
  id: 'room',
  display_name: 'Room',
  topic: null,
  participant_count: 0,
  password_protected: false,
  moderated: false,
  broadcaster_count: 0,
  description: '',
  image_url: null,
  secret: false,
};
const settings = {
  id: 'room',
  ownerId: 'account',
  displayName: 'Room',
  passwordProtected: false,
  requireRegistration: false,
  maxParticipants: null,
  maxBroadcasters: null,
  allowScreenSharing: true,
  allowChat: true,
  allowVideo: true,
  moderated: false,
  inviteOnly: false,
  secret: false,
  lobbyEnabled: false,
  pushToTalk: false,
  guestsAllowed: true,
  guestsCanBroadcast: true,
  topic: null,
};
const endpoints = [
  [
    'public profile',
    (api) => api.publicProfile('user /?', 'token'),
    '/api/auth/profiles/user%20%2F%3F',
    'GET',
    profile,
  ],
  ['account profile', (api) => api.accountProfile('token'), '/api/auth/profile', 'GET', account],
  [
    'profile update',
    (api) => api.updateProfile('token', profile),
    '/api/auth/profile',
    'PATCH',
    account,
  ],
  [
    'recovery key',
    (api) => api.recoveryKey('token', { current_password: 'fixture' }),
    '/api/auth/recovery/key',
    'POST',
    { recovery_key: 'saved-key' },
  ],
  [
    'public rooms',
    (api) => api.rooms('token', new URLSearchParams({ page: '1', q: 'test room' })),
    '/api/rooms?page=1&q=test+room',
    'GET',
    [room],
  ],
  ['owned rooms', (api) => api.ownRooms('token'), '/api/rooms/mine', 'GET', [room]],
  [
    'room creation',
    (api) => api.createRoom('token', { id: 'room', display_name: 'Room' }),
    '/api/rooms',
    'POST',
    settings,
  ],
  [
    'room identity',
    (api) => api.updateRoomIdentity('room /?', 'token', room),
    '/api/rooms/room%20%2F%3F/identity',
    'PATCH',
    room,
  ],
];

for (const [name, request, path, method, valid] of endpoints) {
  test(`${name} uses its fixed endpoint decoder and never forwards unknown fields`, async () => {
    const { ui, state } = await uiFixture();
    const wire = Array.isArray(valid)
      ? valid.map((entry) => ({ ...entry, internal: 'private' }))
      : { ...valid, internal: 'private' };
    const before = structuredClone(wire);
    state.response = { ok: true, status: 200, json: async () => wire };
    const value = await request(ui.api);
    assert.equal(state.requests[0][0], path);
    assert.equal(state.requests[0][1].method, method);
    assert.equal(state.requests[0][1].credentials, 'same-origin');
    assert.equal(state.requests[0][1].headers.Authorization, 'Bearer token');
    assert.deepEqual(wire, before, 'decoding must not mutate a response object');
    for (const item of Array.isArray(value) ? value : [value]) {
      assert.equal(Object.hasOwn(item, 'internal'), false);
      assert.equal(Object.hasOwn(item, 'ownerId'), false);
    }
    if (name === 'room creation') {
      assert.equal(
        Object.hasOwn(value, 'maxParticipants'),
        false,
        'Rust null limits normalize to optional values',
      );
      assert.equal(value.allowChat, true);
    } else assert.deepEqual(value, valid);
  });

  test(`${name} rejects incomplete, mistyped, malformed and empty JSON without leaking its contents`, async () => {
    const { ui, state } = await uiFixture();
    for (const payload of [
      null,
      false,
      'private server fragment',
      { internal: 'private' },
      Array.isArray(valid) ? [{}] : [],
    ]) {
      state.response = { ok: true, status: 200, json: async () => payload };
      await assert.rejects(request(ui.api), {
        message: 'The server returned invalid data. Please try again.',
      });
    }
    state.response = {
      ok: true,
      status: 200,
      json: async () => {
        throw new SyntaxError('private parser fragment');
      },
    };
    await assert.rejects(request(ui.api), {
      message: 'The server returned invalid data. Please try again.',
    });
    state.response = {
      ok: true,
      status: 204,
      json: async () => {
        throw new Error('JSON must not be read');
      },
    };
    await assert.rejects(request(ui.api), {
      message: 'The server returned invalid data. Please try again.',
    });
  });
}

test('all serialized profile and directory fields remain required, including nullable values', async () => {
  const { ui, state } = await uiFixture();
  for (const [request, valid] of [
    [() => ui.api.accountProfile(null), account],
    [() => ui.api.publicProfile('account', null), profile],
    [() => ui.api.ownRooms(null), room],
  ]) {
    for (const key of Object.keys(valid)) {
      const incomplete = { ...valid };
      delete incomplete[key];
      state.response = {
        ok: true,
        status: 200,
        json: async () => (valid === room ? [incomplete] : incomplete),
      };
      await assert.rejects(request(), /server returned invalid data/, `${key} is required`);
    }
  }
  for (const patch of [
    { participant_count: -1 },
    { participant_count: 1.5 },
    { participant_count: Number.MAX_SAFE_INTEGER + 1 },
    { broadcaster_count: NaN },
    { description: null },
    { image_url: {} },
    { secret: 0 },
  ]) {
    state.response = { ok: true, status: 200, json: async () => [{ ...room, ...patch }] };
    await assert.rejects(ui.api.ownRooms(null), /server returned invalid data/);
  }
  for (const patch of [
    { avatar_url: false },
    { recovery_enabled: 'false' },
    { bio: null },
    { email: 42 },
  ]) {
    state.response = { ok: true, status: 200, json: async () => ({ ...account, ...patch }) };
    await assert.rejects(ui.api.accountProfile(null), /server returned invalid data/);
  }
});

test('only password, recovery redemption and deletion accept 204 and never parse its body', async () => {
  const { ui, state } = await uiFixture();
  const calls = [
    () => ui.api.changePassword('token', { current_password: 'old', new_password: 'new' }),
    () =>
      ui.api.redeemRecovery({
        email: 'person@example.test',
        recovery_key: 'key',
        new_password: 'new',
      }),
    () => ui.api.deleteRoom('room /?', 'token'),
  ];
  let parses = 0;
  state.response = {
    ok: true,
    status: 204,
    json: async () => {
      parses++;
      throw new Error('No body');
    },
  };
  for (const call of calls) assert.equal(await call(), undefined);
  assert.equal(parses, 0);
  assert.deepEqual(
    state.requests.map(([path, options]) => [path, options.method]),
    [
      ['/api/auth/password', 'POST'],
      ['/api/auth/recovery/redeem', 'POST'],
      ['/api/rooms/room%20%2F%3F', 'DELETE'],
    ],
  );
  assert.equal(Object.hasOwn(state.requests[1][1].headers, 'Authorization'), false);
  assert.equal(Object.hasOwn(state.requests[2][1], 'body'), false);
  state.response = { ok: true, status: 200, json: async () => ({ accepted: true }) };
  for (const call of calls) await assert.rejects(call(), /unexpected response/);
});

test('non-string JSON errors retain bounded plain text and network failures keep their identity', async () => {
  const { ui, state } = await uiFixture();
  for (const raw of ['{"error":null}', '{"error":{"detail":"bad"}}', '{"error":42}']) {
    state.response = { ok: false, status: 429, text: async () => raw };
    await assert.rejects(
      ui.api.ownRooms(null),
      (error) => error instanceof ui.ApiError && error.status === 429 && error.message === raw,
    );
  }
  const failure = new Error('Disconnected');
  const network = await uiFixture({
    fetch: async () => {
      throw failure;
    },
  });
  await assert.rejects(network.ui.api.ownRooms(null), (error) => error === failure);
});
