import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const first = '11111111-1111-4111-8111-111111111111';
const settings = (password = false, count = 1) => ({
  password_enabled: password,
  recovery_enabled: false,
  maximum: 10,
  passkeys: Array.from({ length: count }, (_, index) => ({
    id: index === 0 ? first : '22222222-2222-4222-8222-222222222222',
    created_at: '2026-09-23T12:00:00Z',
  })),
});
const challenge = (kind) => ({
  kind,
  ceremony_id: `owned-${kind}`,
  options: { publicKey: {}, mediation: 'required' },
});

async function fixture(t, initial = settings()) {
  const dom = await uiFixture();
  const requests = [],
    native = [],
    timers = new Map();
  let timerId = 0,
    now = 0,
    wall = 1000;
  const state = {
    current: true,
    removed: 0,
    changes: 0,
    response: challenge('authenticate'),
    native: async () => ({}),
    list: initial,
  };
  const request = async (method, token, data, signal) => {
    requests.push({ method, token, data, signal });
    return typeof state.response === 'function' ? state.response(method) : state.response;
  };
  const { AccountSecurityFlow } = await loadTypeScript('src/account-security.ts', {
    modules: {
      './auth': {
        deserializeRequestOptions: (options) => options,
        deserializeCreationOptions: (options) => options,
        serializeCredential: () => ({ id: 'owned-assertion' }),
      },
      './ui': {
        ...dom.ui,
        api: {
          passkeySettings: async () => state.list,
          passkeyAction: (...args) => request('start', ...args),
          passkeyAuthorize: (...args) => request('authorize', ...args),
          passkeyEnroll: (...args) => request('enroll', ...args),
        },
      },
    },
    globals: {
      performance: { now: () => now },
      Date: { now: () => wall },
      setTimeout: (callback, ms) => {
        timers.set(++timerId, { callback, at: now + ms });
        return timerId;
      },
      clearTimeout: (id) => timers.delete(id),
      navigator: {
        credentials: {
          get: (options) => {
            native.push({ kind: 'get', options });
            return state.native();
          },
          create: (options) => {
            native.push({ kind: 'create', options });
            return state.native();
          },
        },
      },
    },
  });
  const flow = new AccountSecurityFlow({
    token: () => 'owned-token',
    current: () => state.current,
    changed: () => state.changes++,
    removed: () => state.removed++,
  });
  t.after(() => flow.dispose());
  await flow.load();
  return {
    ...dom,
    flow,
    requests,
    native,
    timers,
    state,
    advance: async (ms, runTimers = true) => {
      now += ms;
      wall += ms;
      if (runTimers)
        for (const [id, timer] of [...timers]) {
          if (timer.at <= now) {
            timers.delete(id);
            timer.callback();
          }
        }
      await flush();
    },
    advanceWall: (ms) => {
      wall += ms;
    },
  };
}

test('adding a backup passkey needs separate explicit verification and creation gestures', async (t) => {
  const f = await fixture(t);
  await f.flow.start({ action: 'add' });
  assert.equal(f.native.length, 0);
  assert.equal(f.flow.phase, 'authenticate');
  f.state.response = challenge('register');
  const verified = f.flow.continueWithPasskey();
  assert.equal(f.native.length, 1, 'platform verification begins synchronously in the click turn');
  await verified;
  assert.equal(f.flow.phase, 'register');
  assert.equal(f.native.length, 1, 'a response cannot automatically open the next native prompt');
  f.state.response = { kind: 'added' };
  const registered = f.flow.continueWithPasskey();
  assert.equal(f.native[1].kind, 'create');
  await registered;
  await flush();
  assert.equal(f.flow.phase, 'idle');
  assert.deepEqual(
    f.requests.map((request) => request.method),
    ['start', 'authorize', 'enroll'],
  );
  assert.ok(f.requests.every((request) => request.token === 'owned-token'));
});

test('password proof supports first enrollment without an existing passkey or automatic native call', async (t) => {
  const f = await fixture(t, settings(true, 0));
  f.state.response = challenge('register');
  await f.flow.start({ action: 'add' }, 'owned-password');
  assert.deepEqual(f.requests[0].data, {
    operation: { action: 'add' },
    current_password: 'owned-password',
  });
  assert.equal(f.flow.phase, 'register');
  assert.equal(f.native.length, 0);
});

test('dismissal during a native ceremony aborts it, consumes late results and never authorizes', async (t) => {
  const f = await fixture(t);
  await f.flow.start({ action: 'recovery_key' });
  const pending = deferred();
  f.state.native = () => pending.promise;
  const ceremony = f.flow.continueWithPasskey();
  assert.equal(f.flow.canDismiss, true);
  f.flow.dispose();
  await ceremony;
  assert.equal(f.native[0].options.signal.aborted, true);
  pending.resolve({});
  await flush();
  assert.equal(f.requests.length, 1);
  assert.equal(f.timers.size, 0);
  assert.equal(f.flow.recoveryKey, '');
});

