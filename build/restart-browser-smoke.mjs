import assert from 'node:assert/strict';
import { spawn as spawnCommand } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { access, lstat, mkdir, open, realpath, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { basename, dirname, isAbsolute, join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { reserveLocalPorts, serverEnvironment } from './shutdown-smoke.mjs';

const require = createRequire(import.meta.url);
const { closeOwnedBrowser } = require('../web/e2e/lifecycle-cleanup.cjs');
const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const engines = ['chromium', 'firefox', 'webkit'];
const usage = 'Usage: node build/restart-browser-smoke.mjs --binary /absolute/path/to/simplestChat --browser chromium|firefox|webkit --output /absolute/fresh/directory';
const validPath = value => typeof value === 'string' && isAbsolute(value) && !/[\x00-\x1f\x7f]/u.test(value);
const validPort = value => Number.isSafeInteger(value) && value >= 1000 && value <= 65535;

export function parseOptions(args) {
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index], value = args[index + 1];
    if (!['--binary', '--browser', '--output'].includes(key) || typeof value !== 'string' || values.has(key)) throw new Error(usage);
    values.set(key, value);
  }
  const options = { binary: values.get('--binary'), browser: values.get('--browser'), output: values.get('--output') };
  if (!validPath(options.binary) || !validPath(options.output) || !engines.includes(options.browser)) throw new Error(usage);
  return options;
}

