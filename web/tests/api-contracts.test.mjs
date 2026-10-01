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
const invite = {
  id: '11111111-1111-4111-8111-111111111111',
  uses_left: 1,
  expires_at: '2026-10-05T00:00:00Z',
  created_at: '2026-09-28T00:00:00Z',
};
const roomInvite = { ...invite, role: 'member' };
const redemption = { room_id: 'room', display_name: 'Room', role: 'member' };
const preferences = {
  allowPrivateMessages: false,
  sounds: true,
  largeText: false,
  timestamps: 'seconds',
  ignored: [{ id: 'person', name: 'Person' }],
};
const endpoints = [
  [
    'public profile',
    (api) => api.publicProfile('user /?'),
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
    (api) => api.rooms(new URLSearchParams({ page: '1', q: 'test room' })),
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
  [
    'chat preferences',
    (api) => api.accountPreferences('token'),
    '/api/auth/preferences',
    'GET',
    preferences,
  ],
  [
    'chat preferences update',
    (api) => api.updatePreferences('token', preferences),
    '/api/auth/preferences',
    'PUT',
    preferences,
  ],
  [
    'registration invites',
    (api) => api.registrationInvites('token'),
    '/api/auth/invites',
    'GET',
    [invite],
  ],
  [
    'registration invite creation',
    (api) => api.createRegistrationInvite('token'),
    '/api/auth/invites',
    'POST',
    { ...invite, code: 'a'.repeat(32) },
  ],
  [
    'memberships',
    (api) => api.memberships('token'),
    '/api/rooms/memberships',
    'GET',
    { items: [{ ...room, role: 'member' }], next_cursor: null },
  ],
  [
    'room invites',
    (api) => api.roomInvites('token', 'room /?'),
    '/api/rooms/room%20%2F%3F/invites',
    'GET',
    [roomInvite],
  ],
  [
    'room invite creation',
    (api) => api.createRoomInvite('token', 'room', { role: 2, uses: 1, days: 7 }),
    '/api/rooms/room/invites',
    'POST',
    { ...roomInvite, code: 'a'.repeat(32) },
  ],
  [
    'invite preview',
    (api) => api.previewInvite('token', 'a'.repeat(32)),
    '/api/rooms/invites/preview',
    'POST',
    redemption,
  ],
  [
    'invite redemption',
    (api) => api.redeemInvite('token', 'a'.repeat(32)),
    '/api/rooms/invites/redeem',
    'POST',
    redemption,
  ],
];

test('passkey management endpoints have fixed strict contracts and preserve cancellation signals', async () => {
  const { ui, state } = await uiFixture();
  const signal = new AbortController().signal;
  const summary = {
    password_enabled: false,
    recovery_enabled: true,
    passkeys: [{ id: '11111111-1111-4111-8111-111111111111', created_at: '2026-09-23T12:00:00Z' }],
    maximum: 10,
  };
  for (const [invoke, path, body] of [
    [() => ui.api.passkeySettings('token', signal), '/api/auth/passkeys', summary],
    [
      () => ui.api.passkeyAction('token', { operation: { action: 'add' } }, signal),
      '/api/auth/passkeys/start',
      {
        kind: 'authenticate',
        ceremony_id: 'owned',
        options: { publicKey: {}, mediation: 'required' },
      },
    ],
    [
      () => ui.api.passkeyAuthorize('token', { ceremony_id: 'owned', credential: {} }, signal),
      '/api/auth/passkeys/authorize',
      { kind: 'register', ceremony_id: 'owned-next', options: { publicKey: {} } },
    ],
    [
      () => ui.api.passkeyEnroll('token', { ceremony_id: 'owned', credential: {} }, signal),
      '/api/auth/passkeys/enroll',
      { kind: 'added' },
    ],
    [
      () =>
        ui.api.passkeyAction(
          'token',
          {
            operation: { action: 'replace', id: summary.passkeys[0].id },
          },
          signal,
        ),
      '/api/auth/passkeys/start',
      {
        kind: 'replace_registration',
        ceremony_id: 'owned-replacement',
        options: { publicKey: {} },
        recovery_key: 'sc-recovery-' + 'a'.repeat(43),
      },
    ],
    [
      () =>
        ui.api.passkeyEnroll('token', { ceremony_id: 'owned-replacement', credential: {} }, signal),
      '/api/auth/passkeys/enroll',
      { kind: 'replaced' },
    ],
  ]) {
    state.response = { ok: true, status: 200, json: async () => body };
    assert.deepEqual(await invoke(), body);
    assert.equal(state.requests.at(-1)[0], path);
    assert.ok(state.requests.at(-1)[1].signal instanceof AbortSignal);
    assert.equal(state.requests.at(-1)[1].signal.aborted, false);
    state.response.json = async () => ({ ...body, private: 'server secret' });
    await assert.rejects(
      invoke,
      path === '/api/auth/passkeys' ? /invalid data/ : ui.ApiOutcomeUnknownError,
    );
  }
  for (const invalid of [
    { ...summary, password_enabled: 'false' },
    { ...summary, maximum: 10000 },
    { ...summary, passkeys: [...summary.passkeys, ...summary.passkeys] },
    { ...summary, passkeys: [{ ...summary.passkeys[0], id: 'credential-private' }] },
    { ...summary, passkeys: [{ ...summary.passkeys[0], created_at: 'not a date' }] },
  ]) {
    state.response.json = async () => invalid;
    await assert.rejects(() => ui.api.passkeySettings('token', signal), /invalid data/);
  }
  for (const invalid of [
    { kind: 'login' },
    { kind: 'recovery_key', recovery_key: '' },
    { kind: 'authenticate', ceremony_id: '', options: { publicKey: {} } },
    { kind: 'replace_registration', ceremony_id: 'owned', options: { publicKey: {} } },
    {
      kind: 'replace_registration',
      ceremony_id: 'owned',
      options: { publicKey: {} },
      recovery_key: '',
    },
    {
      kind: 'replace_registration',
      ceremony_id: '',
      options: { publicKey: {} },
      recovery_key: 'saved',
    },
    { kind: 'replaced', recovery_key: 'must-not-leak' },
  ]) {
    state.response.json = async () => invalid;
    await assert.rejects(
      () => ui.api.passkeyAction('token', { operation: { action: 'add' } }, signal),
      ui.ApiOutcomeUnknownError,
    );
  }
});

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
    // Public reads carry neither the cookie nor a bearer; everything else sends both.
    const anonymous = name === 'public profile' || name === 'public rooms';
    assert.equal(state.requests[0][1].credentials, anonymous ? 'omit' : 'same-origin');
    assert.equal(
      state.requests[0][1].headers.Authorization,
      anonymous ? undefined : 'Bearer token',
    );
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
        message:
          method === 'GET'
            ? 'The server returned invalid data. Please try again.'
            : new ui.ApiOutcomeUnknownError().message,
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
      message:
        method === 'GET'
          ? 'The server returned invalid data. Please try again.'
          : new ui.ApiOutcomeUnknownError().message,
    });
    state.response = {
      ok: true,
      status: 204,
      json: async () => {
        throw new Error('JSON must not be read');
      },
    };
    await assert.rejects(request(ui.api), {
      message:
        method === 'GET'
          ? 'The server returned invalid data. Please try again.'
          : new ui.ApiOutcomeUnknownError().message,
    });
  });
}

