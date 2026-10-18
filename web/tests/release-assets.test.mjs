import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import rollover from '../e2e/release-assets.cjs';
import { deferred, flush } from './ui-fixture.mjs';

const original = 'a'.repeat(40);
const candidate = 'b'.repeat(40);

function lifecycle(failure) {
  const calls = [];
  const act = async (name) => {
    calls.push(name);
    if (name === failure) throw new Error('private bearer credentials and child output');
  };
  return {
    calls,
    driver: {
      async open(revision) {
        const old = revision === original;
        await act(old ? 'open-original' : 'open-candidate');
        return {
          loadAfterReplacement: () => act(old ? 'load-old' : 'load-new'),
          unchanged: () => act(old ? 'old-identity' : 'new-identity'),
        };
      },
      deploy: (revision) => act(revision === candidate ? 'deploy-candidate' : 'rollback'),
    },
  };
}

test('rollover retains both documents across upgrade and rollback before reporting success', async () => {
  const f = lifecycle();
  const report = await rollover.checkRollover(f.driver, original, candidate);
  assert.deepEqual(f.calls, [
    'open-original',
    'deploy-candidate',
    'load-old',
    'open-candidate',
    'rollback',
    'load-new',
    'old-identity',
    'new-identity',
  ]);
  assert.deepEqual(report, {
    passed: true,
    editions: 2,
    oldTabAfterUpgrade: true,
    newTabAfterRollback: true,
    backendMissesVerified: 2,
    documentsUnchanged: true,
    captureCalls: 0,
    htmlUncached: true,
    selectedVersionVerified: true,
  });
});

test('invalid or identical editions cannot open a page or deploy', async () => {
  for (const [before, after] of [
    ['invalid', candidate],
    [original, candidate.toUpperCase()],
    [original, original],
  ]) {
    const f = lifecycle();
    await assert.rejects(
      rollover.checkRollover(f.driver, before, after),
      (error) => error instanceof rollover.RolloverError && error.stage === 'configuration',
    );
    assert.deepEqual(f.calls, []);
  }
});

for (const failure of [
  'open-original',
  'deploy-candidate',
  'load-old',
  'open-candidate',
  'rollback',
  'load-new',
  'old-identity',
  'new-identity',
]) {
  test(`a failed ${failure} stops the sequence with a fixed content-free stage`, async () => {
    const f = lifecycle(failure);
    await assert.rejects(rollover.checkRollover(f.driver, original, candidate), (error) => {
      assert(error instanceof rollover.RolloverError);
      assert.equal(error.stage, failure.endsWith('identity') ? 'identity' : failure);
      assert.equal(error.message, 'Release asset rollover check failed');
      assert.equal(error.cause, undefined);
      assert.doesNotMatch(JSON.stringify(error), /private|bearer|credentials|output/);
      return true;
    });
    assert.equal(f.calls.at(-1), failure, 'later release or import actions must not run');
  });
}

async function browserFixture(t, options = {}) {
  const events = [];
  const response = deferred();
  const chunk = '/assets/rollover-0123456789abcdef.js';
  let init;
  let imports = 0;
  let cancelled = 0;
  let deployed = original;
  let deployments = 0;
  const versions = [];
  const state = {
    navigator: { mediaDevices: {} },
    DOMException,
    releaseRollover: {
      revision: original,
      identity: 'original-document',
      chunk,
      async load() {
        events.push('import');
        imports++;
        if (options.loadError) throw new Error('private native import error');
        response.resolve({ status: () => options.proxyStatus ?? 200 });
        return original;
      },
    },
  };
  vm.createContext(state);
  const page = {
    async goto(url) {
      events.push('navigate');
      assert.equal(url, 'https://localhost/asset-rollover.html');
      vm.runInContext(`(${init.toString()})()`, state);
      return {
        ok: () => true,
        headers: () => ({ 'cache-control': options.htmlCache ?? 'no-store' }),
      };
    },
    evaluate(fn) {
      return Promise.resolve().then(() => vm.runInContext(`(${fn.toString()})()`, state));
    },
    waitForResponse(predicate, settings) {
      events.push('response-listener');
      assert.equal(settings.timeout, 10_000);
      assert.equal(predicate({ url: () => 'https://localhost' + chunk }), true);
      assert.equal(predicate({ url: () => 'https://localhost/assets/unrelated.js' }), false);
      return response.promise;
    },
  };
  const context = {
    async addInitScript(fn) {
      init = fn;
    },
    async newPage() {
      return page;
    },
    request: {
      async get(url) {
        assert.equal(url, 'https://localhost/app-version.json');
        versions.push(deployed);
        return {
          status: () => options.versionStatus ?? 200,
          headers: () => ({ 'cache-control': options.versionCache ?? 'no-store' }),
          json: async () => ({
            revision:
              options.selectedRevision ??
              (options.staleRollback && deployments === 2 ? candidate : deployed),
          }),
        };
      },
    },
    async close() {
      events.push('close');
      response.reject(new Error('context closed'));
    },
  };
  t.mock.method(globalThis, 'fetch', async (input, settings) => {
    events.push('backend');
    assert.equal(input, 'http://127.0.0.1:3000' + chunk);
    assert.equal(settings.redirect, 'error');
    assert(settings.signal instanceof AbortSignal);
    return {
      status: options.backendStatus ?? 404,
      body: {
        async cancel() {
          cancelled++;
        },
      },
    };
  });
  const driver = await rollover.browserDriver(
    {
      async newContext(settings) {
        assert.deepEqual(settings, { ignoreHTTPSErrors: true });
        return context;
      },
    },
    async (executable, args, settings) => {
      assert.equal(executable, '/usr/bin/python3');
      assert.equal(args[0], '-B');
      assert.match(args[1], /\/ops\/ansible\/files\/release-public\.py$/);
      assert.equal(args[2], 'deploy');
      assert.deepEqual(settings, {
        timeout: 150_000,
        maxBuffer: 2 * 1024 * 1024,
        killSignal: 'SIGTERM',
        windowsHide: true,
      });
      deployed = args[3];
      deployments++;
      return { stdout: JSON.stringify({ passed: true, revision: deployed }) };
    },
  );
  return { driver, state, events, versions, imports: () => imports, cancelled: () => cancelled };
}

