/** Rust owns both guest servers; this fixed phase owns one Playwright browser. */
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');
const { createBrowserOwner } = require('./runner-browser-owner.cjs');
const { createChannel, statusOf } = require('./restart-browser-channel.cjs');

const ENGINES = ['chromium', 'firefox', 'webkit'];
const LIMIT = 32 * 1024;
class PhaseFailure extends Error {
  constructor(status) {
    super('Restart browser phase failed.');
    this.status = status;
  }
}
function configuration(input) {
  const env = { ...input };
  const runId = env.BROWSER_RESTART_RUN_ID;
  const browser = env.BROWSER_RESTART_ENGINE;
  const port = env.BROWSER_RESTART_PORT;
  if (
    typeof runId !== 'string' ||
    !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(runId) ||
    !ENGINES.includes(browser) ||
    typeof port !== 'string' ||
    !/^[1-9][0-9]{3,4}$/.test(port) ||
    Number(port) > 65535 ||
    (env.NODE_OPTIONS !== undefined && env.NODE_OPTIONS !== '')
  )
    throw new PhaseFailure(2);
  return Object.freeze({ runId, browser, port: Number(port) });
}
function validRecord(record, browser) {
  const value = record?.browserCleanup;
  return (
    record?.browserName === browser &&
    record?.launchMode === 'browser-server' &&
    [
      'completed',
      'passed',
      'gracefulCloseCompleted',
      'forceKillRequested',
      'exitStatusObserved',
    ].every((key) => typeof value?.[key] === 'boolean') &&
    (value.exitCode === null || (Number.isSafeInteger(value.exitCode) && value.exitCode >= 0)) &&
    (value.signalCode === null ||
      (typeof value.signalCode === 'string' && /^SIG[A-Z0-9]{1,16}$/.test(value.signalCode)))
  );
}
function project(record, browser) {
  const value = record?.browserCleanup;
  return {
    browserName: record?.browserName === browser ? browser : null,
    launchMode: record?.launchMode === 'browser-server' ? 'browser-server' : null,
    completed: value?.completed === true,
    passed: value?.passed === true,
    gracefulCloseCompleted: value?.gracefulCloseCompleted === true,
    forceKillRequested: value?.forceKillRequested === true,
    exitStatusObserved: value?.exitStatusObserved === true,
    exitCode: Number.isSafeInteger(value?.exitCode) && value.exitCode >= 0 ? value.exitCode : null,
    signalCode:
      typeof value?.signalCode === 'string' && /^SIG[A-Z0-9]{1,16}$/.test(value.signalCode)
        ? value.signalCode
        : null,
  };
}
function clean(value) {
  return (
    value.completed &&
    value.passed &&
    value.gracefulCloseCompleted &&
    !value.forceKillRequested &&
    value.exitStatusObserved &&
    value.exitCode === 0 &&
    value.signalCode === null
  );
}

