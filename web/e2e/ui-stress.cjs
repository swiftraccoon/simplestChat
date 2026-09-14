/**
 * Text-only, real-server UI stress. Run through build/with-test-server.sh with
 * UI_STRESS_E2E=1 and a disposable database. UI_STRESS_PROFILE=smoke validates the
 * harness; only the default full profile rolls over the 300-message history.
 * No production origins, rate-limit overrides, fake replies, or capture devices.
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { setTimeout: delay } = require('node:timers/promises');
const {
  configuration,
  distribution,
  cpuDelta,
  retainedMessages,
  serverCounters,
  serverCounterDelta,
  cleanServerDelta,
  workloadMessage,
  installUiStressObservation,
} = require('./ui-stress-metrics.cjs');
const { serverCounts, handshakeGate } = require('./lifecycle-checks.cjs');
const { closeOwnedBrowser } = require('./lifecycle-cleanup.cjs');
const {
  initializePerformanceReport,
  persistPerformanceReport,
} = require('./performance-report.cjs');

class StressFailure extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}
const check = (condition, code) => {
  if (!condition) throw new StressFailure(code);
};
const identity = (value) => typeof value === 'string' && /^[a-zA-Z0-9_-]{1,128}$/.test(value);

/** Playwright evaluate() has no action timeout; do not let a wedged page block release. */
async function cleanupWithin(action, milliseconds = 2000) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(action),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new StressFailure('cleanup_action_timeout')), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** One outstanding request per sender; no frame history or credentials retained. */
class Guest {
  constructor(url) {
    this.socket = new WebSocket(url);
    this.waiters = new Set();
    this.failure = null;
    this.joined = false;
    this.left = false;
    this.closing = false;
    this.cleanClose = false;
    this.received = new Set();
    this.closed = new Promise((resolve) => {
      this.socket.addEventListener('close', (event) => {
        this.cleanClose = this.closing && event.wasClean && event.code === 1000;
        if (!this.closing) this.fail('unexpected_guest_close');
        resolve();
      });
    });
    this.socket.addEventListener('error', () => this.fail('guest_socket_error'));
    this.socket.addEventListener('message', (event) => {
      if (typeof event.data !== 'string' || Buffer.byteLength(event.data) > 262144) {
        this.fail('guest_frame_limit');
        return;
      }
      let message;
      try {
        message = JSON.parse(event.data);
      } catch {
        this.fail('guest_invalid_json');
        return;
      }
      if (!message || typeof message.type !== 'string') {
        this.fail('guest_invalid_message');
        return;
      }
      if (
        ['error', 'socialError', 'roomClosed', 'roomPasswordRequired', 'lobbyWaiting'].includes(
          message.type,
        )
      ) {
        this.fail('guest_request_rejected');
        return;
      }
      if (message.type === 'chatReceived' || message.type === 'messageAck') {
        const entry = message.type === 'messageAck' ? message.message : message;
        const id = entry?.messageId;
        if (!identity(id) || this.received.size >= 2048) {
          this.fail('guest_message_limit');
          return;
        }
        if (!this.workload || !workloadMessage(entry, this.workload)) {
          this.fail('guest_payload_or_sender_mismatch');
          return;
        }
        if (this.received.has(id)) {
          this.fail('guest_duplicate_message');
          return;
        }
        this.received.add(id);
      }
      for (const waiter of this.waiters) {
        if (waiter.match(message)) {
          this.waiters.delete(waiter);
          waiter.resolve(message);
        }
      }
    });
  }

  fail(code) {
    this.failure ??= new StressFailure(code);
    for (const waiter of this.waiters) waiter.reject(this.failure);
    this.waiters.clear();
  }

  async opened() {
    if (this.socket.readyState === WebSocket.OPEN) return;
    await new Promise((resolve, reject) => {
      const done = () => {
        clearTimeout(timer);
        this.socket.removeEventListener('open', opened);
      };
      const opened = () => {
        done();
        resolve();
      };
      const timer = setTimeout(() => {
        done();
        reject(new StressFailure('guest_open_timeout'));
      }, 10000);
      this.socket.addEventListener('open', opened, { once: true });
    });
    if (this.failure) throw this.failure;
  }