test('directory counts are nullable when a room was busy at listing time', async () => {
  const { ui, state } = await uiFixture();
  state.response = {
    ok: true,
    status: 200,
    json: async () => [{ ...room, participant_count: null, broadcaster_count: null }],
  };
  const [listed] = await ui.api.ownRooms(null);
  assert.equal(listed.participant_count, null);
  assert.equal(listed.broadcaster_count, null);
});

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
  for (const call of calls) await assert.rejects(call(), ui.ApiOutcomeUnknownError);
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

test('capabilities and membership pagination own fixed decoded contracts', async () => {
  const { ui, state } = await uiFixture();
  const capabilities = {
    version: 1,
    accounts: true,
    passwordLogin: true,
    passkeyLogin: false,
    passwordRegistration: 'invite',
    passkeyRegistration: 'disabled',
    roomDirectory: true,
    roomCreation: true,
    adHocRooms: false,
  };
  state.response = {
    ok: true,
    status: 200,
    json: async () => ({ ...capabilities, private: 'removed' }),
  };
  assert.deepEqual(await ui.api.capabilities(), capabilities);
  assert.equal(state.requests[0][0], '/api/capabilities');
  assert.equal(state.requests[0][1].credentials, 'omit');
  for (const wrong of [
    { ...capabilities, version: 2 },
    { ...capabilities, passwordRegistration: 'maybe' },
  ]) {
    state.response = { ok: true, status: 200, json: async () => wrong };
    await assert.rejects(ui.api.capabilities(), /invalid data/);
  }
  state.response = { ok: true, status: 200, json: async () => ({ items: [], next_cursor: null }) };
  assert.deepEqual(await ui.api.memberships('token', 'after /?'), { items: [], next_cursor: null });
  assert.equal(state.requests.at(-1)[0], '/api/rooms/memberships?after=after%20%2F%3F');
  const page = {
    items: [{ ...room, role: 'member' }],
    next_cursor: 'next-room',
  };
  state.response = { ok: true, status: 200, json: async () => page };
  assert.deepEqual(await ui.api.memberships('token'), page);
  assert.equal(state.requests.at(-1)[0], '/api/rooms/memberships');
  for (const invalid of [
    [],
    page.items,
    { ...page, next_cursor: 4 },
    { items: [{ ...room, role: 4 }], next_cursor: null },
    { items: [] },
  ]) {
    state.response = { ok: true, status: 200, json: async () => invalid };
    await assert.rejects(ui.api.memberships('token'), /invalid data/);
  }
});

