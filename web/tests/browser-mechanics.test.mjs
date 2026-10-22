import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createRequire } from 'node:module';
import test from 'node:test';
import { Writable } from 'node:stream';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { run, main } = require('../e2e/browser-mechanics.cjs');
const { createBrowserOwner } = require('../e2e/runner-browser-owner.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
function environment(overrides = {}) {
  return {
    BROWSER_MECHANICS_RUN_ID: runId,
    E2E_ARTIFACTS: '/owned/private/browser',
    PATH: '/owned/tools',
    HOME: '/owned/home',
    TMPDIR: '/owned/tmp',
    PLAYWRIGHT_BROWSERS_PATH: '/owned/browser-cache',
    PLAYWRIGHT_MODULE: 'playwright',
    ...overrides,
  };
}
function fixture(options = {}) {
  const signals = new EventEmitter();
  const state = { servers: [], closeAll: 0, launchOptions: [], snapshots: [] };
  const browser = {};
  const playwright = {
    chromium: {
      async launchServer(launchOptions) {
        state.launchOptions.push(launchOptions);
        options.launchEntered?.resolve();
        await options.launchGate?.promise;
        const child = { exitCode: null, signalCode: null };
        const server = {
          closes: 0,
          kills: 0,
          process: () => child,
          wsEndpoint: () => 'owned-private-endpoint',
          async close() {
            this.closes++;
            options.closeEntered?.resolve();
            await options.closeGate?.promise;
            if (options.closeFailure) throw options.closeFailure;
            child.exitCode = 0;
          },
          async kill() {
            this.kills++;
            child.signalCode = 'SIGKILL';
          },
        };
        state.servers.push(server);
        return server;
      },
      async connect() {
        options.connectEntered?.resolve();
        await options.connectGate?.promise;
        if (options.connectFailure) throw options.connectFailure;
        return browser;
      },
    },
  };
  const createOwner = (configuration) => {
    const owner = createBrowserOwner({
      ...configuration,
      persist(records) {
        state.snapshots.push(structuredClone(records));
        configuration.persist(options.projectRecords?.(structuredClone(records)) ?? records);
      },
    });
    return {
      ...owner,
      async closeAll() {
        state.closeAll++;
        return owner.closeAll();
      },
    };
  };
  return {
    state,
    signals,
    browser,
    options: { env: environment(), playwright, signals, createOwner },
    assertClosed() {
      assert.equal(state.closeAll, 1);
      assert.equal(signals.listenerCount('SIGINT'), 0);
      assert.equal(signals.listenerCount('SIGTERM'), 0);
      for (const server of state.servers) assert.equal(server.closes, 1);
    },
  };
}
function deferred() {
  return Promise.withResolvers();
}
const launch = (runtime) => runtime.launchBrowser('chromium', { headless: true });

test('fixed mechanics suite captures environment once and reports actual clean owner closure', async () => {
  const f = fixture();
  const captured = f.options.env;
  const result = await run({
    ...f.options,
    async suite(runtime) {
      captured.HOME = '/late/foreign-home';
      assert.equal(runtime.env.HOME, '/owned/home');
      assert.equal(Object.isFrozen(runtime.env), true);
      assert.equal(runtime.env.BASE_URL, 'http://127.0.0.1:3109');
      assert.equal(runtime.env.E2E_BROWSER, 'chromium');
      assert.equal(runtime.env.IMPAIR_SCRIPT, 'none');
      assert.equal(runtime.env.IMPAIRED_PROFILES, 'baseline,silentAudio');
      assert.equal(runtime.env.IMPAIRED_SILENT_AUDIO, '1');
      assert.equal(runtime.env.IMPAIR_LOSS, '0');
      assert.equal(runtime.env.IMPAIR_UDP_PORT, '41100');
      assert.equal(runtime.env.TEST_BROWSER_HTTPS, '0');
      assert.equal(runtime.env.PLAYWRIGHT_BROWSERS_PATH, '/owned/browser-cache');
      assert.equal(runtime.env.PLAYWRIGHT_MODULE, 'playwright');
      await launch(runtime);
    },
  });
  f.assertClosed();
  assert.equal(result.status, 0);
  assert.equal(result.cause, undefined);
  assert.deepEqual(result.receipt, {
    schemaVersion: 1,
    runId,
    completed: true,
    phasePassed: true,
    browserCleanupPassed: true,
    cancelled: null,
    browserCount: 1,
    records: [
      {
        browserName: 'chromium',
        launchMode: 'browser-server',
        completed: true,
        passed: true,
        gracefulCloseCompleted: true,
        forceKillRequested: false,
        exitStatusObserved: true,
        exitCode: 0,
        signalCode: null,
      },
    ],
  });
});

test('a fulfilled phase without a browser cannot manufacture cleanup proof', async () => {
  const f = fixture();
  const result = await run({ ...f.options, async suite() {} });
  assert.equal(result.status, 1);
  assert.equal(result.receipt.phasePassed, true);
  assert.equal(result.receipt.browserCleanupPassed, false);
  assert.deepEqual(result.receipt.records, []);
  f.assertClosed();
});

test('missing or malformed negative lifecycle fields cannot become clean false/null evidence', async () => {
  for (const [key, value] of [
    ['forceKillRequested', undefined],
    ['forceKillRequested', null],
    ['forceKillRequested', 0],
    ['forceKillRequested', 'false'],
    ['signalCode', undefined],
    ['signalCode', ''],
    ['signalCode', false],
    ['signalCode', 0],
    ['signalCode', 'PRIVATE_SIGNAL'],
  ]) {
    const f = fixture({
      projectRecords(records) {
        for (const record of records) {
          if (!record.browserCleanup?.completed) continue;
          if (value === undefined) delete record.browserCleanup[key];
          else record.browserCleanup[key] = value;
        }
        return records;
      },
    });
    const result = await run({ ...f.options, suite: launch });
    assert.equal(result.status, 1, key);
    assert.equal(result.receipt.browserCleanupPassed, false, key);
    assert.equal(result.receipt.records[0].forceKillRequested, false);
    assert.equal(result.receipt.records[0].signalCode, null);
    assert.doesNotMatch(JSON.stringify(result.receipt), /PRIVATE_SIGNAL/);
    f.assertClosed();
  }
});

test('suite and cleanup failures preserve the first error and redact native payloads', async () => {
  const primary = new Error('PRIVATE_SUITE_TOKEN');
  const f = fixture({ closeFailure: new Error('PRIVATE_BROWSER_ENDPOINT') });
  const result = await run({
    ...f.options,
    async suite(runtime) {
      await launch(runtime);
      throw primary;
    },
  });
  assert.equal(result.status, 1);
  assert.equal(result.cause, primary);
  assert.equal(result.receipt.phasePassed, false);
  assert.equal(result.receipt.browserCleanupPassed, false);
  assert.equal(result.receipt.records[0].forceKillRequested, true);
  assert.equal(f.state.servers[0].kills, 1);
  assert.doesNotMatch(JSON.stringify(result.receipt), /PRIVATE_|errors|endpoint/);
  f.assertClosed();
});

test('a failed connection closes its already-created server exactly once', async () => {
  const failure = new Error('PRIVATE_CONNECT_FAILURE');
  const f = fixture({ connectFailure: failure });
  const result = await run({ ...f.options, suite: launch });
  assert.equal(result.status, 1);
  assert.equal(result.cause, failure);
  assert.equal(result.receipt.browserCleanupPassed, true);
  assert.equal(result.receipt.phasePassed, false);
  f.assertClosed();
});

test('repeated signals wait for an already-admitted launch before one final closeAll', async () => {
  for (const [signal, code] of [
    ['SIGINT', 130],
    ['SIGTERM', 143],
  ]) {
    const launchEntered = deferred();
    const launchGate = deferred();
    const f = fixture({ launchEntered, launchGate });
    const task = run({ ...f.options, suite: launch });
    await launchEntered.promise;
    f.signals.emit(signal);
    f.signals.emit('SIGTERM');
    assert.equal(f.state.closeAll, 0, 'a pre-launch snapshot is not final cleanup');
    launchGate.resolve();
    const result = await task;
    assert.equal(result.status, code);
    assert.equal(result.receipt.cancelled, code);
    assert.equal(result.receipt.browserCleanupPassed, true);
    f.assertClosed();
  }
});

test('a signal during connection failure waits for its internal cleanup before closeAll', async () => {
  const connectEntered = deferred();
  const connectGate = deferred();
  const f = fixture({
    connectEntered,
    connectGate,
    connectFailure: new Error('PRIVATE_LATE_CONNECT'),
  });
  const task = run({ ...f.options, suite: launch });
  await connectEntered.promise;
  f.signals.emit('SIGTERM');
  assert.equal(f.state.closeAll, 0);
  connectGate.resolve();
  const result = await task;
  assert.equal(result.status, 143);
  assert.equal(result.receipt.cancelled, 143);
  assert.equal(result.receipt.phasePassed, false);
  assert.equal(result.receipt.browserCleanupPassed, true);
  f.assertClosed();
});

test('signals and repeated explicit close calls share the same owner settlement', async () => {
  const closeEntered = deferred();
  const closeGate = deferred();
  const f = fixture({ closeEntered, closeGate });
  const task = run({
    ...f.options,
    async suite(runtime) {
      const browser = await launch(runtime);
      const first = runtime.closeBrowser(browser);
      assert.equal(runtime.closeBrowser(browser), first);
      await first;
      await runtime.closeBrowser(browser);
    },
  });
  await closeEntered.promise;
  f.signals.emit('SIGTERM');
  f.signals.emit('SIGINT');
  closeGate.resolve();
  const result = await task;
  assert.equal(result.status, 143);
  assert.equal(result.receipt.browserCleanupPassed, true);
  f.assertClosed();
});

test('a known phase failure stays primary when cancellation arrives during final cleanup', async () => {
  const closeEntered = deferred();
  const closeGate = deferred();
  const primary = new Error('PRIVATE_EARLIER_FAILURE');
  const f = fixture({ closeEntered, closeGate });
  const task = run({
    ...f.options,
    async suite(runtime) {
      await launch(runtime);
      throw primary;
    },
  });
  await closeEntered.promise;
  f.signals.emit('SIGTERM');
  closeGate.resolve();
  const result = await task;
  assert.equal(result.status, 1);
  assert.equal(result.cause, primary);
  assert.equal(result.receipt.cancelled, 143);
  assert.equal(result.receipt.browserCleanupPassed, true);
  f.assertClosed();
});

test('constructor failure removes handlers and cannot claim cleanup for an unreturned owner', async () => {
  const failure = new Error('PRIVATE_CONSTRUCTOR');
  const signals = new EventEmitter();
  const result = await run({
    env: environment(),
    playwright: {},
    signals,
    createOwner() {
      assert.equal(signals.listenerCount('SIGTERM'), 1);
      throw failure;
    },
    async suite() {
      assert.fail('no suite before owner admission');
    },
  });
  assert.equal(result.status, 1);
  assert.equal(result.cause, failure);
  assert.equal(result.receipt.browserCleanupPassed, false);
  assert.equal(signals.listenerCount('SIGINT'), 0);
  assert.equal(signals.listenerCount('SIGTERM'), 0);
});

test('custom workload and Node injection knobs refuse before any owner allocation', async () => {
  for (const [name, value] of [
    ['E2E_BROWSER', 'firefox'],
    ['BASE_URL', 'https://private.invalid'],
    ['IMPAIR_SCRIPT', '/untrusted/tool'],
    ['IMPAIRED_PROFILES', 'lossy'],
    ['IMPAIRED_SILENT_AUDIO', '0'],
    ['IMPAIR_LOSS', '5'],
    ['IMPAIR_DELAY_MS', '1'],
    ['IMPAIR_JITTER_MS', '1'],
    ['IMPAIR_RATE_KBIT', '800'],
    ['IMPAIR_DIRECTION', 'both'],
    ['IMPAIR_UDP_PORT', '41101'],
    ['IMPAIR_WORKERS', '2'],
    ['CANARY_MODE', '1'],
    ['CANARY_FORCE_RELAY', '1'],
    ['CANARY_CORRELATION_FILE', '/private/receipt'],
    ['CALL_OUTCOME_E2E', '1'],
    ['IMPAIRED_BLOCK_UDP', '1'],
    ['IMPAIRED_ROOM', 'private-room'],
    ['TEST_BROWSER_HTTPS', '1'],
    ['WEBKIT_TEST_NETWORK', '1'],
    ['NODE_OPTIONS', '--require=/private/module'],
    ['BROWSER_MECHANICS_RUN_ID', 'PRIVATE_TOKEN'],
    ['E2E_ARTIFACTS', '/owned/../foreign'],
  ]) {
    await assert.rejects(
      run({
        env: environment({ [name]: value }),
        createOwner() {
          assert.fail('invalid configuration allocated an owner');
        },
      }),
      /^Error: Browser mechanics configuration is invalid\.$/,
      name,
    );
  }
});

test('foreign project and duplicate launch requests never allocate a second browser', async () => {
  for (const name of ['firefox', 'chromium']) {
    const f = fixture();
    const result = await run({
      ...f.options,
      async suite(runtime) {
        await launch(runtime);
        await runtime.launchBrowser(name, {});
      },
    });
    assert.equal(result.status, 1);
    assert.equal(f.state.servers.length, 1);
    assert.equal(result.receipt.browserCleanupPassed, true);
    f.assertClosed();
  }
});

function sink(failure) {
  let text = '';
  return {
    stream: new Writable({
      write(bytes, _encoding, callback) {
        text += bytes.toString();
        callback(failure);
      },
    }),
    text: () => text,
  };
}

test('main emits only the bounded safe receipt after cleanup and never native errors', async () => {
  const f = fixture();
  const stdout = sink();
  const stderr = sink();
  const status = await main({
    ...f.options,
    stdout: stdout.stream,
    stderr: stderr.stream,
    async suite(runtime) {
      await launch(runtime);
      throw new Error('PRIVATE_NATIVE_DETAILS');
    },
  });
  f.assertClosed();
  assert.equal(status, 1);
  assert.ok(Buffer.byteLength(stdout.text()) <= 16 * 1024);
  assert.equal(JSON.parse(stdout.text()).browserCleanupPassed, true);
  assert.equal(stderr.text(), 'Browser mechanics failed.\n');
  assert.doesNotMatch(stdout.text() + stderr.text(), /PRIVATE_|owned-private-endpoint/);
});

test('stdout failure cannot pass and does not replace an already-selected interruption', async () => {
  for (const cancelled of [false, true]) {
    const f = fixture();
    const options = {
      ...f.options,
      stdout: sink(new Error('PRIVATE_STDOUT')).stream,
      stderr: sink().stream,
      async suite(runtime) {
        await launch(runtime);
        if (cancelled) f.signals.emit('SIGTERM');
      },
    };
    if (cancelled) assert.equal(await main(options), 143);
    else await assert.rejects(main(options), /Browser mechanics publication failed/);
    f.assertClosed();
  }
});

test('CLI rejects additional arguments without loading or launching a browser', () => {
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL('../e2e/browser-mechanics.cjs', import.meta.url)), 'unexpected'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.equal(result.status, 2);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'Browser mechanics arguments are invalid.\n');
});