test('stalled mutation becomes terminal uncertain, blocks new actions and scrubs a late secret', async (t) => {
  const f = await fixture(t, settings(true));
  const pending = deferred();
  f.state.response = () => pending.promise;
  const work = f.flow.start({ action: 'recovery_key' }, 'owned-password');
  assert.equal(f.flow.canDismiss, false);
  await f.advance(20000);
  await work;
  assert.equal(f.flow.phase, 'uncertain');
  assert.equal(f.flow.canDismiss, false);
  assert.match(f.flow.message, /may have completed/);
  await f.flow.start({ action: 'add' });
  assert.equal(f.requests.length, 1, 'no retry after an unknown server result');
  const late = { kind: 'recovery_key', recovery_key: 'private-late-secret' };
  pending.resolve(late);
  await flush();
  assert.equal(late.recovery_key, '');
  assert.equal(f.flow.recoveryKey, '');
  assert.equal(f.flow.phase, 'uncertain');
});

for (const result of [{ kind: 'removed' }, { kind: 'recovery_key', recovery_key: 'private' }]) {
  test(`identity changes discard a late ${result.kind} mutation response`, async (t) => {
    const f = await fixture(t, settings(true));
    const pending = deferred();
    f.state.response = () => pending.promise;
    const work = f.flow.start(
      result.kind === 'removed' ? { action: 'remove', id: first } : { action: 'recovery_key' },
      'password',
    );
    f.state.current = false;
    pending.resolve({ ...result });
    await work;
    assert.equal(f.state.removed, 0);
    assert.equal(f.flow.recoveryKey, '');
  });
}

for (const status of [400, 401, 403, 408, 429, 500, 504]) {
  test(`HTTP ${status} preserves definite rejection versus uncertain mutation semantics`, async (t) => {
    const f = await fixture(t);
    f.state.response = () => {
      throw new f.ui.ApiError('Fixed rejection', status);
    };
    await f.flow.start({ action: 'recovery_key' });
    assert.equal(f.flow.phase, status >= 500 || status === 408 ? 'uncertain' : 'idle');
  });
}

test('wall-clock expiration cannot open a browser prompt after a suspended timer', async (t) => {
  const f = await fixture(t);
  await f.flow.start({ action: 'add' });
  f.advanceWall(60000);
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 0);
  assert.equal(f.flow.phase, 'idle');
  assert.equal(f.timers.size, 0);
});

test('late browser results cannot submit proof past a suspended wall deadline', async (t) => {
  const f = await fixture(t);
  await f.flow.start({ action: 'add' });
  const pending = deferred();
  f.state.native = () => pending.promise;
  const work = f.flow.continueWithPasskey();
  f.advanceWall(60000);
  pending.resolve({});
  await work;
  assert.equal(f.requests.length, 1);
  assert.equal(f.flow.phase, 'idle');
});

test('recovery-only protection cannot remove the last passkey; confirmed removal signs out once', async (t) => {
  const f = await fixture(t);
  f.flow.settings.recovery_enabled = true;
  await f.flow.start({ action: 'remove', id: first });
  assert.equal(f.requests.length, 0);
  f.flow.settings = settings(true);
  f.state.response = { kind: 'removed' };
  await f.flow.start({ action: 'remove', id: first }, 'owned-password');
  assert.equal(f.state.removed, 1);
  await f.flow.start({ action: 'remove', id: first }, 'owned-password');
  assert.equal(f.state.removed, 1);
});

test('a successful recovery key is removed from response and memory after acknowledgment', async (t) => {
  const f = await fixture(t, settings(true));
  const response = { kind: 'recovery_key', recovery_key: 'private-once' };
  f.state.response = response;
  await f.flow.start({ action: 'recovery_key' }, 'owned-password');
  assert.equal(response.recovery_key, '');
  assert.equal(f.flow.recoveryKey, 'private-once');
  f.flow.dismissRecoveryKey();
  assert.equal(f.flow.recoveryKey, '');
  assert.equal(f.flow.phase, 'idle');
});

test('mismatched successful action responses are uncertain rather than reported as completed', async (t) => {
  const f = await fixture(t);
  f.state.response = { kind: 'added' };
  await f.flow.start({ action: 'recovery_key' });
  assert.equal(f.flow.phase, 'uncertain');
  assert.equal(f.flow.recoveryKey, '');
});
