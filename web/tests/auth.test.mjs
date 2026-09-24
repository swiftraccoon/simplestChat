import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const REFRESH_MS = 12 * 60 * 1000;
const REPLAY_MS = 2250;
const RETRY_MS = 3000;
const RETRY_WINDOW_MS = 15000;
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

function session(name, expiresAt = 900) {
  const payload = Buffer.from(JSON.stringify({ sub: name, exp: expiresAt })).toString('base64url');
  return {
    token: `fixture.${payload}.${name}`,
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
  extensions = {};
  constructor(value = new AuthenticatorAssertionResponse()) {
    this.response = value;
  }
  getClientExtensionResults() {
    return this.extensions;
  }
}

function passkeyOptions(kind) {
  return {
    ceremony_id: `ceremony-${kind}`,
    ...(kind === 'login' ? { mediation: 'required' } : {}),
    publicKey:
      kind === 'register'
        ? {
            challenge: 'AQI',
            rp: { name: 'Test', id: 'localhost' },
            user: { id: 'AwQ', name: 'person@example.test', displayName: 'Person' },
            pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
            excludeCredentials: [{ type: 'public-key', id: 'BQY', transports: ['internal'] }],
            authenticatorSelection: {
              userVerification: 'required',
              residentKey: 'required',
              requireResidentKey: true,
            },
          }
        : {
            challenge: 'AQI',
            rpId: 'localhost',
            allowCredentials: [],
            userVerification: 'required',
            timeout: 60000,
          },
  };
}

async function fixture(t, { locks, random = 0 } = {}) {
  const requests = [];
  const queued = [];
  const timers = new Map();
  const changes = [];
  let nextTimer = 0;
  let now = 0;
  let wallNow = 0;
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
          return wallNow;
        }
      },
      performance: { now: () => now },
      Math: Object.assign(Object.create(Math), { random: () => random }),
      setTimeout: (callback, milliseconds) => {
        const id = nextTimer++;
        timers.set(id, { callback, milliseconds, deadline: now + milliseconds });
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
      wallNow += milliseconds;
    },
    moveWallClock: (milliseconds) => {
      wallNow += milliseconds;
    },
    session: (name, lifetimeSeconds = 900) =>
      session(name, Math.floor(wallNow / 1000) + lifetimeSeconds),
    enqueue: (value) => queued.push(value),
    async login(name = 'initial', lifetimeSeconds = 900) {
      queued.push(response(session(name, Math.floor(wallNow / 1000) + lifetimeSeconds)));
      await auth.login(`${name}@example.test`, 'password');
    },
    async fire(milliseconds) {
      const timer = [...timers.entries()].find(([, value]) => value.milliseconds === milliseconds);
      assert.ok(timer, `Missing ${milliseconds}ms timer`);
      timers.delete(timer[0]);
      const elapsed = Math.max(0, timer[1].deadline - now);
      now += elapsed;
      wallNow += elapsed;
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
  assert.equal(f.auth.jwt, session('rotated').token);
  assert.deepEqual(f.changes, [
    [true, false],
    [true, true],
  ]);
  assert.equal(f.timers.size, 1);
  const { signal, ...options } = f.requests[1].options;
  assert.ok(signal instanceof AbortSignal);
  assert.deepEqual(
    { url: f.requests[1].url, options },
    {
      url: '/api/auth/refresh',
      options: { method: 'POST', credentials: 'include' },
    },
  );
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
      request: async (name, options, request) => {
        assert.ok(options.signal instanceof AbortSignal);
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
      request: async (name, options, request) => {
        assert.ok(options.signal instanceof AbortSignal);
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

for (const failure of [500, 502, 503, 504, 'network']) {
  test(`scheduled refresh preserves identity through ${failure} and retries in place`, async (t) => {
    const f = await fixture(t);
    await f.login();
    const accepted = f.auth.jwt;
    const gate = deferred();
    f.enqueue(gate.promise);
    await f.fire(REFRESH_MS);
    if (failure === 'network') gate.reject(new TypeError('Failed to fetch'));
    else gate.resolve(response({ error: 'temporarily unavailable' }, failure));
    await flush();
    assert.equal(f.auth.jwt, accepted);
    assert.deepEqual(f.changes, [[true, false]]);
    assert.equal(f.requests.length, 2);
    assert.deepEqual(
      [...f.timers.values()].map((timer) => timer.milliseconds).sort((a, b) => a - b),
      [RETRY_MS, RETRY_WINDOW_MS],
    );

    const refreshed = f.session('refreshed');
    f.enqueue(response(refreshed));
    await f.fire(RETRY_MS);
    assert.equal(f.auth.jwt, refreshed.token);
    assert.deepEqual(f.changes, [
      [true, false],
      [true, true],
    ]);
    assert.equal(f.requests.length, 3);
    assert.equal(f.timers.size, 1);
  });
}

test('scheduled refresh jitter is bounded and never retries immediately', async (t) => {
  const f = await fixture(t, { random: 0.999 });
  await f.login();
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  assert.ok([...f.timers.values()].some((timer) => timer.milliseconds === RETRY_MS + 249));
  assert.equal(f.requests.length, 2);
});

test('scheduled refresh stops after three actual requests', async (t) => {
  const f = await fixture(t);
  await f.login();
  for (const delay of [REFRESH_MS, RETRY_MS, RETRY_MS]) {
    f.enqueue(response(null, 503));
    await f.fire(delay);
  }
  assert.equal(f.requests.filter((request) => request.url === '/api/auth/refresh').length, 3);
  assert.equal(f.auth.isLoggedIn, false);
  assert.deepEqual(f.changes, [
    [true, false],
    [false, false],
  ]);
  assert.equal(f.timers.size, 0);
});

for (const boundary of ['response', 'body']) {
  test(`scheduled refresh rejects a successful ${boundary} at the exact retention deadline`, async (t) => {
    const f = await fixture(t);
    await f.login();
    const gate = deferred();
    f.enqueue(boundary === 'body' ? { ...response(null), json: () => gate.promise } : gate.promise);
    await f.fire(REFRESH_MS);
    f.elapse(RETRY_WINDOW_MS);
    gate.resolve(boundary === 'body' ? f.session('late') : response(f.session('late')));
    await flush();
    assert.equal(f.auth.isLoggedIn, false);
    assert.deepEqual(f.changes, [
      [true, false],
      [false, false],
    ]);
    assert.equal(f.timers.size, 0);
    assert.equal(f.requests[1].options.signal.aborted, true);
  });
}

test('scheduled refresh watchdog aborts a request that never settles', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(new Promise(() => {}));
  await f.fire(REFRESH_MS);
  await f.fire(RETRY_WINDOW_MS);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.requests[1].options.signal.aborted, true);
  assert.equal(f.timers.size, 0);
});

test('accepted JWT expiry caps retention despite a backwards wall clock', async (t) => {
  const f = await fixture(t);
  await f.login('initial', (REFRESH_MS + 2000) / 1000);
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  f.moveWallClock(-60_000);
  await f.fire(2000);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

test('wall-clock retention deadline covers a suspended monotonic clock', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  f.moveWallClock(RETRY_WINDOW_MS);
  await f.fire(RETRY_MS);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.requests.length, 2, 'no retry after the fixed wall deadline');
  assert.equal(f.timers.size, 0);
});

test('accepted JWT wall-clock expiry also fences a pending successful refresh', async (t) => {
  const f = await fixture(t);
  await f.login('initial', (REFRESH_MS + 2000) / 1000);
  const gate = deferred();
  f.enqueue(gate.promise);
  await f.fire(REFRESH_MS);
  f.moveWallClock(2000);
  gate.resolve(response(f.session('late')));
  await flush();
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
});

test('unknown accepted-token expiry does not gain transient retry allowance', async (t) => {
  const f = await fixture(t);
  f.enqueue(response({ ...session('opaque'), token: 'opaque-fixture-token' }));
  await f.auth.login('opaque@example.test', 'password');
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

test('failed interactive logout preserves the original retry budget and cooldown', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  const expiryTimer = [...f.timers.values()].find(
    (timer) => timer.milliseconds === RETRY_WINDOW_MS,
  );
  f.elapse(1000);
  f.enqueue(response(null, 500));
  await assert.rejects(f.auth.logout(), /could not revoke/);
  assert.ok([...f.timers.values()].includes(expiryTimer), 'the original watchdog remains armed');
  assert.equal(f.requests[1].options.signal.aborted, true);
  for (const delay of [RETRY_MS - 1000, RETRY_MS]) {
    f.enqueue(response(null, 503));
    await f.fire(delay);
  }
  assert.equal(f.requests.filter((request) => request.url === '/api/auth/refresh').length, 3);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
});

test('retention expiry during an interactive login cannot resurrect the old session', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  const gate = deferred();
  f.enqueue(gate.promise);
  const login = assert.rejects(f.auth.login('new@example.test', 'password'), superseded);
  await f.fire(RETRY_WINDOW_MS);
  gate.resolve(response(f.session('late')));
  await login;
  assert.equal(f.auth.isLoggedIn, false);
  assert.deepEqual(f.changes, [
    [true, false],
    [false, false],
  ]);
  assert.equal(f.timers.size, 0);
});

test('a replaced session is immune to retired retry and expiry callbacks', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response(null, 503));
  await f.fire(REFRESH_MS);
  const retired = [...f.timers.values()].map((timer) => timer.callback);
  await f.login('new');
  for (const callback of retired) callback();
  await flush();
  assert.equal(f.auth.userId, 'new');
  assert.equal(f.requests.length, 3);
  assert.deepEqual(f.changes, [
    [true, false],
    [true, false],
  ]);
  assert.equal(f.timers.size, 1);
});

