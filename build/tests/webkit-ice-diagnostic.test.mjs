import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const source = await readFile(new URL('../../web/e2e/webkit-ice-diagnostic.cjs', import.meta.url), 'utf8');
const plain = value => JSON.parse(JSON.stringify(value));

function fixture({ env = { WEBKIT_ICE_DIAGNOSTIC: '1' }, platform = 'darwin', version = '1.64.0', send, immediateDeadline = false } = {}) {
  const calls = [], cleared = [], timeouts = [];
  const connection = { toImpl: page => { calls.push(['toImpl', page]); return implementation; } };
  const playwright = { _connection: connection };
  const page = { _connection: connection };
  const session = { isDisposed: () => false, send: async (...args) => { calls.push(['send', ...args]); return send?.(...args); } };
  class _WKPage {}
  const delegate = new _WKPage();
  const implementation = { delegate };
  Object.assign(delegate, { _page: implementation, _session: session });
  const module = { exports: {} };
  vm.runInNewContext(source, {
    module, process: { env, platform },
    require: name => {
      calls.push(['require', name]);
      if (name === 'playwright/package.json') return { version };
      if (name === 'playwright') return playwright;
      throw new Error('Unexpected dependency');
    },
    setTimeout: (callback, delay) => {
      timeouts.push(delay);
      if (immediateDeadline) queueMicrotask(callback);
      return 'owned-deadline';
    },
    clearTimeout: timer => cleared.push(timer),
  }, { timeout: 1000 });
  return { configure: module.exports.configureWebKitIceDiagnostic, calls, cleared, timeouts, playwright, page, connection, delegate, implementation, session };
}
const options = filteringEnabled => ({ browserName: 'webkit', filteringEnabled });

test('explicit diagnostic opt-in is required before reading dependencies or touching a page', async () => {
  for (const value of [undefined, '', '0', 'true']) {
    const f = fixture({ env: { WEBKIT_ICE_DIAGNOSTIC: value } });
    await assert.rejects(f.configure(f.playwright, f.page, options(false)), /requires WEBKIT_ICE_DIAGNOSTIC=1/);
    assert.deepEqual(f.calls, []);
  }
});

test('only macOS WebKit with an explicit boolean reaches the private transport', async () => {
  for (const platform of ['linux', 'win32']) {
    const f = fixture({ platform });
    await assert.rejects(f.configure(f.playwright, f.page, options(false)), /requires macOS WebKit/);
    assert.deepEqual(f.calls, []);
  }
  for (const invalid of [{}, null, { browserName: 'chromium', filteringEnabled: false }, ...[undefined, null, 0, 'false', {}].map(filteringEnabled => ({ browserName: 'webkit', filteringEnabled }))]) {
    const f = fixture();
    await assert.rejects(f.configure(f.playwright, f.page, invalid), /requires macOS WebKit/);
    assert.deepEqual(f.calls, []);
  }
});

test('both controlled settings send only the audited native WebKit command', async () => {
  for (const enabled of [true, false]) {
    const f = fixture();
    assert.deepEqual(plain(await f.configure(f.playwright, f.page, options(enabled))), {
      setting: 'ICECandidateFilteringEnabled', value: enabled, playwrightVersion: '1.64.0',
    });
    assert.deepEqual(f.calls.slice(0, 2), [['require', 'playwright/package.json'], ['require', 'playwright']]);
    assert.equal(f.calls[2][0], 'toImpl'); assert.equal(f.calls[2][1], f.page);
    assert.deepEqual(plain(f.calls.slice(3)), [['send', 'Page.overrideSetting', { setting: 'ICECandidateFilteringEnabled', value: enabled }]]);
    assert.deepEqual(f.timeouts, [5000]); assert.deepEqual(f.cleared, ['owned-deadline']);
  }
});

test('a changed Playwright version fails before accessing the internal bridge', async () => {
  for (const version of ['1.63.0', '1.64.1', null]) {
    const f = fixture({ version });
    await assert.rejects(f.configure(f.playwright, f.page, options(false)), /configuration failed/);
    assert.deepEqual(f.calls, [['require', 'playwright/package.json']]);
  }
});

test('foreign clients, pages, implementations and disposed sessions cannot send commands', async () => {
  const mutations = [
    f => { f.playwright = { _connection: f.connection }; },
    f => { f.page._connection = {}; },
    f => { f.connection.toImpl = undefined; },
    f => { f.connection.toImpl = () => { throw new Error('PRIVATE_PROTOCOL'); }; },
    f => { f.implementation.delegate = {}; },
    f => { f.delegate._page = {}; },
    f => { f.session.isDisposed = () => true; },
    f => { f.session.send = undefined; },
  ];
  for (const mutate of mutations) {
    const f = fixture(); mutate(f);
    await assert.rejects(f.configure(f.playwright, f.page, options(false)), error => {
      assert.equal(error.message, 'WebKit ICE diagnostic configuration failed.'); return true;
    });
    assert.equal(f.calls.some(call => call[0] === 'send'), false);
  }
});

test('protocol errors and a stalled command produce only a fixed error and clear the owned deadline', async () => {
  for (const config of [
    { send: () => { throw new Error('PRIVATE_ADDRESS_AND_PROTOCOL'); } },
    { send: () => new Promise(() => {}), immediateDeadline: true },
  ]) {
    const f = fixture(config);
    await assert.rejects(f.configure(f.playwright, f.page, options(false)), error => {
      assert.equal(error.message, 'WebKit ICE diagnostic configuration failed.'); return true;
    });
    assert.deepEqual(f.cleared, ['owned-deadline']);
  }
});

test('the diagnostic transport remains tied to the exact audited package and lock pin', async () => {
  const specification = JSON.parse(await readFile(new URL('../../web/e2e/package.json', import.meta.url), 'utf8'));
  const lock = JSON.parse(await readFile(new URL('../../web/e2e/package-lock.json', import.meta.url), 'utf8'));
  assert.equal(specification.devDependencies.playwright, '1.64.0');
  assert.equal(lock.packages['node_modules/playwright-core'].version, '1.64.0');
});
