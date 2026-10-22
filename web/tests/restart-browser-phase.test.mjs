import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createRequire } from 'node:module';
import { Writable } from 'node:stream';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import vm from 'node:vm';

const require = createRequire(import.meta.url);
const { run, main, configuration } = require('../e2e/restart-browser-phase.cjs');
const { createBrowserOwner } = require('../e2e/runner-browser-owner.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
const env = {
  BROWSER_RESTART_RUN_ID: runId,
  BROWSER_RESTART_ENGINE: 'chromium',
  BROWSER_RESTART_PORT: '45101',
};
const gate = () => {
  let resolve;
  const promise = new Promise((done) => (resolve = done));
  return { promise, resolve };
};
const flush = async () => {
  for (let index = 0; index < 30; index++) await Promise.resolve();
};

function fixture(options = {}) {
  const signals = new EventEmitter();
  const timers = new Map();
  const state = {
    pages: [],
    commands: [],
    contexts: [],
    launches: [],
    connections: [],
    closes: 0,
    kills: 0,
    channelCloses: 0,
    cancellations: [],
    clock: 0,
    scripts: [],
    order: [],
  };
  const child = { exitCode: null, signalCode: null };
  const server = {
    process: () => child,
    wsEndpoint: () => 'owned-browser-endpoint',
    async close() {
      state.closes++;
      state.order.push('browser-close');
      options.closeEntered?.resolve();
      await options.duringClose?.(routeHandler);
      await options.closeGate?.promise;
      if (options.closeFailure) throw new Error('PRIVATE_CLOSE');
      child.exitCode = 0;
    },
    async kill() {
      state.kills++;
      child.signalCode = 'SIGKILL';
    },
  };
  let routeHandler;
  const newSocket = (page) => {
    const socket = new EventEmitter();
    socket.url = () => `ws://127.0.0.1:45101/${options.foreignSocket ? 'PRIVATE' : 'ws'}`;
    socket.receive = (message) =>
      socket.emit('framereceived', { payload: JSON.stringify(message) });
    socket.send = (message) => socket.emit('framesent', { payload: JSON.stringify(message) });
    page.socket = socket;
    page.emit('websocket', socket);
    return socket;
  };
  class Page extends EventEmitter {
    constructor(index) {
      super();
      this.index = index;
      this.frame = {};
      this.values = {};
      this.messages = [];
      this.realm = vm.createContext({ navigator: { mediaDevices: {} }, DOMException });
      for (const script of state.scripts) vm.runInContext(`(${script.toString()})()`, this.realm);
    }
    mainFrame() {
      return this.frame;
    }
    async goto(url, navigation) {
      assert.equal(url, 'http://127.0.0.1:45101');
      assert.deepEqual(navigation, { waitUntil: 'domcontentloaded' });
      const calls = [];
      await routeHandler({
        request: () => ({ url: () => (options.external ? 'https://PRIVATE.invalid' : url) }),
        continue: async () => calls.push('continue'),
        abort: async () => calls.push('abort'),
      });
      assert.deepEqual(calls, [options.external ? 'abort' : 'continue']);
      this.emit('framenavigated', this.frame);
      if (options.pageError) this.emit('pageerror', new Error('PRIVATE_PAGE'));
      options.navigationEntered?.resolve();
      await options.navigationGate?.promise;
    }
    locator(selector) {
      const page = this;
      return {
        async fill(value) {
          page.values[selector] = value;
        },
        async click() {
          if (selector === '#join-btn') {
            assert.equal(page.values['#name-input'], `Restart guest ${page.index + 1}`);
            assert.match(page.values['#room-input'], /^restart-browser-/);
            const socket = newSocket(page);
            socket.send({ type: 'joinRoom' });
            if (!options.noInitialJoin) socket.receive({ type: 'roomJoined' });
            if (options.malformed) socket.emit('framereceived', { payload: 'PRIVATE_INVALID' });
          } else {
            assert.equal(selector, '#leave-btn');
            if (options.advanceFinalCounters) page.emit('framenavigated', page.frame);
          }
        },
        async waitFor(value) {
          assert.ok(['#room-screen', '#join-screen'].includes(selector));
          assert.deepEqual(value, { state: 'visible' });
        },
        async press(key) {
          assert.equal(selector, '#chat-input');
          assert.equal(key, 'Enter');
          const text = page.values[selector];
          const dropped = text.includes(' before ') ? options.dropBefore : options.dropAfter;
          if (!dropped) for (const peer of state.pages) peer.messages.push(text);
        },
        async inputValue() {
          return page.values[selector];
        },
        getByText(text, value) {
          assert.equal(selector, '#chat-messages');
          assert.deepEqual(value, { exact: true });
          return {
            async waitFor() {
              if (!page.messages.includes(text)) throw new Error('PRIVATE_MISSING_CHAT');
            },
          };
        },
      };
    }
    async evaluate(source) {
      if (options.invalidCapture) return { user: 0, display: 0, PRIVATE: 'SECRET' };
      return structuredClone(vm.runInContext(`(${source.toString()})()`, this.realm));
    }
  }
  const context = {
    setDefaultTimeout(value) {
      assert.equal(value, 10000);
    },
    async route(pattern, handler) {
      assert.equal(pattern, '**/*');
      routeHandler = handler;
    },
    async addInitScript(script) {
      state.scripts.push(script);
    },
    async newPage() {
      const page = new Page(state.pages.length);
      state.pages.push(page);
      return page;
    },
  };
  const browser = {
    version: () => 'inert-browser-1',
    async newContext(value) {
      state.contexts.push(value);
      return context;
    },
  };
  const playwright = Object.fromEntries(
    ['chromium', 'firefox', 'webkit'].map((name) => [
      name,
      {
        async launchServer(value) {
          state.launches.push([name, value]);
          options.launchEntered?.resolve();
          await options.launchGate?.promise;
          if (options.launchFailure) throw new Error('PRIVATE_LAUNCH');
          return server;
        },
        async connect(...args) {
          state.connections.push(args);
          options.connectEntered?.resolve();
          await options.connectGate?.promise;
          if (options.connectFailure) throw new Error('PRIVATE_CONNECT');
          return browser;
        },
      },
    ]),
  );
  let pendingRequest;
  const channel = {
    async request(phase) {
      state.commands.push(phase);
      state.order.push(phase);
      if (options.requestGate && phase === 'stop-first') {
        options.requestEntered?.resolve();
        await new Promise((resolve, reject) => (pendingRequest = { resolve, reject }));
      }
      if (phase === 'stop-first' && !options.noRestartNotice)
        for (const page of state.pages) page.socket.receive({ type: 'serverRestarting' });
      if (phase === 'start-second') {
        state.clock += 100;
        for (const page of state.pages) {
          if (!options.noRejoin) newSocket(page).receive({ type: 'roomJoined' });
          if (options.navigateOnRestart) page.emit('framenavigated', page.frame);
          if (options.terminalRoom) page.socket.receive({ type: 'roomClosed' });
          if (options.lostDraft) page.values['#chat-input'] = '';
          if (options.capture)
            await vm.runInContext(
              'navigator.mediaDevices.getUserMedia().catch(() => {})',
              page.realm,
            );
        }
      }
      if (phase === 'stop-second' && options.advanceFinalCounters)
        for (const page of state.pages) {
          page.socket.receive({ type: 'serverRestarting' });
          page.socket.send({ type: 'afterRecovery' });
        }
    },
    cancel(code) {
      state.cancellations.push(code);
      pendingRequest?.reject(new Error('PRIVATE_CANCELLED_CONTROL'));
    },
    async finish() {
      state.channelCloses++;
      await options.channelCloseGate?.promise;
      if (options.channelFailure) throw new Error('PRIVATE_CONTROL_CLOSE');
    },
  };
  return {
    state,
    signals,
    playwright,
    async run(overrides = {}) {
      const result = await run({
        env: { ...env, ...overrides.env },
        playwright,
        channel,
        signals,
        now: () => state.clock,
        wait: async (milliseconds) => {
          state.clock += milliseconds;
        },
        uuid: () => runId,
        setTimer(callback, milliseconds) {
          timers.set(callback, milliseconds);
          return callback;
        },
        clearTimer(callback) {
          timers.delete(callback);
        },
        ...overrides,
      });
      assert.equal(timers.size, 0);
      assert.equal(signals.listenerCount('SIGINT'), 0);
      assert.equal(signals.listenerCount('SIGTERM'), 0);
      assert.equal(state.channelCloses, 1);
      assert.ok(Buffer.byteLength(JSON.stringify(result.receipt)) <= 32 * 1024);
      assert.ok(!JSON.stringify(result.receipt).includes('PRIVATE'));
      return result;
    },
    options: { env, playwright, channel, signals },
  };
}

test('the complete fixed browser journey preserves every assertion and three-transition order', async () => {
  for (const browser of ['chromium', 'firefox', 'webkit']) {
    const f = fixture();
    const result = await f.run({ env: { ...env, BROWSER_RESTART_ENGINE: browser } });
    assert.equal(result.status, 0);
    assert.equal(result.receipt.phasePassed, true);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.deepEqual(f.state.commands, ['stop-first', 'start-second', 'stop-second']);
    assert.deepEqual(f.state.launches, [[browser, { headless: true, timeout: 15000 }]]);
    assert.deepEqual(f.state.connections, [['owned-browser-endpoint', { timeout: 5000 }]]);
    assert.deepEqual(f.state.contexts, [
      { permissions: [], viewport: { width: 1280, height: 900 } },
    ]);
    assert.deepEqual(result.receipt.observation.captureCalls, [
      { user: 0, display: 0 },
      { user: 0, display: 0 },
    ]);
    assert.equal(result.receipt.observation.rejoinMilliseconds, 100);
    assert.equal(result.receipt.observation.draftPreserved, true);
    assert.deepEqual(result.receipt.observation.messageDelivery, { before: true, after: true });
    assert.deepEqual(result.receipt.observation.navigationCountsBeforeRestart, [1, 1]);
    assert.deepEqual(result.receipt.observation.recoveryPages, result.receipt.observation.pages);
    assert.deepEqual(
      result.receipt.observation.pages.map((page) => page.received),
      [
        { roomJoined: 2, serverRestarting: 1 },
        { roomJoined: 2, serverRestarting: 1 },
      ],
    );
    assert.equal(f.state.closes, 1);
    assert.equal(f.state.kills, 0);
    assert.ok(f.state.order.indexOf('browser-close') > f.state.order.indexOf('stop-second'));
    for (const page of f.state.pages)
      for (const expression of [
        'navigator.mediaDevices.getUserMedia',
        'navigator.mediaDevices.getDisplayMedia',
        'navigator.getUserMedia',
        'navigator.webkitGetUserMedia',
        'navigator.mozGetUserMedia',
      ])
        assert.equal(vm.runInContext(`typeof ${expression}`, page.realm), 'function');
    const realm = f.state.pages[0].realm;
    for (const method of [
      'navigator.mediaDevices.getUserMedia',
      'navigator.mediaDevices.getDisplayMedia',
      'navigator.getUserMedia',
      'navigator.webkitGetUserMedia',
      'navigator.mozGetUserMedia',
    ])
      await assert.rejects(vm.runInContext(`${method}()`, realm), { name: 'NotAllowedError' });
    assert.deepEqual(structuredClone(vm.runInContext('__restartCaptureCalls', realm)), {
      user: 4,
      display: 1,
    });
  }
});

test('the immutable recovery checkpoint precedes leave navigation and final shutdown events', async () => {
  const f = fixture({ advanceFinalCounters: true });
  const result = await f.run();
  assert.equal(result.status, 0);
  assert.equal(result.receipt.phasePassed, true);
  assert.equal(result.receipt.browserCleanupPassed, true);
  const { pages, recoveryPages, navigationCountsBeforeRestart } = result.receipt.observation;
  assert.deepEqual(navigationCountsBeforeRestart, [1, 1]);
  assert.deepEqual(
    recoveryPages.map((page) => page.navigations),
    [1, 1],
  );
  assert.deepEqual(
    pages.map((page) => page.navigations),
    [2, 2],
  );
  for (let index = 0; index < 2; index++) {
    assert.equal(recoveryPages[index].received.roomJoined, 2);
    assert.equal(recoveryPages[index].received.serverRestarting, 1);
    assert.equal(pages[index].received.serverRestarting, 2);
    assert.equal(recoveryPages[index].sent.afterRecovery, undefined);
    assert.equal(pages[index].sent.afterRecovery, 1);
    assert.notEqual(recoveryPages[index], pages[index]);
    assert.notEqual(recoveryPages[index].received, pages[index].received);
    assert.notEqual(recoveryPages[index].sent, pages[index].sent);
    pages[index].received.roomJoined = 99;
    pages[index].sent.joinRoom = 99;
    assert.equal(recoveryPages[index].received.roomJoined, 2);
    assert.equal(recoveryPages[index].sent.joinRoom, 1);
  }
});

test('each original browser assertion fails privately while the same browser still closes', async () => {
  for (const name of [
    'dropBefore',
    'dropAfter',
    'noInitialJoin',
    'noRestartNotice',
    'noRejoin',
    'navigateOnRestart',
    'lostDraft',
    'terminalRoom',
    'capture',
    'malformed',
    'pageError',
    'external',
    'foreignSocket',
    'invalidCapture',
  ]) {
    const f = fixture({ [name]: true });
    const result = await f.run();
    assert.notEqual(result.status, 0, name);
    assert.equal(result.receipt.phasePassed, false, name);
    assert.equal(result.receipt.browserCleanupPassed, true, name);
    assert.equal(f.state.closes, 1, name);
    if (name === 'noRestartNotice') assert.deepEqual(f.state.commands, ['stop-first']);
  }
});

test('late restart notices and rejoins cannot cross their phase deadline on a resumed turn', async () => {
  for (const phase of ['notice', 'rejoin']) {
    for (const extra of [0, 1]) {
      const f = fixture(phase === 'notice' ? { noRestartNotice: true } : { noRejoin: true });
      let waits = 0;
      const result = await f.run({
        async wait() {
          waits++;
          f.state.clock += (phase === 'notice' ? 5000 : 30000) + extra;
          for (const page of f.state.pages)
            page.socket.receive({ type: phase === 'notice' ? 'serverRestarting' : 'roomJoined' });
        },
      });
      assert.equal(waits, 1);
      assert.equal(result.status, 124);
      assert.equal(result.receipt.phasePassed, false);
      assert.equal(result.receipt.browserCleanupPassed, true);
      assert.equal(f.state.closes, 1);
      assert.deepEqual(
        f.state.commands,
        phase === 'notice' ? ['stop-first'] : ['stop-first', 'start-second'],
      );
      assert.ok(
        result.receipt.observation.pages.every(
          (page) =>
            page.received[phase === 'notice' ? 'serverRestarting' : 'roomJoined'] ===
            (phase === 'notice' ? 1 : 2),
        ),
      );
    }
  }
});

test('positive conditions still require a finite monotonic clock inside the phase window', async () => {
  for (const jump of [10000, NaN, -1]) {
    const f = fixture();
    let samples = 0;
    const result = await f.run({
      now() {
        samples++;
        return samples < 3 ? 0 : jump;
      },
    });
    assert.equal(result.status, jump === 10000 ? 124 : 1);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.deepEqual(f.state.commands, []);
    assert.equal(f.state.closes, 1);
  }
});

test('failed launch/connect and malformed negative lifecycle evidence cannot become success', async () => {
  for (const name of ['launchFailure', 'connectFailure', 'closeFailure', 'channelFailure']) {
    const f = fixture({ [name]: true });
    const result = await f.run();
    assert.notEqual(result.status, 0, name);
    assert.ok(result.cause);
    if (name === 'connectFailure') assert.equal(f.state.closes, 1);
    if (name === 'closeFailure') {
      assert.equal(f.state.kills, 1);
      assert.equal(result.receipt.browserCleanupPassed, false);
    }
  }
  for (const mutation of [
    (value) => delete value.forceKillRequested,
    (value) => (value.forceKillRequested = null),
    (value) => delete value.signalCode,
    (value) => (value.signalCode = 'PRIVATE_SIGNAL'),
    (value) => delete value.exitCode,
  ]) {
    const f = fixture();
    const result = await f.run({
      createOwner(options) {
        const persist = options.persist;
        return createBrowserOwner({
          ...options,
          persist(records) {
            const copied = structuredClone(records);
            if (copied[0]?.browserCleanup?.completed) mutation(copied[0].browserCleanup);
            persist(copied);
          },
        });
      },
    });
    assert.notEqual(result.status, 0);
    assert.equal(result.receipt.browserCleanupPassed, false);
  }
});

test('repeated signals during admitted launch or connect wait and retire exactly once', async () => {
  for (const stage of ['launch', 'connect']) {
    const entered = gate();
    const held = gate();
    const f = fixture({ [`${stage}Entered`]: entered, [`${stage}Gate`]: held });
    const task = f.run();
    await entered.promise;
    f.signals.emit('SIGTERM');
    f.signals.emit('SIGINT');
    await flush();
    assert.equal(f.state.closes, 0);
    held.resolve();
    const result = await task;
    assert.equal(result.status, 143);
    assert.equal(result.receipt.cancelled, 143);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.equal(f.state.closes, 1);
    assert.deepEqual(f.state.commands, []);
  }
});

test('signals retain in-flight locator/request/close boundaries and joined channel close', async () => {
  const entered = gate();
  const held = gate();
  const f = fixture({ navigationEntered: entered, navigationGate: held });
  const task = f.run();
  await entered.promise;
  f.signals.emit('SIGINT');
  await flush();
  assert.equal(f.state.closes, 0);
  held.resolve();
  assert.equal((await task).status, 130);
  const requestEntered = gate();
  const writeClose = gate();
  const request = fixture({ requestEntered, requestGate: true, channelCloseGate: writeClose });
  let done = false;
  const pending = request.run().then((value) => {
    done = true;
    return value;
  });
  await requestEntered.promise;
  request.signals.emit('SIGTERM');
  await flush();
  assert.equal(request.state.closes, 1);
  assert.equal(done, false);
  writeClose.resolve();
  assert.equal((await pending).status, 143);
  const closeEntered = gate();
  const closeGate = gate();
  const closing = fixture({ closeEntered, closeGate });
  const closed = closing.run();
  await closeEntered.promise;
  closing.signals.emit('SIGTERM');
  closeGate.resolve();
  const result = await closed;
  assert.equal(result.status, 143);
  assert.equal(result.receipt.phasePassed, true);
  assert.equal(closing.state.closes, 1);
});

test('first assertion failure survives late cancellation and independent cleanup failures', async () => {
  const entered = gate();
  const held = gate();
  const f = fixture({
    dropBefore: true,
    closeEntered: entered,
    closeGate: held,
    channelFailure: true,
  });
  const task = f.run();
  await entered.promise;
  f.signals.emit('SIGTERM');
  held.resolve();
  const result = await task;
  assert.equal(result.status, 1);
  assert.equal(result.receipt.cancelled, 143);
  assert.match(result.cause.message, /PRIVATE_MISSING_CHAT/);
});

test('route continuations admitted during browser close are joined before the receipt', async () => {
  const first = gate();
  const second = gate();
  const admitted = gate();
  let continuation;
  const f = fixture({
    async duringClose(route) {
      continuation = route({
        request: () => ({ url: () => 'http://127.0.0.1:45101' }),
        continue: async () => assert.fail('Closing routes must be aborted'),
        async abort() {
          admitted.resolve();
          await first.promise;
          void route({
            request: () => ({ url: () => 'http://127.0.0.1:45101' }),
            continue: async () => assert.fail('Closing routes must be aborted'),
            async abort() {
              await second.promise;
              throw new Error('PRIVATE_ROUTE');
            },
          }).catch(() => {});
        },
      });
      continuation.catch(() => {});
    },
  });
  let done = false;
  const task = f.run().then((result) => {
    done = true;
    return result;
  });
  await admitted.promise;
  await flush();
  assert.equal(done, false);
  first.resolve();
  await continuation;
  await flush();
  assert.equal(done, false);
  second.resolve();
  const result = await task;
  assert.equal(result.status, 1);
  assert.equal(result.receipt.browserCleanupPassed, true);
  assert.match(result.cause.message, /PRIVATE_ROUTE/);
});

test('external requests and page errors during otherwise-positive close cannot report zero attempts', async () => {
  for (const event of ['external', 'pageerror']) {
    const held = gate();
    const entered = gate();
    const f = fixture({
      async duringClose(route) {
        if (event === 'pageerror')
          f.state.pages[0].emit('pageerror', new Error('PRIVATE_LATE_PAGE'));
        else
          await route({
            request: () => ({ url: () => 'https://PRIVATE.invalid' }),
            continue: async () => assert.fail('External request must remain guarded during close'),
            async abort() {
              entered.resolve();
              await held.promise;
            },
          });
      },
    });
    const task = f.run();
    if (event === 'external') {
      await entered.promise;
      held.resolve();
    }
    const result = await task;
    assert.equal(result.status, 1);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.equal(
      result.receipt.observation[event === 'external' ? 'blockedExternalRequests' : 'pageErrors'],
      1,
    );
  }
});

test('fixed configuration rejects runtime injection and external origins before owners', () => {
  for (const patch of [
    { BROWSER_RESTART_RUN_ID: 'PRIVATE' },
    { BROWSER_RESTART_ENGINE: 'safari' },
    { BROWSER_RESTART_PORT: 'https://PRIVATE.invalid' },
    { BROWSER_RESTART_PORT: '08080' },
    { BROWSER_RESTART_PORT: '65536' },
    { NODE_OPTIONS: '--inspect' },
  ])
    assert.throws(() => configuration({ ...env, ...patch }));
  const mutable = { ...env };
  const captured = configuration(mutable);
  mutable.BROWSER_RESTART_PORT = '9999';
  assert.equal(captured.port, 45101);
});

test('BrowserOwner explicit five-second connect preserves every default caller argument', async () => {
  for (const timeout of [undefined, 5000]) {
    const calls = [];
    const child = { exitCode: null, signalCode: null };
    const server = {
      wsEndpoint: () => 'owned-endpoint',
      process: () => child,
      close: async () => {
        child.exitCode = 0;
      },
      kill: async () => {},
    };
    const owner = createBrowserOwner({
      browserName: 'chromium',
      connectTimeoutMs: timeout,
      persist() {},
      playwright: {
        chromium: {
          launchServer: async () => server,
          connect: async (...args) => {
            calls.push(args);
            return {};
          },
        },
      },
    });
    await owner.launchBrowser('chromium', {});
    assert.deepEqual(calls, [
      timeout === undefined ? ['owned-endpoint'] : ['owned-endpoint', { timeout: 5000 }],
    ]);
    await owner.closeAll();
  }
  for (const timeout of [0, -1, 1.5, Infinity, '5000'])
    assert.throws(() => createBrowserOwner({ connectTimeoutMs: timeout }));
});

test('CLI emits one bounded final receipt and fixed failure text after cleanup', async () => {
  const f = fixture();
  const bytes = [];
  const errors = [];
  const stdout = new Writable({
    write(chunk, _encoding, callback) {
      bytes.push(chunk);
      callback();
    },
  });
  const stderr = new Writable({
    write(chunk, _encoding, callback) {
      errors.push(chunk);
      callback();
    },
  });
  const status = await main({ ...f.options, stdout, stderr });
  assert.equal(status, 0);
  const receipt = JSON.parse(Buffer.concat(bytes).toString());
  assert.equal(receipt.browserCleanupPassed, true);
  assert.equal(receipt.observation.phase, 'complete');
  assert.equal(errors.length, 0);
  assert.equal(f.state.closes, 1);
});

test('stdout failure stays nonzero after cleanup and preserves known interruption', async () => {
  for (const interrupted of [false, true]) {
    const entered = gate();
    const f = fixture(interrupted ? { requestEntered: entered, requestGate: true } : {});
    const stdout = new Writable({
      write(_chunk, _encoding, callback) {
        callback(new Error('PRIVATE_OUTPUT'));
      },
    });
    const stderr = new Writable({
      write(_chunk, _encoding, callback) {
        callback();
      },
    });
    const task = main({ ...f.options, stdout, stderr });
    if (interrupted) {
      await entered.promise;
      f.signals.emit('SIGTERM');
    }
    assert.equal(await task, interrupted ? 143 : 1);
    assert.equal(f.state.closes, 1);
    assert.equal(f.signals.listenerCount('SIGTERM'), 0);
  }
});

test('CLI rejects arguments privately before loading a runtime or allocating its channel', () => {
  const output = spawnSync(
    process.execPath,
    [
      fileURLToPath(new URL('../e2e/restart-browser-phase.cjs', import.meta.url)),
      'PRIVATE_ARGUMENT',
    ],
    { env: {}, timeout: 2000 },
  );
  assert.equal(output.status, 2);
  assert.equal(output.stdout.length, 0);
  assert.equal(output.stderr.toString(), 'Restart browser phase arguments are invalid.\n');
});
