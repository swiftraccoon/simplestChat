import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const REFRESH_MS = 12 * 60 * 1000;
const REPLAY_MS = 2250;
const superseded = /Authentication request superseded/;

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

async function flush() {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

function session(name) {
  return {
    token: `jwt-${name}`,
    user: { id: name, email: `${name}@example.test`, display_name: name },
  };
}

function response(data, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
    clone() {
      return response(data, status);
    },
  };
}

class AuthenticatorAttestationResponse {
  clientDataJSON = new Uint8Array([1, 2]).buffer;
  attestationObject = new Uint8Array([3, 4]).buffer;
}

class AuthenticatorAssertionResponse {
  clientDataJSON = new Uint8Array([1, 2]).buffer;
  authenticatorData = new Uint8Array([3, 4]).buffer;
  signature = new Uint8Array([5, 6]).buffer;
  userHandle = new Uint8Array([7, 8]).buffer;
}

class PublicKeyCredential {
  id = 'credential';
  rawId = new Uint8Array([9, 10]).buffer;
  type = 'public-key';
  constructor(value = new AuthenticatorAssertionResponse()) {
    this.response = value;
  }
}

function passkeyOptions(kind) {
  return {
    ceremony_id: `ceremony-${kind}`,
    publicKey:
      kind === 'register'
        ? {
            challenge: 'AQI',
            rp: { name: 'Test', id: 'localhost' },
            user: { id: 'AwQ', name: 'person@example.test', displayName: 'Person' },
            pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
            excludeCredentials: [{ type: 'public-key', id: 'BQY', transports: ['internal'] }],
            authenticatorSelection: { userVerification: 'required' },
          }
        : {
            challenge: 'AQI',
            rpId: 'localhost',
            allowCredentials: [{ type: 'public-key', id: 'BQY', transports: ['internal'] }],
            userVerification: 'required',
            timeout: 60000,
          },
  };
}

async function fixture(t, { locks } = {}) {
  const requests = [];
  const queued = [];
  const timers = new Map();
  const changes = [];
  let nextTimer = 0;
  let now = 0;
  const { AuthManager } = await loadTypeScript('src/auth.ts', {
    globals: {
      fetch: (url, options) => {
        requests.push({ url, options });
        assert.ok(queued.length, `Unexpected request: ${url}`);
        return Promise.resolve(queued.shift());
      },
      navigator: locks ? { locks } : {},
      Date: class extends Date {
        static now() {
          return now;
        }
      },
      setTimeout: (callback, milliseconds) => {
        const id = nextTimer++;
        timers.set(id, { callback, milliseconds });
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
      atob,
      btoa,
      PublicKeyCredential,
      AuthenticatorAttestationResponse,
      AuthenticatorAssertionResponse,
    },
  });
  const auth = new AuthManager();
  auth.setOnChange((...args) => changes.push(args));
  t.after(() => auth.forgetSession());
  return {
    auth,
    requests,
    timers,
    changes,
    elapse: (milliseconds) => {
      now += milliseconds;
    },
    enqueue: (value) => queued.push(value),
    async login(name = 'initial') {
      queued.push(response(session(name)));
      await auth.login(`${name}@example.test`, 'password');
    },
    async fire(milliseconds) {
      const timer = [...timers.entries()].find(([, value]) => value.milliseconds === milliseconds);
      assert.ok(timer, `Missing ${milliseconds}ms timer`);
      timers.delete(timer[0]);
      now += milliseconds;
      timer[1].callback();
      await flush();
    },
  };
}

test('login validates identity and refreshes with the existing token-refresh notification', async (t) => {
  const f = await fixture(t);
  await f.login();
  assert.equal(f.auth.userId, 'initial');
  assert.deepEqual(f.changes, [[true, false]]);
  f.enqueue(response(session('rotated')));
  await f.fire(REFRESH_MS);
  assert.equal(f.auth.jwt, 'jwt-rotated');
  assert.deepEqual(f.changes, [
    [true, false],
    [true, true],
  ]);
  assert.equal(f.timers.size, 1);
  assert.deepEqual(f.requests[1], {
    url: '/api/auth/refresh',
    options: { method: 'POST', credentials: 'include' },
  });
});

for (const action of ['login', 'register', 'restore']) {
  test(`forgetSession retires a pending ${action} response`, async (t) => {
    const f = await fixture(t);
    const gate = deferred();
    f.enqueue(gate.promise);
    const pending =
      action === 'restore'
        ? f.auth.tryRestore()
        : action === 'register'
          ? f.auth.register('person@example.test', 'Person', 'password')
          : f.auth.login('person@example.test', 'password');
    const settled = action === 'restore' ? pending : assert.rejects(pending, superseded);
    f.auth.forgetSession();
    gate.resolve(response(session('retired')));
    if (action === 'restore') assert.equal(await settled, false);
    else await settled;
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.auth.jwt, null);
    assert.equal(f.timers.size, 0);
    assert.deepEqual(f.changes, [[false, false]]);
  });
}