for (const status of [401, 403, 429]) {
  test(`scheduled refresh ${status} remains terminal without a transient retry`, async (t) => {
    const f = await fixture(t);
    await f.login();
    f.enqueue(response({ error: 'Access denied' }, status));
    await f.fire(REFRESH_MS);
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.requests.length, 2);
    assert.equal(f.timers.size, 0);
  });
}

test('scheduled refresh counts exact-401 replay confirmation within its three-request cap', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ error: 'Invalid token' }, 401));
  await f.fire(REFRESH_MS);
  f.enqueue(response(null, 503));
  await f.fire(REPLAY_MS);
  f.enqueue(response(null, 503));
  await f.fire(RETRY_MS);
  assert.equal(f.requests.filter((request) => request.url === '/api/auth/refresh').length, 3);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
});

test('scheduled refresh still accepts the exact-401 replay-confirmation successor', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ error: 'Invalid token' }, 401));
  await f.fire(REFRESH_MS);
  f.enqueue(response(f.session('successor')));
  await f.fire(REPLAY_MS);
  assert.equal(f.auth.userId, 'successor');
  assert.deepEqual(f.changes, [
    [true, false],
    [true, true],
  ]);
  assert.equal(f.timers.size, 1);
});

for (const boundary of ['response', 'body']) {
  test(`expired refresh ${boundary} cannot start a new exact-401 replay delay`, async (t) => {
    const f = await fixture(t);
    await f.login();
    const gate = deferred();
    f.enqueue(
      boundary === 'body'
        ? { ...response(null, 401), clone: () => ({ json: () => gate.promise }) }
        : gate.promise,
    );
    await f.fire(REFRESH_MS);
    f.moveWallClock(RETRY_WINDOW_MS);
    gate.resolve(
      boundary === 'body' ? { error: 'Invalid token' } : response({ error: 'Invalid token' }, 401),
    );
    await flush();
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.requests.length, 2, 'the first refresh is the only refresh request');
    assert.equal(f.timers.size, 0, 'no replay delay is installed after the wall deadline');
  });
}