async function run({
  env = process.env,
  playwright,
  createOwner = createBrowserOwner,
  channel: selectedChannel,
  signals = process,
  now = () => performance.now(),
  wait = delay,
  uuid = randomUUID,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
} = {}) {
  const config = configuration(env);
  const channel = selectedChannel ?? createChannel({ runId: config.runId });
  const observation = {
    browser: config.browser,
    phase: 'setup',
    pages: [],
    recoveryPages: null,
    captureCalls: [],
    pageErrors: 0,
    blockedExternalRequests: 0,
    browserVersion: null,
    rejoinMilliseconds: null,
    draftPreserved: null,
    messageDelivery: null,
    navigationCountsBeforeRestart: null,
  };
  const origin = `http://127.0.0.1:${config.port}`;
  let owner;
  let closing = false;
  let status = 0;
  let cause;
  let cancelled = null;
  let records = [];
  let recordsValid = false;
  let phasePassed = false;
  let ownerClosed = false;
  let retirement;
  const pending = new Set();
  const routes = new Set();
  const fail = (error, code = error instanceof PhaseFailure ? error.status : statusOf(error)) => {
    if (status === 0) {
      status = code || 1;
      cause = error;
    }
  };
  const check = () => {
    if (closing || status !== 0) throw cause ?? new PhaseFailure(1);
    if (observation.pageErrors || observation.blockedExternalRequests) throw new PhaseFailure(1);
  };
  const track = (promise, collection = pending) => {
    collection.add(promise);
    promise.then(
      () => collection.delete(promise),
      (error) => {
        collection.delete(promise);
        fail(error);
      },
    );
    return promise;
  };
  const cross = async (action) => {
    check();
    const result = await track(
      Promise.resolve().then(() => {
        check();
        return action();
      }),
    );
    check();
    return result;
  };
  const interrupt = (code) => {
    if (code === 130 || code === 143) cancelled ??= code;
    fail(new PhaseFailure(code), code);
    try {
      channel.cancel(code);
    } catch (error) {
      fail(error);
    }
  };
  const int = () => interrupt(130);
  const term = () => interrupt(143);
  signals.on('SIGINT', int);
  signals.on('SIGTERM', term);
  const workTimer = setTimer(() => interrupt(124), 75000);
  const waitUntil = async (condition, milliseconds) => {
    const started = now();
    if (!Number.isFinite(started)) throw new PhaseFailure(1);
    let previous = 0;
    const checkElapsed = () => {
      const elapsed = now() - started;
      if (!Number.isFinite(elapsed) || elapsed < previous) throw new PhaseFailure(1);
      previous = elapsed;
      if (elapsed >= milliseconds) throw new PhaseFailure(124);
    };
    for (;;) {
      check();
      checkElapsed();
      if (condition()) {
        checkElapsed();
        return;
      }
      await cross(() => wait(25));
    }
  };
  const increment = (target, name) => {
    if (!Number.isSafeInteger(target[name] ?? 0) || (target[name] ?? 0) >= Number.MAX_SAFE_INTEGER)
      return fail(new PhaseFailure(1));
    target[name] = (target[name] ?? 0) + 1;
  };
  const work = async () => {
    check();
    owner = createOwner({
      browserName: config.browser,
      playwright: playwright ?? require(path.join(__dirname, 'node_modules/playwright')),
      connectTimeoutMs: 5000,
      persist(value) {
        if (!Array.isArray(value) || value.length > 1) throw new PhaseFailure(1);
        recordsValid = value.every((record) => validRecord(record, config.browser));
        records = value.map((record) => project(record, config.browser));
      },
    });
    const browser = await cross(() =>
      owner.launchBrowser(config.browser, { headless: true, timeout: 15000 }),
    );
    const version = browser.version();
    if (
      typeof version !== 'string' ||
      Buffer.byteLength(version) > 256 ||
      Buffer.from(version).some((byte) => byte < 32 || byte === 127)
    )
      throw new PhaseFailure(1);
    observation.browserVersion = version;
    const context = await cross(() =>
      browser.newContext({ permissions: [], viewport: { width: 1280, height: 900 } }),
    );
    context.setDefaultTimeout(10000);
    await cross(() =>
      context.route('**/*', (route) => {
        const operation = Promise.resolve().then(() => {
          if (new URL(route.request().url()).origin !== origin) {
            increment(observation, 'blockedExternalRequests');
            fail(new PhaseFailure(1));
            return route.abort();
          }
          return closing || status !== 0 ? route.abort() : route.continue();
        });
        return track(operation, routes);
      }),
    );
    await cross(() =>
      context.addInitScript(() => {
        globalThis.__restartCaptureCalls = { user: 0, display: 0 };
        const deny = (kind) => () => {
          globalThis.__restartCaptureCalls[kind]++;
          return Promise.reject(
            new DOMException('Capture is disabled by this smoke test', 'NotAllowedError'),
          );
        };
        if (!navigator.mediaDevices) throw new Error('Media capture interception unavailable');
        Object.defineProperty(navigator.mediaDevices, 'getUserMedia', {
          configurable: true,
          value: deny('user'),
        });
        Object.defineProperty(navigator.mediaDevices, 'getDisplayMedia', {
          configurable: true,
          value: deny('display'),
        });
        for (const method of ['getUserMedia', 'webkitGetUserMedia', 'mozGetUserMedia'])
          Object.defineProperty(navigator, method, { configurable: true, value: deny('user') });
      }),
    );
    const room = `restart-browser-${uuid()}`;
    const pages = [];
    for (let index = 0; index < 2; index++) {
      const page = await cross(() => context.newPage());
      const counters = { sockets: 0, navigations: 0, received: {}, sent: {}, malformedFrames: 0 };
      observation.pages.push(counters);
      page.on('pageerror', () => increment(observation, 'pageErrors'));
      page.on('framenavigated', (frame) => {
        if (frame === page.mainFrame()) increment(counters, 'navigations');
      });
      page.on('websocket', (socket) => {
        if (socket.url() !== `ws://127.0.0.1:${config.port}/ws`) {
          increment(observation, 'blockedExternalRequests');
          return;
        }
        increment(counters, 'sockets');
        for (const [event, direction] of [
          ['framereceived', 'received'],
          ['framesent', 'sent'],
        ])
          socket.on(event, ({ payload }) => {
            try {
              const message = JSON.parse(
                typeof payload === 'string' ? payload : payload.toString(),
              );
              const type =
                typeof message.type === 'string' && /^[a-zA-Z]{1,64}$/.test(message.type)
                  ? message.type
                  : 'unknown';
              if (
                !Object.hasOwn(counters[direction], type) &&
                Object.keys(counters[direction]).length >= 64
              )
                return fail(new PhaseFailure(1));
              increment(counters[direction], type);
            } catch {
              increment(counters, 'malformedFrames');
            }
          });
      });
      await cross(() => page.goto(origin, { waitUntil: 'domcontentloaded' }));
      await cross(() => page.locator('#name-input').fill(`Restart guest ${index + 1}`));
      await cross(() => page.locator('#room-input').fill(room));
      await cross(() => page.locator('#join-btn').click());
      await cross(() => page.locator('#room-screen').waitFor({ state: 'visible' }));
      await waitUntil(() => counters.received.roomJoined === 1, 10000);
      pages.push(page);
    }
    observation.phase = 'before_restart';
    const before = `Local restart smoke before ${uuid()}`;
    await cross(() => pages[0].locator('#chat-input').fill(before));
    await cross(() => pages[0].locator('#chat-input').press('Enter'));
    await cross(() =>
      pages[1].locator('#chat-messages').getByText(before, { exact: true }).waitFor(),
    );
    const draft = 'Unsent local restart test draft';
    await cross(() => pages[0].locator('#chat-input').fill(draft));
    const navigationCounts = observation.pages.map((page) => page.navigations);
    observation.navigationCountsBeforeRestart = navigationCounts;
    observation.phase = 'restart';
    const interrupted = now();
    await cross(() => channel.request('stop-first'));
    await waitUntil(
      () => observation.pages.every((page) => page.received.serverRestarting === 1),
      5000,
    );
    await cross(() => channel.request('start-second'));
    await waitUntil(() => observation.pages.every((page) => page.received.roomJoined === 2), 30000);
    observation.rejoinMilliseconds = Math.round(now() - interrupted);
    if (!Number.isSafeInteger(observation.rejoinMilliseconds) || observation.rejoinMilliseconds < 0)
      throw new PhaseFailure(1);
    assert.deepEqual(
      observation.pages.map((page) => page.navigations),
      navigationCounts,
    );
    // Leave navigation and the second server's shutdown keep updating the live
    // counters. Retain the exact recovery assertion boundary independently.
    observation.recoveryPages = structuredClone(observation.pages);
    assert.equal(await cross(() => pages[0].locator('#chat-input').inputValue()), draft);
    observation.draftPreserved = true;
    observation.phase = 'after_restart';
    const after = `Local restart smoke after ${uuid()}`;
    await cross(() => pages[1].locator('#chat-input').fill(after));
    await cross(() => pages[1].locator('#chat-input').press('Enter'));
    await cross(() =>
      pages[0].locator('#chat-messages').getByText(after, { exact: true }).waitFor(),
    );
    for (const page of pages) {
      await cross(() => page.locator('#leave-btn').click());
      await cross(() => page.locator('#join-screen').waitFor({ state: 'visible' }));
      const captures = await cross(() => page.evaluate(() => globalThis.__restartCaptureCalls));
      if (
        captures === null ||
        typeof captures !== 'object' ||
        Object.keys(captures).sort().join(',') !== 'display,user' ||
        !['user', 'display'].every(
          (name) => Number.isSafeInteger(captures[name]) && captures[name] >= 0,
        )
      )
        throw new PhaseFailure(1);
      observation.captureCalls.push({ user: captures.user, display: captures.display });
    }
    assert.deepEqual(observation.captureCalls, [
      { user: 0, display: 0 },
      { user: 0, display: 0 },
    ]);
    assert.ok(
      observation.pages.every((page) => !page.received.roomClosed && !page.malformedFrames),
    );
    await cross(() => channel.request('stop-second'));
    observation.messageDelivery = { before: true, after: true };
    observation.phase = 'complete';
    phasePassed = true;
  };
  const retire = () => {
    retirement ??= (async () => {
      closing = true;
      // work() is awaited, not raced against cancellation. Every admitted
      // launch/connect/locator/request has settled before this snapshot.
      await Promise.allSettled([...pending]);
      if (owner) {
        try {
          await owner.closeAll();
          ownerClosed = true;
        } catch (error) {
          fail(error);
        }
      }
      // Keep the external-request guard through browser close and join routes
      // admitted during earlier continuations too. The trusted Playwright
      // boundary is that a positively closed BrowserServer emits no new route
      // callbacks after its admitted continuations have settled.
      while (routes.size) await Promise.allSettled([...routes]);
      try {
        await channel.finish();
      } catch (error) {
        fail(error);
      }
    })();
    return retirement;
  };
  try {
    await work();
  } catch (error) {
    fail(error);
  } finally {
    clearTimer(workTimer);
    await retire();
    signals.removeListener('SIGINT', int);
    signals.removeListener('SIGTERM', term);
  }
  if (observation.pageErrors || observation.blockedExternalRequests) {
    phasePassed = false;
    fail(new PhaseFailure(1));
  }
  const browserCleanupPassed =
    ownerClosed && recordsValid && records.length === 1 && records.every(clean);
  if (!phasePassed || !browserCleanupPassed) fail(new PhaseFailure(1));
  const receipt = {
    schemaVersion: 1,
    runId: config.runId,
    completed: true,
    phasePassed,
    browserCleanupPassed,
    cancelled,
    browserCount: records.length,
    records,
    observation,
  };
  if (Buffer.byteLength(JSON.stringify(receipt)) > LIMIT) throw new PhaseFailure(1);
  return { status, receipt, cause };
}

async function write(stream, bytes) {
  await new Promise((resolve, reject) => {
    stream.on('error', () => {});
    stream.write(bytes, (error) => (error ? reject(error) : resolve()));
  });
}
async function main(options = {}) {
  const result = await run(options);
  try {
    await write(options.stdout ?? process.stdout, `${JSON.stringify(result.receipt)}\n`);
    if (result.status !== 0)
      await write(options.stderr ?? process.stderr, 'Restart browser phase failed.\n');
  } catch {
    if (result.status === 0) return 1;
  }
  return result.status;
}
module.exports = { run, main, configuration };
if (require.main === module) {
  console.log = (...values) => console.error(...values);
  console.info = (...values) => console.error(...values);
  console.debug = (...values) => console.error(...values);
  if (process.argv.length !== 2) {
    console.error('Restart browser phase arguments are invalid.');
    process.exitCode = 2;
  } else
    main().then(
      (status) => (process.exitCode = status),
      () => {
        console.error('Restart browser phase failed.');
        process.exitCode = 1;
      },
    );
}
