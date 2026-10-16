import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const source = await readFile(new URL('../../web/e2e/webkit-test-network.cjs', import.meta.url), 'utf8');
const plain = value => JSON.parse(JSON.stringify(value));
const failed = error => error.message === 'Owned WebKit test network configuration failed.';
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function fixture({ platform = 'darwin', env = { WEBKIT_TEST_NETWORK: '1' }, version = '1.64.0', immediateDeadline = false } = {}) {
  const calls = [], timers = [], cleared = [];
  let nextSend, nextInitialize, disposed = false;
  const connection = { toImpl: () => implementation };
  const playwright = { _connection: connection };
  const context = { close: async options => { calls.push(['close', options]); disposed = true; } };
  const page = { _connection: connection, context: () => context };
  function session(label = 'initial') {
    let sessionDisposed = false;
    return {
      label,
      isDisposed: () => disposed || sessionDisposed,
      dispose: () => { sessionDisposed = true; },
      async send(...args) { calls.push(['send', label, ...args]); return nextSend?.(...args); },
    };
  }
  class _WKPage {
    async _initializeSessionMayThrow(...args) { calls.push(['initialize', this === delegate, args[0].label, ...args.slice(1)]); return nextInitialize?.(...args); }
  }
  const delegate = new _WKPage();
  const original = delegate._initializeSessionMayThrow;
  const implementation = { delegate };
  Object.assign(delegate, { _page: implementation, _session: session() });
  const module = { exports: {} };
  vm.runInNewContext(source, {
    module, process: { platform, env },
    require: name => {
      calls.push(['require', name]);
      if (name === 'playwright/package.json') return { version };
      if (name === 'playwright') return playwright;
      throw new Error('Unexpected dependency');
    },
    setTimeout: (callback, delay) => { timers.push(delay); if (immediateDeadline) queueMicrotask(callback); return timers.length; },
    clearTimeout: timer => cleared.push(timer),
  }, { timeout: 1000 });
  return { install: module.exports.installWebKitTestNetwork, calls, timers, cleared, playwright, page, context, delegate, original, session,
    send: action => { nextSend = action; }, initialize: action => { nextInitialize = action; }, };
}
const install = f => f.install(f.playwright, f.page, { browserName: 'webkit' });

test('owned WebKit configuration requires exact opt-in and platform before reading dependencies', async () => {
  for (const config of [{ env: {} }, { env: { WEBKIT_TEST_NETWORK: 'true' } }, { platform: 'linux' }]) {
    const f = fixture(config); await assert.rejects(install(f), /requires/); assert.deepEqual(f.calls, []);
  }
  const f = fixture();
  await assert.rejects(f.install(f.playwright, f.page, { browserName: 'chromium' }), /requires macOS WebKit/);
  assert.deepEqual(f.calls, []);
});

test('initial configuration is native, exact, deduplicated and restores only its own instance hook', async () => {
  const f = fixture(), other = fixture();
  const controller = await install(f);
  assert.deepEqual(plain({ setting: controller.setting, value: controller.value, playwrightVersion: controller.playwrightVersion }), {
    setting: 'ICECandidateFilteringEnabled', value: false, playwrightVersion: '1.64.0',
  });
  assert.equal(controller.appliedCount(), 1);
  await controller.ready(); await controller.ready();
  assert.deepEqual(plain(f.calls.filter(c => c[0] === 'send')), [['send', 'initial', 'Page.overrideSetting', { setting: 'ICECandidateFilteringEnabled', value: false }]]);
  assert.notEqual(f.delegate._initializeSessionMayThrow, f.original);
  assert.equal(other.delegate._initializeSessionMayThrow, other.original);
  assert.equal(Object.hasOwn(Object.getPrototypeOf(f.delegate), '_initializeSessionMayThrow'), true);
  await controller.dispose(); await controller.dispose();
  assert.equal(f.delegate._initializeSessionMayThrow, f.original);
  assert.equal(Object.hasOwn(f.delegate, '_initializeSessionMayThrow'), false);
  await assert.rejects(controller.ready(), failed);
});