for (const malformed of [{ token: 'incomplete' }, null]) {
  test(`scheduled refresh malformed successful body ${JSON.stringify(malformed)} is terminal`, async (t) => {
    const f = await fixture(t);
    await f.login();
    f.enqueue(response(malformed));
    await f.fire(REFRESH_MS);
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.requests.length, 2);
    assert.equal(f.timers.size, 0);
  });
}

test('initial restore does not gain scheduled-refresh retry behavior', async (t) => {
  const f = await fixture(t);
  f.enqueue(response(null, 503));
  assert.equal(await f.auth.tryRestore(), false);
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 0);
  assert.deepEqual(f.changes, []);
});

test('scheduled refresh owns one abortable Web Lock wait and never fetches after expiry', async (t) => {
  let lockRequests = 0;
  let waitingSignal;
  let release;
  const f = await fixture(t, {
    locks: {
      request: (name, options, request) => {
        assert.equal(name, 'simplestchat-refresh-v1');
        assert.ok(options.signal instanceof AbortSignal);
        lockRequests += 1;
        waitingSignal = options.signal;
        return new Promise((resolve, reject) => {
          const abort = () => reject(new Error('Lock request aborted'));
          options.signal.addEventListener('abort', abort, { once: true });
          release = async () => {
            options.signal.removeEventListener('abort', abort);
            try {
              resolve(await request());
            } catch (error) {
              reject(error);
            }
          };
        });
      },
    },
  });
  await f.login();
  await f.fire(REFRESH_MS);
  assert.equal(lockRequests, 1);
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 1);
  await f.fire(RETRY_WINDOW_MS);
  assert.equal(waitingSignal.aborted, true);
  await release();
  await flush();
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(lockRequests, 1);
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 0);
});