for (const boundary of ['response', 'body']) {
  test(`newer login wins when the older ${boundary} resolves last`, async (t) => {
    const f = await fixture(t);
    const gate = deferred();
    f.enqueue(
      boundary === 'response' ? gate.promise : { ...response(null), json: () => gate.promise },
    );
    const pending = assert.rejects(f.auth.login('old@example.test', 'password'), superseded);
    await flush();
    await f.login('new');
    gate.resolve(boundary === 'response' ? response(session('old')) : session('old'));
    await pending;
    assert.equal(f.auth.userId, 'new');
    assert.deepEqual(f.changes, [[true, false]]);
    assert.equal(f.timers.size, 1);
  });
}

test('a late restore cannot overwrite an interactive login', async (t) => {
  const f = await fixture(t);
  const body = deferred();
  f.enqueue({ ...response(null), json: () => body.promise });
  const restore = f.auth.tryRestore();
  await flush();
  await f.login('new');
  body.resolve(session('old'));
  assert.equal(await restore, false);
  assert.equal(f.auth.userId, 'new');
});

test('late logout success cannot clear a newer login', async (t) => {
  const f = await fixture(t);
  await f.login();
  const gate = deferred();
  f.enqueue(gate.promise);
  const logout = assert.rejects(f.auth.logout(), superseded);
  await f.login('new');
  gate.resolve(response(null, 204));
  await logout;
  assert.equal(f.auth.userId, 'new');
  assert.deepEqual(f.changes, [
    [true, false],
    [true, false],
  ]);
});

for (const failure of ['http', 'network']) {
  test(`failed logout (${failure}) preserves identity and resumes refresh`, async (t) => {
    const f = await fixture(t);
    await f.login();
    const gate = deferred();
    f.enqueue(gate.promise);
    const logout = assert.rejects(
      f.auth.logout(),
      failure === 'http' ? /could not revoke/ : /offline/,
    );
    assert.equal(f.timers.size, 0);
    if (failure === 'http') gate.resolve(response(null, 500));
    else gate.reject(new Error('offline'));
    await logout;
    assert.equal(f.auth.userId, 'initial');
    assert.equal(f.timers.size, 1);
    assert.deepEqual(f.changes, [[true, false]]);
  });
}

for (const outcome of ['success', 'failure', 'network', 'body']) {
  test(`retired scheduled refresh (${outcome}) cannot change a newer login`, async (t) => {
    const f = await fixture(t);
    await f.login();
    const gate = deferred();
    f.enqueue(outcome === 'body' ? { ...response(null), json: () => gate.promise } : gate.promise);
    await f.fire(REFRESH_MS);
    await f.login('new');
    if (outcome === 'network') gate.reject(new Error('offline'));
    else
      gate.resolve(
        outcome === 'body'
          ? session('old')
          : response(session('old'), outcome === 'failure' ? 403 : 200),
      );
    await flush();
    assert.equal(f.auth.userId, 'new');
    assert.deepEqual(f.changes, [
      [true, false],
      [true, false],
    ]);
    assert.equal(f.timers.size, 1);
  });
}

test('successful logout retires an in-flight scheduled refresh', async (t) => {
  const f = await fixture(t);
  await f.login();
  const gate = deferred();
  f.enqueue(gate.promise);
  await f.fire(REFRESH_MS);
  f.enqueue(response(null, 204));
  await f.auth.logout();
  gate.resolve(response(session('retired')));
  await flush();
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
  assert.deepEqual(f.changes, [
    [true, false],
    [false, false],
  ]);
});

test('a refresh retired while waiting for its Web Lock does not fetch', async (t) => {
  const waiting = deferred();
  const names = [];
  const f = await fixture(t, {
    locks: {
      request: async (name, request) => {
        names.push(name);
        await waiting.promise;
        return request();
      },
    },
  });
  const restore = f.auth.tryRestore();
  f.auth.forgetSession();
  waiting.resolve();
  assert.equal(await restore, false);
  assert.deepEqual(names, ['simplestchat-refresh-v1']);
  assert.equal(f.requests.length, 0);
});

test('exact Invalid token refresh rejection retries after the existing replay grace under Web Locks', async (t) => {
  const names = [];
  const f = await fixture(t, {
    locks: {
      request: async (name, request) => {
        names.push(name);
        return request();
      },
    },
  });
  f.enqueue(response({ error: 'Invalid token' }, 401));
  const restore = f.auth.tryRestore();
  await flush();
  assert.equal(f.requests.length, 1);
  f.enqueue(response(session('restored')));
  await f.fire(REPLAY_MS);
  assert.equal(await restore, true);
  assert.deepEqual(names, ['simplestchat-refresh-v1', 'simplestchat-refresh-v1']);
  assert.equal(f.auth.userId, 'restored');
});

