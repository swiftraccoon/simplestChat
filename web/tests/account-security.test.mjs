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
const replacement = () => ({
  ...challenge('replace_registration'),
  recovery_key: 'private-replacement-backup',
});

async function fixture(t, initial = settings(), mounted = false) {
  const dom = await uiFixture();
  dom.Node.prototype.focus = function () {
    dom.document.activeElement = this;
  };
  const requests = [],
    native = [],
    timers = new Map();
  let timerId = 0,
    now = 0,
    wall = 1000;
  const state = {
    current: true,
    removed: 0,
    completed: [],
    copied: [],
    copy: async () => {},
    decodeCreation: (options) => options,
    changes: 0,
    response: challenge('authenticate'),
    native: async () => ({}),
    list: initial,
  };
  const request = async (method, token, data, signal) => {
    requests.push({ method, token, data, signal });
    return typeof state.response === 'function' ? state.response(method) : state.response;
  };
  const { AccountSecurityFlow, mountAccountSecurity } = await loadTypeScript(
    'src/account-security.ts',
    {
      modules: {
        './auth': {
          deserializeRequestOptions: (options) => options,
          deserializeCreationOptions: (options) => state.decodeCreation(options),
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
        Date: class extends Date {
          static now() {
            return wall;
          }
        },
        setTimeout: (callback, ms) => {
          timers.set(++timerId, { callback, at: now + ms });
          return timerId;
        },
        clearTimeout: (id) => timers.delete(id),
        navigator: {
          clipboard: {
            writeText: async (value) => {
              state.copied.push(value);
              await state.copy();
            },
          },
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
    },
  );
  const options = {
    token: () => 'owned-token',
    current: () => state.current,
    changed: () => state.changes++,
    completed: (kind) => {
      state.completed.push(kind);
      if (kind === 'removed') state.removed++;
    },
  };
  const dialog = dom.ui.el('dialog');
  const container = dom.ui.el('div');
  dialog.append(container);
  dom.document.body.append(dialog);
  dialog.showModal();
  const flow = mounted
    ? mountAccountSecurity({ ...options, container, dialog })
    : new AccountSecurityFlow(options);
  t.after(() => flow.dispose());
  await flow.load();
  return {
    ...dom,
    flow,
    container,
    dialog,
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

function clickButton(f, label) {
  const node = f.container
    .querySelectorAll('button')
    .find((button) => button.textContent === label);
  assert.ok(node, `Expected visible button: ${label}`);
  node.click();
}

function proofControls(f) {
  const proof = f.container.querySelector('select');
  const password = f.container.querySelector('input');
  assert.ok(proof?.isConnected, 'verification selector is in the current screen');
  assert.ok(password?.isConnected, 'verification password is in the current screen');
  return { proof, password };
}

test('rendered removal uses visible password proof and reports removal only after confirmation', async (t) => {
  const f = await fixture(t, settings(true), true);
  assert.equal(proofControls(f).proof.value, 'passkey', 'other account actions keep their default');
  clickButton(f, 'Remove passkey 11111111');
  assert.equal(f.container.querySelector('h3').textContent, 'Remove passkey');
  const { proof, password } = proofControls(f);
  assert.equal(proof.value, 'password');
  assert.equal(password.parentNode.hidden, false);
  assert.equal(password.autocomplete, 'current-password');
  assert.equal(f.document.activeElement, proof);
  assert.match(f.container.textContent, /Verify your identity before removal/);
  assert.equal(f.requests.length, 0);

  const pending = deferred();
  f.state.response = () => pending.promise;
  password.value = 'owned-password';
  clickButton(f, 'Verify and remove passkey');
  assert.deepEqual(f.requests[0].data, {
    operation: { action: 'remove', id: first },
    current_password: 'owned-password',
  });
  assert.equal(password.value, '', 'detached password input is cleared');
  assert.equal(f.native.length, 0);
  assert.equal(f.state.removed, 0);
  assert.equal(f.flow.phase, 'requesting');
  assert.doesNotMatch(f.container.textContent, /Passkey removed/);
  pending.resolve({ kind: 'removed' });
  await flush();
  assert.equal(f.state.removed, 1);
  assert.match(f.container.textContent, /Passkey removed\. Sign in again/);
  assert.equal(f.native.length, 0, 'password proof never invokes the browser authenticator');
});

test('missing password keeps removal controls visible and cancelling sends no request', async (t) => {
  const f = await fixture(t, settings(true), true);
  clickButton(f, 'Remove passkey 11111111');
  clickButton(f, 'Verify and remove passkey');
  assert.equal(f.requests.length, 0);
  assert.equal(f.native.length, 0);
  assert.equal(f.container.querySelector('h3').textContent, 'Remove passkey');
  const { proof, password } = proofControls(f);
  assert.equal(proof.value, 'password');
  assert.equal(password.parentNode.hidden, false);
  assert.equal(f.document.activeElement, password);
  assert.match(f.container.textContent, /Enter your current password/);
  password.value = 'unsent-password';
  clickButton(f, 'Keep passkey');
  assert.equal(password.value, '');
  assert.equal(f.container.querySelector('h3').textContent, 'Sign-in and recovery');
  assert.equal(proofControls(f).proof.value, 'passkey');
  assert.equal(f.requests.length, 0);
  assert.equal(f.state.removed, 0);
});

test('removal allows choosing passkey proof and returning to password without a native prompt', async (t) => {
  const f = await fixture(t, settings(true), true);
  clickButton(f, 'Remove passkey 11111111');
  const { proof, password } = proofControls(f);
  password.value = 'unused-password';
  proof.value = 'passkey';
  proof.emit('change');
  assert.equal(password.value, '');
  assert.equal(password.parentNode.hidden, true);
  clickButton(f, 'Verify and remove passkey');
  await flush();
  assert.deepEqual(f.requests[0].data, { operation: { action: 'remove', id: first } });
  assert.equal(f.flow.phase, 'authenticate');
  assert.match(f.container.textContent, /The passkey has not been removed/);
  assert.equal(f.native.length, 0, 'fetching proof options is not a user gesture to authenticate');
  clickButton(f, 'Back to removal options');
  assert.equal(f.flow.phase, 'idle');
  assert.equal(proofControls(f).proof.value, 'password');
  assert.equal(proofControls(f).password.parentNode.hidden, false);
  assert.equal(f.timers.size, 0);
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 0, 'retired proof cannot be submitted');
  assert.equal(f.requests.length, 1);
  assert.equal(f.state.removed, 0);
});

test('passkey-only removal still verifies on a separate gesture before deletion', async (t) => {
  const f = await fixture(t, settings(false, 2), true);
  clickButton(f, 'Remove passkey 11111111');
  const { proof, password } = proofControls(f);
  assert.equal(proof.value, 'passkey');
  assert.equal(proof.children.length, 1);
  assert.equal(password.parentNode.hidden, true);
  clickButton(f, 'Verify and remove passkey');
  await flush();
  assert.equal(f.native.length, 0);
  assert.equal(f.state.removed, 0);
  assert.match(f.container.textContent, /The passkey has not been removed/);
  f.state.response = { kind: 'removed' };
  clickButton(f, 'Verify and remove passkey');
  assert.equal(f.native.length, 1, 'explicit verification invokes native get in the click turn');
  await flush();
  assert.equal(f.requests[1].method, 'authorize');
  assert.equal(f.state.removed, 1);
});

test('rendered last-passkey lockout cannot be bypassed by a recovery key', async (t) => {
  const f = await fixture(t, { ...settings(), recovery_enabled: true }, true);
  const remove = f.container
    .querySelectorAll('button')
    .find((button) => button.textContent === 'Remove passkey 11111111');
  assert.equal(remove.disabled, true);
  remove.click();
  assert.equal(f.container.querySelector('h3').textContent, 'Sign-in and recovery');
  assert.equal(f.requests.length, 0);
  assert.equal(f.native.length, 0);
});

test('back from verification cannot cancel an in-flight mutation or uncertain result', async (t) => {
  const f = await fixture(t, settings(true));
  const pending = deferred();
  f.state.response = () => pending.promise;
  const work = f.flow.start({ action: 'remove', id: first }, 'owned-password');
  f.flow.cancelVerification();
  assert.equal(f.flow.phase, 'requesting');
  await f.advance(20000);
  await work;
  f.flow.cancelVerification();
  assert.equal(f.flow.phase, 'uncertain');
  assert.equal(f.flow.canStart, false);
});

test('passkey-only replacement at the key limit requires proof, saved backup and a separate creation gesture', async (t) => {
  const f = await fixture(t, { ...settings(), maximum: 1 }, true);
  clickButton(f, 'Replace passkey 11111111');
  assert.equal(proofControls(f).proof.value, 'passkey');
  assert.equal(proofControls(f).password.parentNode.hidden, true);
  assert.match(f.container.textContent, /Verify your identity before replacement/);
  assert.match(f.container.textContent, /replace any previous recovery key/);
  clickButton(f, 'Verify before replacement');
  await flush();
  assert.deepEqual(f.requests[0].data, { operation: { action: 'replace', id: first } });
  assert.equal(f.native.length, 0);

  const response = replacement();
  f.state.response = response;
  clickButton(f, 'Verify replacement with passkey');
  assert.equal(f.native[0].kind, 'get');
  await flush();
  assert.equal(f.flow.phase, 'replacement_recovery');
  assert.equal(response.recovery_key, '');
  assert.equal(f.flow.settings.recovery_enabled, true);
  assert.match(f.container.textContent, /password manager may overwrite/);
  assert.equal(
    f.container
      .querySelectorAll('button')
      .some((node) => node.textContent === 'Create replacement passkey'),
    false,
  );
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 1, 'even direct continuation cannot skip saving the backup');
  assert.deepEqual(f.state.completed, []);
  const key = f.container.querySelector('textarea');
  assert.equal(key.value, 'private-replacement-backup');
  clickButton(f, 'Copy recovery key');
  await flush();
  assert.deepEqual(f.state.copied, ['private-replacement-backup']);
  assert.match(f.container.textContent, /Copied/);
  assert.equal(f.native.length, 1);

  clickButton(f, 'I saved my recovery key');
  assert.equal(key.value, '');
  assert.equal(f.flow.recoveryKey, '');
  assert.equal(f.flow.phase, 'register');
  assert.equal(f.native.length, 1, 'acknowledgment is not a browser creation gesture');
  const pending = deferred();
  f.state.response = () => pending.promise;
  clickButton(f, 'Create replacement passkey');
  assert.equal(f.native[1].kind, 'create');
  await flush();
  assert.deepEqual(
    f.requests.map((entry) => entry.method),
    ['start', 'authorize', 'enroll'],
  );
  assert.deepEqual(f.requests[2].data, {
    ceremony_id: 'owned-replace_registration',
    credential: { id: 'owned-assertion' },
  });
  assert.deepEqual(f.state.completed, [], 'no success before the verified server swap');
  pending.resolve({ kind: 'replaced' });
  await flush();
  assert.equal(f.flow.phase, 'replaced');
  assert.deepEqual(f.state.completed, ['replaced']);
  assert.match(f.container.textContent, /Passkey replaced\. Sign in again with your new passkey/);
});

test('replacement password proof stays visible, requires a value and can be cancelled without native calls', async (t) => {
  const f = await fixture(t, settings(true), true);
  clickButton(f, 'Replace passkey 11111111');
  assert.equal(proofControls(f).proof.value, 'password');
  assert.equal(proofControls(f).password.parentNode.hidden, false);
  clickButton(f, 'Verify before replacement');
  assert.equal(f.requests.length, 0);
  assert.equal(f.document.activeElement, proofControls(f).password);
  clickButton(f, 'Keep passkey');
  assert.equal(f.requests.length, 0);
  clickButton(f, 'Replace passkey 11111111');
  const password = proofControls(f).password;
  password.value = 'owned-password';
  f.state.response = replacement();
  clickButton(f, 'Verify before replacement');
  await flush();
  assert.deepEqual(f.requests[0].data, {
    operation: { action: 'replace', id: first },
    current_password: 'owned-password',
  });
  assert.equal(password.value, '');
  assert.equal(f.native.length, 0);
  assert.equal(f.flow.phase, 'replacement_recovery');
});

test('replacement proof can return to its visible options without invoking native verification', async (t) => {
  const f = await fixture(t, settings(), true);
  clickButton(f, 'Replace passkey 11111111');
  clickButton(f, 'Verify before replacement');
  await flush();
  clickButton(f, 'Back to replacement options');
  assert.equal(f.flow.phase, 'idle');
  assert.equal(proofControls(f).proof.value, 'passkey');
  assert.equal(f.timers.size, 0);
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 0);
  assert.equal(f.requests.length, 1);
});

test('copy failure and five-minute expiry retain the replacement backup until acknowledgment', async (t) => {
  const f = await fixture(t, settings(true), true);
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  f.state.copy = async () => {
    throw new Error('Private clipboard failure');
  };
  clickButton(f, 'Copy recovery key');
  await flush();
  assert.match(f.container.textContent, /Copy failed\. Select and save the key manually/);
  assert.doesNotMatch(f.container.textContent, /Private clipboard failure/);
  await f.advance(60000);
  assert.equal(
    f.flow.phase,
    'replacement_recovery',
    'ordinary 60-second deadline does not retire replacement preparation',
  );
  await f.advance(240000);
  assert.equal(f.flow.phase, 'replacement_recovery');
  assert.match(f.container.textContent, /replacement request expired/);
  assert.equal(f.container.querySelector('textarea').value, 'private-replacement-backup');
  clickButton(f, 'I saved my recovery key');
  assert.equal(f.flow.phase, 'idle');
  assert.equal(f.flow.recoveryKey, '');
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 0);
  assert.equal(f.requests.length, 1);
});

test('wall-clock expiry before saved acknowledgment cannot create a replacement', async (t) => {
  const f = await fixture(t, settings(true));
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  f.advanceWall(300000);
  f.flow.acknowledgeReplacementRecovery();
  await f.flow.continueWithPasskey();
  assert.equal(f.flow.phase, 'idle');
  assert.equal(f.native.length, 0);
  assert.equal(f.timers.size, 0);
});

test('unusable replacement options still expose the issued backup without permitting creation', async (t) => {
  const f = await fixture(t, settings(true), true);
  f.state.decodeCreation = () => {
    throw new Error('Unusable options');
  };
  const response = replacement();
  f.state.response = response;
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  assert.equal(response.recovery_key, '');
  assert.equal(f.flow.phase, 'replacement_recovery');
  assert.match(f.container.textContent, /Replacement could not be prepared/);
  assert.equal(f.container.querySelector('textarea').value, 'private-replacement-backup');
  clickButton(f, 'I saved my recovery key');
  await f.flow.continueWithPasskey();
  assert.equal(f.flow.phase, 'idle');
  assert.equal(f.native.length, 0);
});

test('intentional dialog close scrubs a displayed replacement backup and retires its challenge', async (t) => {
  const f = await fixture(t, settings(true), true);
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  const key = f.container.querySelector('textarea');
  f.dialog.close();
  assert.equal(key.value, '');
  assert.equal(f.flow.recoveryKey, '');
  assert.equal(f.timers.size, 0);
  f.flow.acknowledgeReplacementRecovery();
  await f.flow.continueWithPasskey();
  assert.equal(f.native.length, 0);
});

test('cancelled native replacement retains truthful recovery guidance without claiming a swap', async (t) => {
  const f = await fixture(t, settings(true));
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  f.flow.acknowledgeReplacementRecovery();
  f.state.native = async () => {
    throw new DOMException('Cancelled', 'NotAllowedError');
  };
  await f.flow.continueWithPasskey();
  assert.equal(f.flow.phase, 'idle');
  assert.match(f.flow.message, /saved recovery key can reset your password/);
  assert.equal(f.requests.length, 1);
  assert.deepEqual(f.state.completed, []);
});

test('a stalled replacement enrollment stays uncertain and never reports replacement', async (t) => {
  const f = await fixture(t, settings(true));
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  f.flow.acknowledgeReplacementRecovery();
  f.state.response = () => new Promise(() => {});
  const work = f.flow.continueWithPasskey();
  await flush();
  await f.advance(20000);
  await work;
  assert.equal(f.flow.phase, 'uncertain');
  assert.deepEqual(f.state.completed, []);
  assert.match(f.flow.message, /Keep your saved recovery key/);
  assert.match(f.flow.message, /try the new passkey/);
  assert.doesNotMatch(f.flow.message, /generate a new one/);
  await f.flow.start({ action: 'replace', id: first });
  assert.equal(f.requests.length, 2);
});

test('definite replacement enrollment rejection retains recovery guidance after provider creation', async (t) => {
  const f = await fixture(t, settings(true));
  f.state.response = replacement();
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  await f.advance(90000);
  f.flow.acknowledgeReplacementRecovery();
  assert.equal(f.flow.phase, 'register', 'replacement has a five-minute preparation deadline');
  f.state.response = () => {
    throw new f.ui.ApiError('Replacement rejected', 400);
  };
  await f.flow.continueWithPasskey();
  assert.equal(f.native[0].kind, 'create');
  assert.equal(f.flow.phase, 'idle');
  assert.match(f.flow.message, /Replacement rejected/);
  assert.match(f.flow.message, /saved recovery key to reset your password/);
  assert.deepEqual(f.state.completed, []);
});

test('late replacement preparation after timeout cannot expose its rotated backup', async (t) => {
  const f = await fixture(t, settings(true));
  const pending = deferred();
  f.state.response = () => pending.promise;
  const work = f.flow.start({ action: 'replace', id: first }, 'owned-password');
  await f.advance(20000);
  await work;
  assert.equal(f.flow.phase, 'uncertain');
  const late = replacement();
  pending.resolve(late);
  await flush();
  assert.equal(late.recovery_key, '');
  assert.equal(f.flow.recoveryKey, '');
  assert.equal(f.native.length, 0);
  assert.equal(f.timers.size, 0);
});

test('replacement responses are bound to operation, step and current identity', async (t) => {
  const f = await fixture(t, settings(true));
  await f.flow.start({ action: 'replace', id: 'missing-key' }, 'owned-password');
  assert.equal(f.requests.length, 0);
  f.state.response = { kind: 'replaced' };
  await f.flow.start({ action: 'replace', id: first }, 'owned-password');
  assert.equal(f.flow.phase, 'uncertain', 'start cannot claim an enrollment completed');
  assert.deepEqual(f.state.completed, []);
  const mismatch = await fixture(t, settings(true));
  const wrong = replacement();
  mismatch.state.response = wrong;
  await mismatch.flow.start({ action: 'add' }, 'owned-password');
  assert.equal(wrong.recovery_key, '');
  assert.equal(mismatch.flow.recoveryKey, '');
  assert.equal(mismatch.flow.phase, 'uncertain');
  const stale = await fixture(t, settings(true));
  const pending = deferred();
  stale.state.response = () => pending.promise;
  const work = stale.flow.start({ action: 'replace', id: first }, 'owned-password');
  stale.state.current = false;
  const late = replacement();
  pending.resolve(late);
  await work;
  assert.equal(late.recovery_key, '');
  assert.equal(stale.flow.recoveryKey, '');
  assert.deepEqual(stale.state.completed, []);
});