  async request(message, match) {
    if (this.failure) throw this.failure;
    check(this.socket.readyState === WebSocket.OPEN, 'guest_not_open');
    let waiter;
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        waiter = { match, resolve, reject };
        this.waiters.add(waiter);
        timer = setTimeout(() => reject(new StressFailure('guest_response_timeout')), 10000);
        this.socket.send(JSON.stringify(message));
      });
    } finally {
      clearTimeout(timer);
      this.waiters.delete(waiter);
    }
  }

  async join(roomId, participantName) {
    await this.opened();
    const result = await this.request(
      { type: 'joinRoom', roomId, participantName },
      (message) => message.type === 'roomJoined',
    );
    check(identity(result.participantId), 'guest_join_identity');
    this.participantId = result.participantId;
    this.joined = true;
  }

  async chat(content, clientMessageId) {
    const started = performance.now();
    const ack = await this.request(
      { type: 'chatMessage', content, clientMessageId },
      (message) => message.type === 'messageAck' && message.clientMessageId === clientMessageId,
    );
    check(
      ack.message?.participantId === this.participantId &&
        ack.message.clientMessageId === clientMessageId &&
        ack.message.content === content &&
        identity(ack.message.messageId),
      'incorrect_message_ack',
    );
    return { id: ack.message.messageId, content, ackMs: performance.now() - started };
  }

  close() {
    if (this.closing) return;
    this.closing = true;
    if (this.joined && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify({ type: 'leaveRoom' }));
      this.left = true;
    }
    this.socket.close(1000, 'Owned UI stress complete');
  }
}