test('retirement during replay grace prevents a second refresh request', async (t) => {
  const f = await fixture(t);
  f.enqueue(response({ error: 'Invalid token' }, 401));
  const restore = f.auth.tryRestore();
  await flush();
  f.auth.forgetSession();
  await f.fire(REPLAY_MS);
  assert.equal(await restore, false);
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 0);
});

test('other refresh failures do not replay and clear only the current session', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ error: 'Expired token' }, 401));
  await f.fire(REFRESH_MS);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

test('malformed authentication JSON never partially replaces a session', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ token: 'new', user: { id: 'new' } }));
  await assert.rejects(f.auth.login('new@example.test', 'password'), /incomplete/);
  assert.equal(f.auth.userId, 'initial');
  assert.equal(f.auth.jwt, 'jwt-initial');
  assert.equal(f.timers.size, 1);
  f.enqueue(response({ error: { unexpected: true } }, 401));
  await assert.rejects(f.auth.login('new@example.test', 'password'), /^Error: Login failed$/);
});

test('failed logout preserves the original refresh deadline instead of extending JWT lifetime', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.elapse(REFRESH_MS - 1000);
  f.enqueue(response(null, 500));
  await assert.rejects(f.auth.logout(), /could not revoke/);
  assert.equal([...f.timers.values()][0].milliseconds, 1000);
  f.enqueue(response(session('refreshed')));
  await f.fire(1000);
  assert.equal(f.auth.userId, 'refreshed');
});

test('recovery session retirement from an auth callback leaves no new refresh timer', async (t) => {
  const f = await fixture(t);
  f.auth.setOnChange((loggedIn) => {
    if (loggedIn) f.auth.forgetSession();
  });
  await f.login();
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
});

for (const kind of ['register', 'login']) {
  test(`passkey ${kind} preserves binary serialization and consumes its ceremony once`, async (t) => {
    const f = await fixture(t);
    const options = passkeyOptions(kind);
    const original = structuredClone(options);
    f.enqueue(response(options));
    const decoded =
      kind === 'register'
        ? await f.auth.passkeyRegisterStart('person@example.test', 'Person')
        : await f.auth.passkeyLoginStart('person@example.test');
    assert.deepEqual([...new Uint8Array(decoded.publicKey.challenge)], [1, 2]);
    assert.deepEqual(options, original, 'server JSON is not mutated');
    const credential = new PublicKeyCredential(
      kind === 'register'
        ? new AuthenticatorAttestationResponse()
        : new AuthenticatorAssertionResponse(),
    );
    f.enqueue(response(session('passkey')));
    const finish = () =>
      kind === 'register'
        ? f.auth.passkeyRegisterFinish(credential)
        : f.auth.passkeyLoginFinish(credential);
    await finish();
    const body = JSON.parse(f.requests[1].options.body);
    assert.equal(body.ceremony_id, `ceremony-${kind}`);
    assert.equal(body.credential.rawId, 'CQo');
    assert.equal(body.credential.response.clientDataJSON, 'AQI');
    assert.equal(
      body.credential.response[kind === 'register' ? 'attestationObject' : 'authenticatorData'],
      'AwQ',
    );
    if (kind === 'login') assert.equal(body.credential.response.userHandle, 'Bwg');
    assert.equal(f.auth.userId, 'passkey');
    await assert.rejects(finish(), /was not started/);
    assert.equal(f.requests.length, 2);
  });

  test(`retired passkey ${kind} start cannot retain a ceremony`, async (t) => {
    const f = await fixture(t);
    const gate = deferred();
    f.enqueue(gate.promise);
    const pending = assert.rejects(
      kind === 'register'
        ? f.auth.passkeyRegisterStart('person@example.test', 'Person')
        : f.auth.passkeyLoginStart('person@example.test'),
      superseded,
    );
    f.auth.forgetSession();
    gate.resolve(response(passkeyOptions(kind)));
    await pending;
    await assert.rejects(
      kind === 'register'
        ? f.auth.passkeyRegisterFinish(new PublicKeyCredential())
        : f.auth.passkeyLoginFinish(new PublicKeyCredential()),
      /was not started/,
    );
    assert.equal(f.requests.length, 1);
  });

  test(`retired passkey ${kind} finish cannot replace a newer login`, async (t) => {
    const f = await fixture(t);
    f.enqueue(response(passkeyOptions(kind)));
    if (kind === 'register') await f.auth.passkeyRegisterStart('person@example.test', 'Person');
    else await f.auth.passkeyLoginStart('person@example.test');
    const gate = deferred();
    f.enqueue(gate.promise);
    const credential = new PublicKeyCredential();
    const pending = assert.rejects(
      kind === 'register'
        ? f.auth.passkeyRegisterFinish(credential)
        : f.auth.passkeyLoginFinish(credential),
      superseded,
    );
    await f.login('new');
    gate.resolve(response(session('passkey')));
    await pending;
    assert.equal(f.auth.userId, 'new');
  });
}