test('malformed successful authentication clears local identity and requires reload before another attempt', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ token: 'new', user: { id: 'new' } }));
  await assert.rejects(f.auth.login('new@example.test', 'password'), {
    name: 'SessionOutcomeUnknownError',
  });
  assert.equal(f.auth.userId, null);
  assert.equal(f.auth.jwt, null);
  assert.equal(f.timers.size, 0);
  await assert.rejects(f.auth.login('again@example.test', 'password'), {
    name: 'SessionOutcomeUnknownError',
  });
  assert.equal(
    f.requests.length,
    2,
    'an uncertain cookie cannot be overwritten by another local intent',
  );
});

test('conclusive HTTP rejection remains retryable even when the error body is malformed', async (t) => {
  const f = await fixture(t);
  await f.login();
  f.enqueue(response({ error: { unexpected: true } }, 401));
  await assert.rejects(f.auth.login('new@example.test', 'password'), /^Error: Login failed$/);
  assert.equal(f.auth.userId, 'initial');
  assert.equal(f.timers.size, 1);
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
        : await f.auth.passkeyLoginStart();
    assert.deepEqual([...new Uint8Array(decoded.publicKey.challenge)], [1, 2]);
    assert.deepEqual(options, original, 'server JSON is not mutated');
    if (kind === 'login') {
      assert.deepEqual(JSON.parse(f.requests[0].options.body), {});
      assert.equal(f.requests[0].url, '/api/auth/passkey/login/start');
      assert.equal(decoded.mediation, 'required');
      assert.deepEqual(decoded.publicKey.allowCredentials, []);
    } else {
      assert.equal(decoded.publicKey.authenticatorSelection.residentKey, 'required');
      assert.equal(decoded.publicKey.authenticatorSelection.requireResidentKey, true);
      assert.equal(decoded.publicKey.authenticatorSelection.authenticatorAttachment, undefined);
    }
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
        : f.auth.passkeyLoginStart(),
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
    else await f.auth.passkeyLoginStart();
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

test('passkey options reject invalid mediation without retaining a ceremony', async (t) => {
  const f = await fixture(t);
  f.enqueue(response({ ...passkeyOptions('login'), mediation: { unexpected: true } }));
  await assert.rejects(f.auth.passkeyLoginStart(), /Unsupported passkey mediation/);
  await assert.rejects(f.auth.passkeyLoginFinish(new PublicKeyCredential()), /was not started/);
  assert.equal(f.requests.length, 1);
});

for (const extensions of [
  {},
  { credProps: { rk: true }, appid: false, unrelated: 'must-not-send' },
  { credProps: { rk: false, unexpected: 'must-not-send' } },
  { credProps: { rk: 'true' }, appid: 'false' },
]) {
  test(`passkey extension serialization permits only typed known properties: ${JSON.stringify(extensions)}`, async (t) => {
    const f = await fixture(t);
    f.enqueue(response(passkeyOptions('register')));
    await f.auth.passkeyRegisterStart('person@example.test', 'Person');
    const credential = new PublicKeyCredential(new AuthenticatorAttestationResponse());
    credential.extensions = extensions;
    f.enqueue(response(session('passkey')));
    await f.auth.passkeyRegisterFinish(credential);
    const sent = JSON.parse(f.requests[1].options.body).credential.clientExtensionResults;
    const expected = {};
    if (typeof extensions.credProps?.rk === 'boolean')
      expected.credProps = { rk: extensions.credProps.rk };
    if (typeof extensions.appid === 'boolean') expected.appid = extensions.appid;
    assert.deepEqual(sent, Object.keys(expected).length ? expected : undefined);
  });
}

for (const stalledStage of ['fetch', 'body', 'rejection-body', 'lock']) {
  test(`initial restore deadline covers ${stalledStage}, returns guest and refuses late adoption`, async (t) => {
    const pending = deferred();
    let lockCallback;
    let lockSignal;
    const f = await fixture(
      t,
      stalledStage === 'lock'
        ? {
            locks: {
              request: (_name, options, callback) => {
                lockCallback = callback;
                lockSignal = options.signal;
                return pending.promise;
              },
            },
          }
        : {},
    );
    const events = [];
    f.auth.setTelemetryHandler((event) => events.push(event));
    if (stalledStage === 'fetch') f.enqueue(pending.promise);
    else if (stalledStage === 'body') f.enqueue({ ...response(null), json: () => pending.promise });
    else if (stalledStage === 'rejection-body')
      f.enqueue({ ...response(null, 401), clone: () => ({ json: () => pending.promise }) });
    const restore = f.auth.tryRestore();
    await flush();
    await f.fire(10000);
    assert.equal(await restore, false);
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(events.at(-1).outcome, 'timeout');
    assert.equal(events.at(-1).durationMs, 10000);
    if (stalledStage === 'lock') {
      assert.equal(lockSignal.aborted, true);
      assert.throws(lockCallback);
      pending.resolve(response(session('late')));
      assert.equal(f.requests.length, 0);
    } else {
      assert.equal(f.requests[0].options.signal.aborted, true);
      pending.resolve(stalledStage === 'fetch' ? response(session('late')) : session('late'));
    }
    await flush();
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.requests.length, stalledStage === 'lock' ? 0 : 1);
    assert.equal(f.timers.size, 0);
  });
}

test('restore checks wall deadline before accepting a late body even if watchdog has not run', async (t) => {
  const pending = deferred();
  const f = await fixture(t);
  f.enqueue({ ...response(null), json: () => pending.promise });
  const restore = f.auth.tryRestore();
  await flush();
  f.moveWallClock(10000);
  pending.resolve(session('late'));
  assert.equal(await restore, false);
  assert.equal(f.auth.isLoggedIn, false);
  assert.equal(f.timers.size, 0);
});

test('telemetry callback failures cannot prevent restoring a valid session', async (t) => {
  const f = await fixture(t);
  f.auth.setTelemetryHandler(() => {
    throw new Error('collector failed');
  });
  f.enqueue(response(session('restored')));
  assert.equal(await f.auth.tryRestore(), true);
  assert.equal(f.auth.userId, 'restored');
});

test('dismissed passkey challenge fetch is abortable and its late result cannot retain a ceremony', async (t) => {
  const pending = deferred();
  const f = await fixture(t);
  const controller = new AbortController();
  f.enqueue(pending.promise);
  const start = f.auth.passkeyLoginStart(controller.signal);
  const rejected = assert.rejects(start, superseded);
  assert.equal(f.requests[0].options.signal, controller.signal);
  controller.abort();
  f.auth.cancelPasskeyAttempt();
  pending.resolve(response(passkeyOptions('login')));
  await rejected;
  await assert.rejects(f.auth.passkeyLoginFinish(new PublicKeyCredential()), /was not started/);
  assert.equal(f.requests.length, 1);
});

for (const stage of ['fetch', 'body']) {
  test(`interactive authentication bounds stalled ${stage}, fences late adoption and refuses further authentication until reload`, async (t) => {
    const pending = deferred();
    const f = await fixture(t);
    f.enqueue(
      stage === 'fetch' ? pending.promise : { ...response(null), json: () => pending.promise },
    );
    const login = f.auth.login('fixture@example.test', 'password');
    const rejected = assert.rejects(login, { name: 'SessionOutcomeUnknownError' });
    await flush();
    await f.fire(20000);
    await rejected;
    assert.equal(f.requests[0].options.signal.aborted, true);
    assert.equal(f.auth.isLoggedIn, false);
    assert.equal(f.timers.size, 0);
    pending.resolve(stage === 'fetch' ? response(session('late')) : session('late'));
    await flush();
    assert.equal(f.auth.isLoggedIn, false);
    await assert.rejects(f.auth.register('new@example.test', 'New', 'password'), {
      name: 'SessionOutcomeUnknownError',
    });
    await assert.rejects(f.auth.passkeyLoginStart(), {
      name: 'SessionOutcomeUnknownError',
    });
    assert.equal(f.requests.length, 1);
  });
}

test('initial restore cannot fetch after a paused Web Lock resumes beyond the wall deadline', async (t) => {
  const waiting = deferred();
  const f = await fixture(t, {
    locks: { request: (_name, _options, callback) => waiting.promise.then(callback) },
  });
  const restore = f.auth.tryRestore();
  f.moveWallClock(10000);
  waiting.resolve();
  assert.equal(await restore, false);
  assert.equal(f.requests.length, 0);
  assert.equal(f.timers.size, 0);
});

test('an initial rejected-refresh body completing after its wall deadline cannot start replay', async (t) => {
  const pending = deferred();
  const f = await fixture(t);
  f.enqueue({ ...response(null, 401), clone: () => ({ json: () => pending.promise }) });
  const restore = f.auth.tryRestore();
  await flush();
  f.moveWallClock(10000);
  pending.resolve({ error: 'Invalid token' });
  assert.equal(await restore, false);
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 0);
});

test('dismissing an idle authentication dialog does not cancel a valid startup restore', async (t) => {
  const pending = deferred();
  const f = await fixture(t);
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  const flow = new AuthDialogFlow(() => f.auth.cancelPasskeyAttempt());
  f.enqueue(pending.promise);
  const restore = f.auth.tryRestore();
  assert.equal(flow.dismiss(), true);
  pending.resolve(response(session('restored')));
  assert.equal(await restore, true);
  assert.equal(f.auth.userId, 'restored');
});