async function run(env = process.env) {
  const config = configuration(env);
  process.kill(config.serverPid, 0); // Read-only liveness check of the helper-supplied PID.
  const artifacts =
    env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-ui-stress.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const filename = path.join(artifacts, 'ui-stress-results.json');
  const hash = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const workdir = env.TEST_SERVER_WORKDIR || process.cwd();
  const playwrightModule = env.PLAYWRIGHT_MODULE || 'playwright';
  const { chromium } = require(playwrightModule);
  const runId = `ui-stress-${crypto.randomUUID()}`;
  const report = {
    schemaVersion: 1,
    runId,
    startedAt: new Date().toISOString(),
    completed: false,
    passed: false,
    workload: { ...config, base: config.base, serverPid: config.serverPid },
    authentication: {
      browserAccounts: 2,
      protocolGuests: config.guests,
      browserRegistration: false,
      scope:
        'Two separately registered password accounts; remaining clients are unauthenticated guests. No passkey or refresh-expiry coverage.',
    },
    provenance: {
      serverRevision: env.SERVER_REVISION || 'not supplied',
      frontendRevision: env.FRONTEND_REVISION || 'not supplied',
      serverBinarySha256: env.TEST_SERVER_BINARY ? hash(env.TEST_SERVER_BINARY) : null,
      cargoLockSha256: hash(path.join(workdir, 'Cargo.lock')),
      webEntrySha256: hash(path.join(workdir, 'web/dist/index.html')),
      harnessSha256: hash(__filename),
      metricsHelperSha256: hash(path.join(__dirname, 'ui-stress-metrics.cjs')),
      node: process.version,
      platform: process.platform,
      osRelease: os.release(),
      arch: process.arch,
      cpu: os.cpus()[0]?.model,
      logicalCpus: os.cpus().length,
      playwright: require(
        path.join(path.dirname(require.resolve(playwrightModule)), 'package.json'),
      ).version,
    },
    limitations: [
      'Bounded loopback Chromium workload, not platform capacity, physical/mobile browser coverage, or an indefinite soak.',
      'Mobile is a 375px Chromium viewport. No camera, microphone, screen capture, fake media, or permission grants.',
      'Real guest joins are spaced 6.3s; sends are paced with no catch-up bursts. Production admission and chat limits remain unchanged.',
      'One owned replacement WebSocket handshake is held briefly during traffic; upstream signaling stays real. This is not a UDP outage or server restart.',
      'Arrival-to-DOM/rAF includes application work and scheduling. Snapshot-to-post-dispatch DOM includes timer queue delay; neither is pure rendering CPU.',
      'A requestAnimationFrame callback is an animation-frame boundary, not proof of pixels being painted. Final DOM identities and content are checked separately.',
      'Trusted input-to-rAF and controller action duration are separate informational measurements, not INP or hard regression thresholds.',
      'CDP task/script/layout deltas and JS heap/DOM counters are not whole-browser CPU/RSS. No forced GC or hard memory-leak claim.',
      'Instrumentation scans retained chat rows on mutation, retains bounded message identities, and uses weak native-resource references; compare identical harnesses.',
      'Final zero server gauges cover memberships/connections/active rooms, not every native allocation. The outer helper records server shutdown separately.',
      'Disposable accounts are left for database-helper cleanup; only the uniquely owned room is explicitly deleted.',
    ],
    phases: [],
    interactions: [],
    checks: {},
    pageErrorCount: 0,
    unexpectedOriginCount: 0,
  };
  initializePerformanceReport(filename, report);
  const save = () => persistPerformanceReport(filename, report);
  const clients = [];
  const guests = [];
  const messages = [];
  const abort = new AbortController();
  let browserServer;
  let browser;
  let stage = 'startup';
  let roomCreated = false;
  let roomDeleted = false;
  let workloadPassed = false;
  let trafficStarted;
  let closePromise;
  const started = performance.now();
  const closeBrowser = () =>
    (closePromise ??= browserServer
      ? closeOwnedBrowser(browserServer, report, save)
      : Promise.resolve());
  const healthy = () => {
    check(!abort.signal.aborted, 'workload_deadline');
    check(report.pageErrorCount === 0, 'browser_page_error');
    check(report.unexpectedOriginCount === 0, 'unexpected_browser_origin');
    for (const guest of guests) if (guest.failure) throw guest.failure;
    for (const client of clients)
      check(client.gate.snapshot().failure === null, 'reconnect_gate_failed');
  };
  const wait = async (milliseconds) => {
    healthy();
    await delay(Math.max(0, milliseconds), undefined, { signal: abort.signal });
    healthy();
  };
  async function poll(code, work, accept, milliseconds = 15000, cleanup = false) {
    const until = performance.now() + milliseconds;
    do {
      if (!cleanup) healthy();
      const value = await work();
      if (accept(value)) return value;
      await delay(100);
    } while (performance.now() < until);
    throw new StressFailure(code);
  }
  async function counts() {
    const response = await fetch(`${config.base}/metrics`, {
      redirect: 'error',
      signal: AbortSignal.timeout(5000),
      headers: { Authorization: `Bearer ${env.TEST_METRICS_TOKEN}` },
    });
    check(response.status === 200 && response.body, 'metrics_unavailable');
    const reader = response.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        check(size <= 131072, 'metrics_response_limit');
        chunks.push(value);
      }
    } finally {
      await reader.cancel().catch(() => {});
    }
    const body = Buffer.concat(chunks, size).toString('utf8');
    return { ...serverCounts(body), counters: serverCounters(body) };
  }
  const state = (client) => client.page.evaluate(() => window.__uiStress.status());
  function validState(value) {
    check(value.failure === null, 'browser_observation_failed');
    check(value.captureRequests === 0, 'unexpected_capture_request');
    check(value.protocolErrors === 0 && value.reconnectFailure === 0, 'browser_protocol_error');
    check(value.liveDuplicates === 0, 'duplicate_live_message');
    check(value.unexpectedSocketCloses === 0, 'unexpected_browser_disconnect');
  }
  async function sample(label, closedQueueAllowance = 0) {
    healthy();
    const measured = [];
    const rss = execFileSync('ps', ['-o', 'rss=', '-p', String(config.serverPid)], {
      encoding: 'utf8',
      timeout: 2000,
      maxBuffer: 4096,
    }).trim();
    check(/^[0-9]+$/.test(rss), 'server_rss_unavailable');
    for (const client of clients) {
      const observation = await client.page.evaluate(() => window.__uiStress.sample());
      validState(observation);
      const current = Object.fromEntries(
        (await client.cdp.send('Performance.getMetrics')).metrics.map((metric) => [
          metric.name,
          metric.value,
        ]),
      );
      const dom = await client.cdp.send('Memory.getDOMCounters');
      check(
        Number.isFinite(current.JSHeapUsedSize) && current.JSHeapUsedSize > 0,
        'heap_metric_unavailable',
      );
      measured.push({
        client: client.label,
        viewport: client.viewport,
        jsHeapUsedBytes: current.JSHeapUsedSize,
        ...dom,
        cpuDelta: client.previous ? cpuDelta(client.previous, current) : null,
        observation: {
          seenMessages: observation.seenIds.length,
          renderedMessages: observation.renderedIds.length,
          framedMessages: observation.framedIds.length,
          pendingMessages: observation.pendingMessages,
          pendingFrames: observation.pendingFrames,
          snapshotCount: observation.snapshotCount,
          replayRecoveredMessages: observation.replayRecoveredMessages,
          openPeers: observation.openPeers,
          openSockets: observation.openSockets,
          captureRequests: observation.captureRequests,
          unexpectedSocketCloses: observation.unexpectedSocketCloses,
          protocolErrors: observation.protocolErrors,
          payloadValidatedMessages: observation.payloadValidatedMessages,
        },
        timings: Object.fromEntries(
          Object.entries(observation.timings).map(([key, values]) => [key, distribution(values)]),
        ),
      });
      client.previous = current;
    }
    const server = await counts();
    const previous = report.phases.at(-1)?.server ?? report.initialServer;
    const delta = serverCounterDelta(previous.counters, server.counters);
    // Preserve departure races separately from the clean traffic phases.
    const gated = label !== 'post_leave_quiescent';
    let gateFailed = false;
    if (gated) {
      try {
        cleanServerDelta(delta, closedQueueAllowance);
      } catch {
        gateFailed = true;
      }
    }
    report.phases.push({
      phase: label,
      elapsedMs: performance.now() - started,
      serverRssBytes: Number(rss) * 1024,
      server,
      serverCounterDelta: delta,
      serverCounterGate: gated
        ? { passed: !gateFailed, closedQueueAllowance }
        : { informationalTeardown: true },
      clients: measured,
    });
    save();
    check(!gateFailed, 'server_counter_gate_failed');
  }
  async function connected(client) {
    await client.page.locator('#room-screen').waitFor({ state: 'visible' });
    await client.page.waitForFunction(
      () =>
        document.querySelector('#connection-status')?.textContent === 'Connected' &&
        !document.querySelector('#chat-input')?.disabled,
    );
    validState(await state(client));
  }
  async function roster(client, expected) {
    await client.page.waitForFunction(
      (count) =>
        document.querySelectorAll('#participant-list > li[data-participant-id]').length === count,
      expected,
    );
    const lists = await client.page.evaluate(() =>
      ['#participant-list', '#classic-users-panel .classic-user-list'].map((selector) =>
        [...document.querySelectorAll(`${selector} > li[data-participant-id]`)].map(
          (element) => element.dataset.participantId,
        ),
      ),
    );
    for (const ids of lists)
      check(
        ids.length === expected && new Set(ids).size === expected,
        'incorrect_or_duplicate_roster',
      );
    assert.deepEqual([...lists[0]].sort(), [...lists[1]].sort(), 'Roster identities differ');
  }
  async function makeClient(label, viewport) {
    const context = await browser.newContext({ viewport, permissions: [] });
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    const client = {
      label,
      viewport,
      context,
      page,
      gate: handshakeGate(15000),
      navigationCount: 0,
    };
    clients.push(client);
    await context.route('**/*', (route) => {
      if (new URL(route.request().url()).origin === config.base) return route.continue();
      report.unexpectedOriginCount++;
      return route.abort();
    });
    await page.routeWebSocket(`${config.base.replace(/^http/, 'ws')}/ws`, (route) =>
      client.gate.handle(route),
    );
    await page.addInitScript(installUiStressObservation);
    page.on('pageerror', () => {
      report.pageErrorCount++;
    });
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) client.navigationCount++;
    });
    client.cdp = await context.newCDPSession(page);
    await client.cdp.send('Performance.enable');
    await page.goto(config.base, { waitUntil: 'networkidle' });
    await page.locator('.conversation-toolbar').waitFor({ state: 'attached' });
    return client;
  }
  async function register(client) {
    const { page, label } = client;
    const password = `Disposable-${crypto.randomBytes(24).toString('hex')}!`;
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-to-register').click();
    await page.locator('#register-email').fill(`${runId}-${label}@example.test`);
    await page.locator('#register-name').fill(`UI ${label}`);
    await page.locator('#register-password').fill(password);
    await page.locator('#register-confirm').fill(password);
    await page.locator('#register-submit').click();
    await page.locator('#register-modal').waitFor({ state: 'hidden' });
    await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
    await poll(
      'authenticated_socket_unavailable',
      () => state(client),
      (value) => value.openSockets === 1,
    );
    await page.evaluate(() => window.__uiStress.guardConnections());
  }
  async function deleteOwnedRoom() {
    if (!roomCreated || roomDeleted) return;
    const page = clients[0].page;
    await page
      .locator('#community-actions')
      .getByRole('button', { name: 'My rooms', exact: true })
      .click();
    const mine = page.getByRole('dialog', { name: 'My rooms', exact: true });
    await mine.getByRole('button', { name: 'Delete room…', exact: true }).click();
    const remove = page.getByRole('dialog', { name: 'Delete room', exact: true });
    await remove.getByLabel(`Type ${runId} to confirm`, { exact: true }).fill(runId);
    await remove.getByRole('button', { name: 'Permanently delete room', exact: true }).click();
    await remove.waitFor({ state: 'hidden' });
    await mine
      .getByText('No owned rooms yet. Use Create Room on the join screen.', { exact: true })
      .waitFor();
    roomDeleted = true;
    await mine.getByRole('button', { name: 'Close', exact: true }).click();
  }
  async function interactions(client, label) {
    const page = client.page;
    const at = performance.now();
    const input = page.locator('#chat-input');
    const draft = `Draft ${client.label} remains intact`;
    await input.fill('');
    await input.pressSequentially(draft, { delay: 20 });
    check((await input.inputValue()) === draft, 'typed_draft_lost');
    await page.locator('#settings-btn').focus();
    await page.locator('#settings-btn').press('Enter');
    const dialog = page.getByRole('dialog', { name: 'Your settings', exact: true });
    await dialog.waitFor({ state: 'visible' });
    const bounds = await dialog.boundingBox();
    check(
      bounds && bounds.x >= -1 && bounds.x + bounds.width <= client.viewport.width + 1,
      'settings_overflow',
    );
    await dialog.getByRole('tab', { name: 'Appearance', exact: true }).click();
    await page.keyboard.press('Escape');
    await dialog.waitFor({ state: 'hidden' });
    check(
      await page.locator('#settings-btn').evaluate((element) => document.activeElement === element),
      'settings_focus_not_restored',
    );
    check((await input.inputValue()) === draft, 'settings_lost_draft');
    check(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
      'page_overflow',
    );
    const scroll = await page.locator('#chat-messages').evaluate((element) => {
      element.scrollTop = 0;
      element.dispatchEvent(new Event('scroll'));
      return { top: element.scrollTop, overflow: element.scrollHeight > element.clientHeight + 50 };
    });
    check(scroll.overflow, 'chat_scrollback_not_exercised');
    const accepted = messages.length;
    await poll(
      'no_traffic_during_scrollback',
      async () => messages.length,
      (count) => count >= accepted + 2,
    );
    check(
      (await page.locator('#chat-messages').evaluate((element) => element.scrollTop)) <=
        scroll.top + 2,
      'scrollback_jumped_to_bottom',
    );
    await page.locator('#scroll-bottom-btn').click();
    await page.waitForFunction(() => {
      const element = document.querySelector('#chat-messages');
      return element.scrollHeight - element.clientHeight - element.scrollTop < 5;
    });
    validState(await state(client));
    report.interactions.push({
      client: client.label,
      phase: label,
      controllerActionMs: performance.now() - at,
      passed: true,
    });
    save();
  }

  let finalWatchdog;
  const deadline = setTimeout(() => {
    report.failure ??= { stage, code: 'workload_deadline' };
    report.passed = false;
    abort.abort();
    for (const guest of guests) {
      try {
        guest.close();
      } catch {}
    }
    for (const client of clients) client.gate.dispose();
    void closeBrowser().catch(() => {});
    finalWatchdog ??= setTimeout(() => {
      try {
        save();
      } catch {}
      void browserServer?.kill().catch(() => {});
      process.exit(1);
    }, 30000);
  }, config.deadlineMs);
  try {
    const baseline = await counts();
    check(
      baseline.rooms === 0 && baseline.participants === 0 && baseline.connections === 0,
      'test_server_not_empty',
    );
    report.initialServer = baseline;
    browserServer = await chromium.launchServer({
      headless: true,
      args: ['--disable-background-timer-throttling', '--disable-renderer-backgrounding'],
    });
    browser = await chromium.connect(browserServer.wsEndpoint());
    report.provenance.browser = browser.version();
    const owner = await makeClient('owner', { width: 1440, height: 1000 });
    const member = await makeClient('member', { width: 375, height: 812 });
    stage = 'register_accounts';
    await register(owner);
    await register(member);
    report.authentication.browserRegistration = true;
    await sample('registered_startup');
    stage = 'create_and_join_room';
    await owner.page.locator('#create-room-btn').click();
    await owner.page.locator('#cr-id').fill(runId);
    await owner.page.locator('#cr-name').fill('Owned UI stress room');
    await owner.page.locator('#create-room-submit').click();
    await connected(owner);
    roomCreated = true;
    let lastJoin = performance.now();
    await wait(config.joinIntervalMs);
    await member.page.locator('#name-input').fill('UI member');
    await member.page.locator('#room-input').fill(runId);
    await member.page.locator('#join-btn').click();
    await connected(member);
    lastJoin = performance.now();
    stage = 'guest_ramp';
    for (let index = 0; index < config.guests; index++) {
      await wait(config.joinIntervalMs - (performance.now() - lastJoin));
      const guest = new Guest(`${config.base.replace(/^http/, 'ws')}/ws`);
      guests.push(guest);
      await guest.join(runId, `UI guest ${String(index + 1).padStart(2, '0')}`);
      lastJoin = performance.now();
    }
    const expectedWorkload = {
      runId,
      messages: config.messages,
      senders: guests.slice(0, Math.min(8, guests.length)).map((guest) => guest.participantId),
    };
    for (const guest of guests) guest.workload = expectedWorkload;
    for (const client of clients) {
      await client.page.evaluate(
        (workload) => window.__uiStress.expectWorkload(workload),
        expectedWorkload,
      );
      await connected(client);
      await roster(client, config.guests + 2);
    }
    await poll(
      'cohort_server_counts',
      counts,
      (value) =>
        value.participants === config.guests + 2 && value.connections === config.guests + 2,
    );
    report.checks.cohort = true;
    await sample('full_roster');
    stage = 'chat_traffic';
    trafficStarted = performance.now();
    report.traffic = {
      acceptedMessages: 0,
      targetMessages: config.messages,
      targetIntervalMs: config.messageIntervalMs,
      completed: false,
    };
    let lastSend = 0;
    const tasks = [
      (async () => {
        for (let index = 0; index < config.messages; index++) {
          await wait(config.messageIntervalMs - (performance.now() - lastSend));
          lastSend = performance.now();
          const guest = guests[index % Math.min(8, guests.length)];
          messages.push(
            await guest.chat(
              `UI stress message ${String(index + 1).padStart(4, '0')}`,
              `${runId}-${index + 1}`,
            ),
          );
          report.traffic.acceptedMessages = messages.length;
          if (messages.length % 40 === 0) save();
        }
        report.traffic = {
          acceptedMessages: messages.length,
          completed: true,
          targetIntervalMs: config.messageIntervalMs,
          elapsedMs: performance.now() - trafficStarted,
          acknowledgement: distribution(messages.map((message) => message.ackMs)),
        };
      })(),
      (async () => {
        await poll(
          'interaction_traffic_wait',
          async () => messages.length,
          (count) => count >= (config.profile === 'full' ? 80 : 12),
          30000,
        );
        for (const client of clients) await interactions(client, 'during_traffic');
        if (config.profile === 'full') {
          await poll(
            'retained_history_interaction_wait',
            async () => messages.length,
            (count) => count >= 800,
            150000,
          );
          for (const client of clients) await interactions(client, 'full_retained_history');
        }
      })(),
      (async () => {
        await poll(
          'reconnect_traffic_wait',
          async () => messages.length,
          (count) => count >= config.reconnectAfterMessages,
          150000,
        );
        await sample('before_reconnect');
        const beforeCloseMessages = messages.length;
        const before = await state(owner);
        const draft = await owner.page.locator('#chat-input').inputValue();
        const navigations = owner.navigationCount;
        owner.gate.arm();
        await owner.page.evaluate(() => window.__uiStress.closeCurrent());
        await poll(
          'replacement_handshake_not_held',
          async () => owner.gate.snapshot(),
          (value) => value.held,
        );
        const heldAtMessages = messages.length;
        await poll(
          'no_traffic_during_reconnect',
          async () => messages.length,
          (count) => count >= heldAtMessages + config.heldMessages,
        );
        owner.gate.release();
        const after = await poll(
          'reconnect_replay_incomplete',
          () => state(owner),
          (value) =>
            value.reconnectSuccess === before.reconnectSuccess + 1 &&
            value.snapshotCount > before.snapshotCount &&
            value.replayRecoveredMessages > before.replayRecoveredMessages &&
            value.pendingReplays === 0,
          30000,
        );
        validState(after);
        check(
          after.joined === before.joined && owner.navigationCount === navigations,
          'reconnect_rejoined_or_navigated',
        );
        await connected(owner);
        await roster(owner, config.guests + 2);
        check(
          (await owner.page.locator('#chat-input').inputValue()) === draft,
          'reconnect_lost_draft',
        );
        report.reconnect = {
          heldTrafficMessages: messages.length - heldAtMessages,
          replayRecoveredMessages: after.replayRecoveredMessages - before.replayRecoveredMessages,
          successfulGraceReconnect: true,
          fullRejoin: false,
          passed: true,
        };
        // One retained grace membership can receive one closed-queue broadcast
        // per accepted chat while its writer is absent. +2 covers an in-flight
        // sender ACK at each metrics boundary, not arbitrary server failures.
        await sample('after_reconnect', messages.length - beforeCloseMessages + 2);
      })(),
    ];
    let workloadError;
    await Promise.allSettled(
      tasks.map((task) =>
        task.catch((error) => {
          workloadError ??= error;
          abort.abort();
          throw error;
        }),
      ),
    );
    if (workloadError) throw workloadError;
    healthy();
    check(
      messages.length === config.messages &&
        new Set(messages.map((message) => message.id)).size === config.messages,
      'accepted_message_count',
    );
    stage = 'delivery_and_retention';
    for (const client of clients) {
      const observation = await poll(
        'browser_message_delivery_incomplete',
        () => state(client),
        (value) =>
          value.framedIds.length === config.messages &&
          value.pendingMessages === 0 &&
          value.pendingFrames === 0 &&
          value.pendingReplays === 0,
        30000,
      );
      validState(observation);
      check(
        observation.payloadValidatedMessages === config.messages,
        'browser_payload_validation_incomplete',
      );
      const expectedIds = new Set(messages.map((message) => message.id));
      check(
        observation.seenIds.length === expectedIds.size &&
          observation.seenIds.every((id) => expectedIds.has(id)),
        'browser_message_identity_mismatch',
      );
      check(
        observation.framedIds.every((id) => expectedIds.has(id)),
        'browser_frame_identity_mismatch',
      );
      const actual = await client.page
        .locator('#chat-messages .chat-msg:not(.system)')
        .evaluateAll((elements) =>
          elements.map((element) => ({
            id: element.dataset.messageId,
            content: element.querySelector('.msg-text').textContent,
          })),
        );
      retainedMessages(actual, messages, config.retention);
      await roster(client, config.guests + 2);
    }
    await poll(
      'guest_delivery_incomplete',
      async () => guests.every((guest) => guest.received.size === messages.length),
      Boolean,
    );
    for (const guest of guests)
      check(
        messages.every((message) => guest.received.has(message.id)),
        'guest_delivery_incomplete',
      );
    report.checks.exactBrowserDelivery = true;
    report.checks.exactGuestDelivery = true;
    report.checks.retainedMessages = true;
    report.historyRolloverExercised = messages.length > config.retention;
    check(
      config.profile !== 'full' || report.historyRolloverExercised,
      'full_profile_missing_rollover',
    );
    await wait(2000);
    await sample('post_traffic_quiescent');
    stage = 'explicit_leave';
    for (const guest of guests) guest.close();
    await poll(
      'guest_cleanup_timeout',
      async () => guests.every((guest) => guest.cleanClose),
      Boolean,
    );
    for (const client of clients) {
      await roster(client, clients.length);
      await client.page.locator('#leave-btn').click();
      await client.page.locator('#join-screen').waitFor({ state: 'visible' });
      const value = await state(client);
      validState(value);
      check(
        value.openPeers === 0 && value.attachedMediaElements === 0,
        'browser_resources_not_released',
      );
      // After the first browser leaves, the remaining roster contains itself.
      if (client === clients[0]) break;
    }
    await roster(member, 1);
    await member.page.locator('#leave-btn').click();
    await member.page.locator('#join-screen').waitFor({ state: 'visible' });
    stage = 'delete_owned_room';
    await deleteOwnedRoom();
    await poll(
      'membership_cleanup_incomplete',
      counts,
      (value) => value.rooms === 0 && value.participants === 0 && value.connections === 2,
    );
    await wait(2000);
    await sample('post_leave_quiescent');
    for (const client of clients) {
      const value = await state(client);
      validState(value);
      check(
        value.openPeers === 0 && value.attachedMediaElements === 0,
        'browser_resources_not_released',
      );
    }
    report.checks.explicitLeaves = true;
    report.checks.ownedRoomDeleted = true;
    workloadPassed = true;
  } catch (error) {
    report.failure ??= {
      stage,
      code: error instanceof StressFailure ? error.code : 'operation_failed',
    };
    if (report.traffic) {
      report.traffic.acceptedMessages = messages.length;
      report.traffic.elapsedMs = performance.now() - trafficStarted;
      report.traffic.acknowledgement = distribution(messages.map((message) => message.ackMs));
    }
    report.failureObservations = [];
    for (const client of clients) {
      let timer;
      try {
        const observed = await Promise.race([
          state(client),
          new Promise((_, reject) => {
            timer = setTimeout(
              () => reject(new StressFailure('failure_observation_timeout')),
              2000,
            );
          }),
        ]);
        report.failureObservations.push({
          client: client.label,
          available: true,
          counters: Object.fromEntries(
            Object.entries(observed).filter(([, value]) => typeof value === 'number'),
          ),
          observationFailure: observed.failure,
          seenMessages: observed.seenIds.length,
          renderedMessages: observed.renderedIds.length,
          framedMessages: observed.framedIds.length,
        });
      } catch {
        report.failureObservations.push({ client: client.label, available: false });
      } finally {
        clearTimeout(timer);
      }
    }
    try {
      report.failureServer = await counts();
    } catch {
      report.failureServer = { available: false };
    }
    save();
  } finally {
    stage = 'cleanup';
    // Includes two bounded page evaluations, owned browser shutdown and the
    // independently bounded server-gauge poll. A forced exit can never pass.
    finalWatchdog ??= setTimeout(() => {
      report.failure ??= { stage: 'cleanup', code: 'finalization_deadline' };
      report.passed = false;
      if (report.cleanup) report.cleanup.passed = false;
      try {
        save();
      } catch {}
      void browserServer?.kill().catch(() => {});
      process.exit(1);
    }, 65000);
    abort.abort();
    for (const client of clients) client.gate.dispose();
    report.cleanup = {
      guests: guests.length,
      cleanGuestClosures: 0,
      browserClosed: false,
      serverReturnedToZero: false,
      passed: false,
      errors: [],
    };
    try {
      for (const guest of guests) {
        try {
          guest.close();
        } catch {
          report.cleanup.errors.push('guest_close_failed');
          report.failure ??= { stage, code: 'guest_close_failed' };
        }
      }
      for (const client of clients) {
        if (!client.page.isClosed())
          await cleanupWithin(() =>
            client.page.evaluate(() => {
              window.__uiStress?.beginCleanup();
              window.__uiStress?.leave();
            }),
          ).catch(() => {
            report.cleanup.errors.push('browser_leave_cleanup_failed');
            report.failure ??= { stage, code: 'browser_leave_cleanup_failed' };
          });
      }
      await poll(
        'guest_cleanup_timeout',
        async () => guests.every((guest) => guest.cleanClose),
        Boolean,
        5000,
        true,
      );
      report.cleanup.cleanGuestClosures = guests.filter((guest) => guest.cleanClose).length;
    } catch {
      report.cleanup.errors.push('guest_cleanup_failed');
      report.failure ??= { stage, code: 'guest_cleanup_failed' };
    }
    try {
      await closeBrowser();
      report.cleanup.browserClosed = true;
    } catch {
      report.cleanup.errors.push('browser_cleanup_failed');
      report.failure ??= { stage, code: 'browser_cleanup_failed' };
    }
    try {
      report.finalServer = await poll(
        'server_cleanup_timeout',
        counts,
        (value) => value.rooms === 0 && value.participants === 0 && value.connections === 0,
        35000,
        true,
      );
      report.cleanup.serverReturnedToZero = true;
    } catch {
      report.cleanup.errors.push('server_cleanup_failed');
      report.failure ??= { stage, code: 'server_cleanup_failed' };
    }
    report.cleanup.passed =
      report.cleanup.errors.length === 0 &&
      report.cleanup.cleanGuestClosures === guests.length &&
      report.cleanup.browserClosed &&
      report.cleanup.serverReturnedToZero;
    report.completed = true;
    report.finishedAt = new Date().toISOString();
    report.durationMs = performance.now() - started;
    report.passed = workloadPassed && !report.failure && report.cleanup.passed;
    save();
    clearTimeout(deadline);
    clearTimeout(finalWatchdog);
  }
  return report;
}

if (require.main === module) {
  run()
    .then((report) => {
      console.log(
        JSON.stringify({
          passed: report.passed,
          profile: report.workload.profile,
          acceptedMessages: report.traffic?.acceptedMessages ?? 0,
          failure: report.failure ?? null,
        }),
      );
      if (!report.passed) process.exitCode = 1;
    })
    .catch(() => {
      console.error('UI stress configuration or final evidence could not be validated.');
      process.exitCode = 1;
    });
}

module.exports = { run, Guest, cleanupWithin };