test('the browser driver proves direct404 before observing retained200 and keeps the document', async (t) => {
  const f = await browserFixture(t);
  const page = await f.driver.open(original);
  assert.equal(f.imports(), 0, 'the edition-specific chunk must be cold before replacement');
  await page.loadAfterReplacement();
  assert.equal(f.imports(), 1);
  assert.equal(f.cancelled(), 1);
  assert.deepEqual(f.events, ['navigate', 'backend', 'response-listener', 'import']);
  await page.unchanged();
});

test('a backend hit cannot count as retained delivery and its response is always released', async (t) => {
  const f = await browserFixture(t, { backendStatus: 200 });
  const page = await f.driver.open(original);
  await assert.rejects(page.loadAfterReplacement());
  assert.equal(f.imports(), 0);
  assert.equal(f.cancelled(), 1);
});

test('a missing proxy response cannot be counted as a successful import', async (t) => {
  const f = await browserFixture(t, { proxyStatus: 404 });
  const page = await f.driver.open(original);
  await assert.rejects(page.loadAfterReplacement());
});

test('an import failure owns the pending response rejection during cleanup', async (t) => {
  const f = await browserFixture(t, { loadError: true });
  const page = await f.driver.open(original);
  await assert.rejects(page.loadAfterReplacement(), /private native import error/);
  await f.driver.close();
  await flush();
  assert.deepEqual(f.events.slice(-3), ['response-listener', 'import', 'close']);
});

for (const options of [
  { htmlCache: 'public, max-age=31536000' },
  { versionCache: 'public, max-age=60' },
  { selectedRevision: candidate },
]) {
  test(`cached entry metadata or a stale selected version fails (${Object.keys(options)[0]})`, async (t) => {
    const f = await browserFixture(t, options);
    await assert.rejects(f.driver.open(original));
    assert.equal(f.imports(), 0);
  });
}

test('document replacement and unexpected capture are rejected independently', async (t) => {
  const f = await browserFixture(t);
  const page = await f.driver.open(original);
  f.state.releaseRollover.identity = 'different-document';
  await assert.rejects(page.unchanged());
  f.state.releaseRollover.identity = 'original-document';
  await assert.rejects(f.state.navigator.mediaDevices.getUserMedia(), /Capture is forbidden/);
  await assert.rejects(page.unchanged());
  assert.equal(f.state.__releaseCaptureCalls, 1);
});

test('each deployment verifies selected metadata, including rollback to the endpoint-less baseline', async (t) => {
  const f = await browserFixture(t);
  await f.driver.open(original);
  await f.driver.deploy(candidate);
  await f.driver.deploy(original);
  assert.deepEqual(f.versions, [original, candidate, original]);
});

test('stale candidate metadata after an otherwise successful rollback is rejected', async (t) => {
  const f = await browserFixture(t, { staleRollback: true });
  await f.driver.open(original);
  await f.driver.deploy(candidate);
  await assert.rejects(f.driver.deploy(original));
  assert.deepEqual(f.versions, [original, candidate, original]);
});

for (const options of [{ versionStatus: 404 }, { versionCache: 'public, max-age=60' }]) {
  test(`successful CLI deployment cannot bypass invalid proxy metadata (${Object.keys(options)[0]})`, async (t) => {
    const f = await browserFixture(t, options);
    await assert.rejects(f.driver.deploy(candidate));
    assert.deepEqual(f.versions, [candidate]);
  });
}

for (const broken of ['context', 'browser']) {
  test(`a failing ${broken} cleanup cannot prevent the other owned resource from closing`, async () => {
    const calls = [];
    const resource = (name) => ({
      close() {
        calls.push(name);
        if (name === broken) throw new Error('private cleanup details');
      },
    });
    await assert.rejects(
      rollover.closeBrowser(resource('browser'), resource('context')),
      (error) => error instanceof rollover.RolloverError && error.stage === 'cleanup',
    );
    assert.deepEqual(calls.sort(), ['browser', 'context']);
  });
}