test('invitation secrets travel only in JSON bodies; revocation uses nonsecret IDs', async () => {
  const { ui, state } = await uiFixture();
  const code = 'a'.repeat(32);
  state.response = { ok: true, status: 200, json: async () => redemption };
  for (const action of ['previewInvite', 'redeemInvite']) {
    await ui.api[action]('token', code);
    const [path, init] = state.requests.at(-1);
    assert.equal(path.includes(code), false);
    assert.deepEqual(JSON.parse(init.body), { code });
  }
  state.response = { ok: true, status: 204 };
  await ui.api.revokeRoomInvite('token', 'room', invite.id);
  assert.equal(state.requests.at(-1)[0], `/api/rooms/room/invites/${invite.id}`);
  await ui.api.revokeRegistrationInvite('token', invite.id);
  assert.equal(state.requests.at(-1)[0], `/api/auth/invites/${invite.id}`);
});

test('WebSocket ticket minting validates its short-lived credential and preserves cancellation', async () => {
  const { ui, state } = await uiFixture();
  const controller = new AbortController();
  const valid = { ticket: 'a'.repeat(43), expires_in: 30 };
  state.response = { ok: true, status: 200, json: async () => valid };
  assert.deepEqual(await ui.api.websocketTicket('private-token', controller.signal), valid);
  const [path, init] = state.requests[0];
  assert.equal(path, '/api/auth/ws-ticket');
  assert.equal(init.method, 'POST');
  assert.equal(init.headers.Authorization, 'Bearer private-token');
  assert.deepEqual(JSON.parse(init.body), {});
  for (const value of [
    { ...valid, ticket: 'a'.repeat(42) },
    { ...valid, ticket: 'a'.repeat(44) },
    { ...valid, ticket: '/'.repeat(43) },
    { ...valid, expires_in: 0 },
    { ...valid, expires_in: 31 },
    { ...valid, expires_in: 1.5 },
    { ...valid, expires_in: '30' },
  ]) {
    state.response.json = async () => value;
    await assert.rejects(
      ui.api.websocketTicket('private-token', controller.signal),
      ui.ApiOutcomeUnknownError,
    );
  }
  controller.abort();
  const before = state.requests.length;
  await assert.rejects(ui.api.websocketTicket('private-token', controller.signal));
  assert.equal(state.requests.length, before);
});