test('new process cannot resume until original initialization and native configuration both finish', async () => {
  const f = fixture(), controller = await install(f);
  const initialized = deferred(), configured = deferred(), next = f.session('replacement');
  f.initialize(() => initialized.promise); f.send(() => configured.promise);
  let resumed = false, ready = false;
  const initialization = f.delegate._initializeSessionMayThrow(next, 'owned-handler').then(() => { f.delegate._session = next; resumed = true; });
  const readiness = controller.ready().then(() => { ready = true; });
  await Promise.resolve(); assert.equal(resumed, false); assert.equal(ready, false);
  assert.equal(f.calls.filter(c => c[0] === 'send').length, 1);
  initialized.resolve(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(resumed, false); assert.equal(ready, false);
  assert.deepEqual(plain(f.calls.find(c => c[0] === 'initialize')), ['initialize', true, 'replacement', 'owned-handler']);
  configured.resolve(); await initialization; await readiness;
  assert.equal(resumed, true); assert.equal(ready, true); assert.equal(controller.appliedCount(), 2);
  await controller.dispose();
});

test('a failed replacement command closes its owned context and cannot report readiness or resume', async () => {
  const f = fixture(), controller = await install(f);
  f.send(() => { throw new Error('PRIVATE_PROTOCOL_AND_ADDRESS'); });
  let resumed = false;
  await assert.rejects(f.delegate._initializeSessionMayThrow(f.session('replacement')).then(() => { resumed = true; }), failed);
  assert.equal(resumed, false);
  assert.deepEqual(plain(f.calls.filter(c => c[0] === 'close')), [['close', { reason: 'Owned WebKit test network configuration failed.' }]]);
  await assert.rejects(controller.ready(), failed); await assert.rejects(controller.dispose(), failed);
  assert.equal(f.delegate._initializeSessionMayThrow, f.original);
});

test('ordinary provisional disposal does not poison later native process configuration', async () => {
  const f = fixture(), controller = await install(f), discarded = f.session('discarded');
  const started = deferred();
  f.initialize(async () => { await started.promise; discarded.dispose(); throw new Error('PRIVATE_DISPOSED'); });
  const initialization = f.delegate._initializeSessionMayThrow(discarded);
  const readiness = controller.ready();
  started.resolve();
  await initialization; await readiness;
  assert.equal(f.calls.some(c => c[0] === 'close'), false);
  f.initialize(undefined);
  const next = f.session('replacement');
  await f.delegate._initializeSessionMayThrow(next); f.delegate._session = next;
  await controller.ready(); assert.equal(controller.appliedCount(), 2); await controller.dispose();
});

test('unexpected closure of an already configured current session cannot reuse cached readiness', async () => {
  const f = fixture(), controller = await install(f);
  assert.equal(controller.appliedCount(), 1);
  f.delegate._session.dispose();
  await assert.rejects(controller.ready(), failed);
  assert.equal(controller.appliedCount(), 1);
  await controller.dispose();
});

test('pin, ownership or existing-hook mismatch cannot alter a delegate', async () => {
  for (const config of [{ version: '1.64.1' }, { version: '1.63.0' }]) {
    const f = fixture(config); await assert.rejects(install(f), failed); assert.equal(f.delegate._initializeSessionMayThrow, f.original);
  }
  for (const mutate of [f => { f.playwright = { _connection: f.playwright._connection }; }, f => { f.page._connection = {}; }, f => { f.delegate._page = {}; }, f => { f.delegate._initializeSessionMayThrow = f.original; }]) {
    const f = fixture(); mutate(f); await assert.rejects(install(f), failed); assert.equal(f.calls.some(c => c[0] === 'send'), false);
  }
});

test('initial protocol failure or deadline restores hook, closes context and exposes no private error', async () => {
  for (const config of [{}, { immediateDeadline: true }]) {
    const f = fixture(config);
    f.send(config.immediateDeadline ? () => new Promise(() => {}) : () => { throw new Error('PRIVATE_PROTOCOL'); });
    await assert.rejects(install(f), failed);
    assert.equal(f.delegate._initializeSessionMayThrow, f.original);
    assert.equal(f.calls.filter(c => c[0] === 'close').length, 1);
    assert.equal(f.timers.every(delay => delay === 5000), true);
    assert.equal(f.cleared.length, f.timers.length);
  }
});