/** Two owned local servers, one browser lifetime, no camera/microphone permissions. */
export async function runRestartBrowserSmoke(options, dependencies = {}) {
  const { binary, browser: engineName, output: requestedOutput, signal } = options ?? {};
  parseOptions(['--binary', binary, '--browser', engineName, '--output', requestedOutput]);
  const { spawn = spawnCommand, reservePorts = reserveLocalPorts, fetch = globalThis.fetch,
    validateBinary = file => access(file, constants.X_OK),
    loadPlaywright = () => require(join(repoRoot, 'web/e2e/node_modules/playwright')) } = dependencies;
  await validateBinary(binary);
  const existing = await lstat(requestedOutput).catch(error => {
    if (error.code !== 'ENOENT') throw error;
    return null;
  });
  if (existing) throw new Error('Restart smoke output must be a fresh directory');
  const output = join(await realpath(dirname(requestedOutput)), basename(requestedOutput));
  if (signal?.aborted) throw new Error('Restart browser smoke cancelled');
  await mkdir(output, { mode: 0o700 });
  const started = performance.now();
  const report = { schemaVersion: 1, browser: engineName, startedAt: new Date().toISOString(), passed: false,
    phase: 'setup', pages: [], servers: [], captureCalls: [], pageErrors: 0, blockedExternalRequests: 0 };
  const cancellation = new AbortController();
  const cancel = () => cancellation.abort(new Error('Restart browser smoke cancelled'));
  signal?.addEventListener('abort', cancel, { once: true });
  const workTimer = setTimeout(() => cancellation.abort(new Error('Restart browser work exceeded 75 seconds')), 75_000);
  let browserServer, pendingBrowserLaunch, context, closing = false, failure;
  const children = [];
  const check = () => {
    if (closing || cancellation.signal.aborted) throw cancellation.signal.reason ?? new Error('Restart smoke is finalizing');
    if (report.pageErrors) throw new Error('Browser page raised an uncaught exception');
    if (report.blockedExternalRequests) throw new Error('Browser attempted an external request');
  };
  async function waitUntil(condition, milliseconds, message, duringCleanup = false) {
    const deadline = performance.now() + milliseconds;
    for (;;) {
      if (!duringCleanup) check();
      if (await condition()) return;
      if (performance.now() >= deadline) throw new Error(message);
      await delay(25);
    }
  }
  async function startServer(ports) {
    check();
    const descriptor = await open(join(output, `server-${children.length + 1}.log`), 'wx', 0o600);
    let child, record;
    try {
      check();
      child = spawn(binary, [], { cwd: repoRoot, env: serverEnvironment(ports), stdio: ['ignore', descriptor.fd, descriptor.fd] });
      record = { pid: child.pid ?? null, exit: null, spawnFailed: false, processError: false, signals: [] };
      children.push({ child, record });
      report.servers.push(record);
      // Attach before awaiting file closure: a failed spawn can emit its error
      // on the next tick, before another asynchronous operation settles.
      child.once('error', () => { record.processError = true; record.spawnFailed = !child.pid; });
      child.once('exit', (code, exitSignal) => { record.exit = { code, signal: exitSignal }; });
    } finally { await descriptor.close(); }
    await waitUntil(async () => {
      if (record.processError || record.exit) throw new Error('Owned server failed before readiness');
      try {
        const response = await fetch(`http://127.0.0.1:${ports.http}/ready`, { signal: AbortSignal.timeout(750), redirect: 'error' });
        return response.status === 200 && (await response.json()).status === 'ready';
      } catch { return false; }
    }, 20_000, 'Owned server readiness timed out');
    return children.at(-1);
  }
  async function stopServer(owned, timeout = 20_000, cleanup = false) {
    const { child, record } = owned;
    if (!record.exit && child.pid && !record.spawnFailed && !record.signals.includes('SIGTERM')) {
      record.signals.push('SIGTERM');
      if (!child.kill('SIGTERM')) throw new Error('Could not signal owned server');
    }
    await waitUntil(() => Boolean(record.exit || record.spawnFailed), timeout, 'Owned server shutdown timed out', cleanup);
    assert.deepEqual(record.exit, { code: 0, signal: null }, 'Owned server must exit cleanly with status zero');
  }
  const work = async () => {
    const playwright = await loadPlaywright();
    const ports = await reservePorts();
    if (!ports || !validPort(ports.http) || !validPort(ports.media)) throw new Error('Invalid allocated local ports');
    report.ports = ports;
    const origin = `http://127.0.0.1:${ports.http}`;
    const first = await startServer(ports);
    check();
    pendingBrowserLaunch = playwright[engineName].launchServer({ headless: true, timeout: 15_000 });
    browserServer = await pendingBrowserLaunch;
    check();
    const browser = await playwright[engineName].connect(browserServer.wsEndpoint(), { timeout: 5000 });
    report.browserVersion = browser.version();
    context = await browser.newContext({ permissions: [], viewport: { width: 1280, height: 900 } });
    context.setDefaultTimeout(10_000);
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === origin) await route.continue();
      else { report.blockedExternalRequests++; await route.abort(); }
    });
    await context.addInitScript(() => {
      globalThis.__restartCaptureCalls = { user: 0, display: 0 };
      const deny = kind => () => {
        globalThis.__restartCaptureCalls[kind]++;
        return Promise.reject(new DOMException('Capture is disabled by this smoke test', 'NotAllowedError'));
      };
      if (!navigator.mediaDevices) throw new Error('Media capture interception unavailable');
      Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { configurable: true, value: deny('user') });
      Object.defineProperty(navigator.mediaDevices, 'getDisplayMedia', { configurable: true, value: deny('display') });
      for (const method of ['getUserMedia', 'webkitGetUserMedia', 'mozGetUserMedia']) {
        Object.defineProperty(navigator, method, { configurable: true, value: deny('user') });
      }
    });
    const room = `restart-browser-${randomUUID()}`;
    const pages = [];
    for (let index = 0; index < 2; index++) {
      check();
      const page = await context.newPage();
      const counters = { sockets: 0, navigations: 0, received: {}, sent: {}, malformedFrames: 0 };
      report.pages.push(counters);
      page.on('pageerror', () => { report.pageErrors++; });
      page.on('framenavigated', frame => { if (frame === page.mainFrame()) counters.navigations++; });
      page.on('websocket', socket => {
        if (socket.url() !== `ws://127.0.0.1:${ports.http}/ws`) { report.blockedExternalRequests++; return; }
        counters.sockets++;
        for (const [event, direction] of [['framereceived', 'received'], ['framesent', 'sent']]) {
          socket.on(event, ({ payload }) => {
            try {
              const message = JSON.parse(typeof payload === 'string' ? payload : payload.toString());
              const type = typeof message.type === 'string' && /^[a-zA-Z]{1,64}$/u.test(message.type) ? message.type : 'unknown';
              counters[direction][type] = (counters[direction][type] ?? 0) + 1;
            } catch { counters.malformedFrames++; }
          });
        }
      });
      await page.goto(origin, { waitUntil: 'domcontentloaded' });
      await page.locator('#name-input').fill(`Restart guest ${index + 1}`);
      await page.locator('#room-input').fill(room);
      await page.locator('#join-btn').click();
      await page.locator('#room-screen').waitFor({ state: 'visible' });
      await waitUntil(() => counters.received.roomJoined === 1, 10_000, 'Initial guest join was not observed');
      pages.push(page);
    }
    report.phase = 'before_restart';
    const before = `Local restart smoke before ${randomUUID()}`;
    await pages[0].locator('#chat-input').fill(before);
    await pages[0].locator('#chat-send-btn').click();
    await pages[1].locator('#chat-messages').getByText(before, { exact: true }).waitFor();
    const draft = 'Unsent local restart test draft';
    await pages[0].locator('#chat-input').fill(draft);
    const navigationCounts = report.pages.map(page => page.navigations);
    report.phase = 'restart';
    const interrupted = performance.now();
    await stopServer(first);
    await waitUntil(() => report.pages.every(page => page.received.serverRestarting === 1), 5000, 'Both pages must receive serverRestarting');
    const second = await startServer(ports);
    await waitUntil(() => report.pages.every(page => page.received.roomJoined === 2), 30_000, 'Both pages must rejoin automatically without reloading');
    report.rejoinMilliseconds = Math.round(performance.now() - interrupted);
    assert.deepEqual(report.pages.map(page => page.navigations), navigationCounts, 'Recovery must not navigate or reload either page');
    assert.equal(await pages[0].locator('#chat-input').inputValue(), draft, 'Unsent public draft must survive server restart');
    report.draftPreserved = true;
    report.phase = 'after_restart';
    const after = `Local restart smoke after ${randomUUID()}`;
    await pages[1].locator('#chat-input').fill(after);
    await pages[1].locator('#chat-send-btn').click();
    await pages[0].locator('#chat-messages').getByText(after, { exact: true }).waitFor();
    for (const page of pages) {
      await page.locator('#leave-btn').click();
      await page.locator('#join-screen').waitFor({ state: 'visible' });
      report.captureCalls.push(await page.evaluate(() => globalThis.__restartCaptureCalls));
    }
    assert.deepEqual(report.captureCalls, [{ user: 0, display: 0 }, { user: 0, display: 0 }], 'This smoke must never request capture');
    assert.ok(report.pages.every(page => !page.received.roomClosed && !page.malformedFrames), 'Restart must not close rooms or send invalid frames');
    await stopServer(second);
    check();
    report.messageDelivery = { before: true, after: true };
    report.phase = 'complete';
  };
  let rejectCancellation;
  const cancelled = new Promise((_, reject) => {
    rejectCancellation = () => reject(cancellation.signal.reason);
    cancellation.signal.addEventListener('abort', rejectCancellation, { once: true });
  });
  try { await Promise.race([work(), cancelled]); }
  catch (error) { failure = error; }
  finally {
    closing = true;
    clearTimeout(workTimer);
    signal?.removeEventListener('abort', cancel);
    cancellation.signal.removeEventListener('abort', rejectCancellation);
    const cleanupErrors = [];
    // Cancellation can win while Playwright is acquiring its BrowserServer.
    // Adopt that still-owned launch before cleanup; otherwise a late resolution
    // could leave a browser running after this function returned. launchServer
    // has its own 15s deadline. Reserve that plus 15s browser and 6s native
    // cleanup inside the remaining 45s of this 120s overall attempt budget.
    if (pendingBrowserLaunch) {
      let launchTimer, settlementTimedOut = false;
      try {
        browserServer = await Promise.race([
          pendingBrowserLaunch,
          new Promise((_, reject) => {
            launchTimer = setTimeout(() => {
              settlementTimedOut = true;
              reject(new Error('Owned browser launch settlement timed out'));
            }, 15_000);
          }),
        ]);
      } catch {
        if (settlementTimedOut) {
          cleanupErrors.push('Owned browser launch cleanup unconfirmed');
          // If a driver violates its declared launch deadline, never abandon
          // a later resource. Its eventual handle is killed, but this attempt
          // remains failed with unconfirmed cleanup in its immutable report.
          pendingBrowserLaunch.then(server => server.kill()).catch(() => {});
        } else report.browserLaunchRejected = true;
      } finally { clearTimeout(launchTimer); }
    }
    if (browserServer) {
      try { await closeOwnedBrowser(browserServer, report, async () => {}); }
      catch { cleanupErrors.push('Owned browser cleanup failed'); }
    }
    for (const owned of children) {
      if (!owned.record.exit && !owned.record.spawnFailed) {
        try { await stopServer(owned, 5000, true); }
        catch {
          cleanupErrors.push('Owned server required forced cleanup');
          owned.record.signals.push('SIGKILL');
          owned.child.kill('SIGKILL');
          try { await waitUntil(() => Boolean(owned.record.exit), 1000, 'Owned server cleanup unconfirmed', true); }
          catch { cleanupErrors.push('Owned server cleanup unconfirmed'); }
        }
      }
    }
    report.cleanupErrors = cleanupErrors;
    report.elapsedMilliseconds = Math.round(performance.now() - started);
    report.finishedAt = new Date().toISOString();
    report.passed = !failure && !cleanupErrors.length && report.phase === 'complete' &&
      report.servers.length === 2 && report.servers.every(server => server.exit?.code === 0 && server.exit.signal === null) &&
      report.browserCleanup?.passed === true && report.elapsedMilliseconds < 120_000;
    if (failure) report.failure = { name: failure.name ?? 'Error', message: String(failure.message ?? 'Restart smoke failed').slice(0, 1000) };
    await writeFile(join(output, 'outcome.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  }
  if (!report.passed) {
    const error = new Error(`Restart browser smoke failed; inspect ${output}`, { cause: failure });
    error.report = report;
    throw error;
  }
  return report;
}

export async function runCli({ args = process.argv.slice(2), stdout = process.stdout, stderr = process.stderr, run = runRestartBrowserSmoke } = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) { stdout.write(`${usage}\n`); return 0; }
  let options;
  try { options = parseOptions(args); }
  catch { stderr.write(`${usage}\n`); return 2; }
  const cancellation = new AbortController();
  const cancel = () => cancellation.abort();
  process.once('SIGINT', cancel);
  process.once('SIGTERM', cancel);
  try {
    const report = await run({ ...options, signal: cancellation.signal });
    stdout.write(`PASS ${report.browser} restart: automatic rejoin ${report.rejoinMilliseconds}ms, draft and chat preserved; ${options.output}\n`);
    return 0;
  } catch (error) { stderr.write(`${error.message}\n`); return 1; }
  finally { process.removeListener('SIGINT', cancel); process.removeListener('SIGTERM', cancel); }
}

const entryFile = process.argv[1] ? await realpath(process.argv[1]).catch(() => undefined) : undefined;
if (entryFile && import.meta.url === pathToFileURL(entryFile).href) process.exitCode = await runCli();
