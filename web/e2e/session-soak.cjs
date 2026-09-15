/**
 * Real-time, authenticated UI continuity across scheduled refresh and original
 * access-token expiry. Run only through the owned disposable server helper.
 * Smoke validates the harness but can never claim refresh/expiry coverage.
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { setTimeout: delay } = require('node:timers/promises');
const {
  configuration,
  sessionExpiry,
  continuityFailure,
  expiryCoverage,
  installSessionObservation,
} = require('./session-soak-checks.cjs');
const { distribution, retainedMessages } = require('./ui-stress-metrics.cjs');
const { serverCounts } = require('./lifecycle-checks.cjs');
const { closeOwnedBrowser } = require('./lifecycle-cleanup.cjs');
const {
  initializePerformanceReport,
  persistPerformanceReport,
} = require('./performance-report.cjs');

class SoakFailure extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}
const check = (condition, code) => {
  if (!condition) throw new SoakFailure(code);
};

/** Includes evaluations/fetch bodies, which lack Playwright's action timeout. */
async function within(action, milliseconds = 5000) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(action),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new SoakFailure('operation_timeout')), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function run(env = process.env) {
  const config = configuration(env);
  process.kill(config.serverPid, 0);
  const artifacts =
    env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-session-soak.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const filename = path.join(artifacts, 'session-soak-results.json');
  const runId = `session-soak-${crypto.randomUUID()}`;
  const hash = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const workdir = env.TEST_SERVER_WORKDIR || process.cwd();
  const playwrightModule = env.PLAYWRIGHT_MODULE || 'playwright';
  const { chromium } = require(playwrightModule);
  const started = performance.now();
  const report = {
    schemaVersion: 1,
    runId,
    startedAt: new Date().toISOString(),
    completed: false,
    passed: false,
    workload: config,
    provenance: {
      serverRevision: env.SERVER_REVISION || 'not supplied',
      frontendRevision: env.FRONTEND_REVISION || 'not supplied',
      serverBinarySha256: env.TEST_SERVER_BINARY ? hash(env.TEST_SERVER_BINARY) : null,
      cargoLockSha256: hash(path.join(workdir, 'Cargo.lock')),
      webEntrySha256: hash(path.join(workdir, 'web/dist/index.html')),
      harnessSha256: hash(__filename),
      checksSha256: hash(path.join(__dirname, 'session-soak-checks.cjs')),
      node: process.version,
      platform: process.platform,
      osRelease: os.release(),
      arch: process.arch,
    },
    limitations: [
      'Owned loopback Chromium, two separately registered accounts, desktop and 375px viewport; not physical Safari/Firefox/mobile coverage.',
      'Real wall-clock refresh/expiry only. No shortened token lifetime, forced disconnect, guest traffic, public origin, or production limit overrides.',
      'Chromium background timer/render throttling is disabled for active foreground-style continuity. This does not cover suspended tabs, browser sleep, or device sleep.',
      'Text only; capture APIs reject without opening camera, microphone, or screen. No recording, screenshot, trace, cookie, credential, or raw frame artifacts.',
      'A socket close or full rejoin fails seamless continuity even if subsequent messages recover. First failure remains primary during at most 60 seconds of recovery observation.',
      'Exact live acknowledgements and peer DOM delivery are checked per message. Final retained history is checked separately; timing is not a hard performance threshold.',
      'Accounts belong to the disposable database. Only the uniquely created room is explicitly deleted; outer helper records server shutdown independently.',
    ],
    authentication: [],
    clients: [],
    samples: [],
    messages: [],
    checks: {},
    pageErrorCount: 0,
    unexpectedOriginCount: 0,
  };
  initializePerformanceReport(filename, report);
  const save = () => persistPerformanceReport(filename, report);
  const clients = [];
  const accepted = [];
  const authPending = new Set();
  let stage = 'startup';
  let browserServer;
  let browser;
  let closePromise;
  let firstFailureAt = null;
  let roomCreated = false;
  let roomDeleted = false;
  let trafficStarted;
  let expired = false;
  let finalWatchdog;
  const closeBrowser = () =>
    (closePromise ??= browserServer
      ? closeOwnedBrowser(browserServer, report, save)
      : Promise.resolve());
  const fail = (code, client) => {
    if (!report.failure) {
      report.failure = { stage, code, ...(client ? { client } : {}), atMs: Date.now() };
      firstFailureAt = performance.now();
      save();
    }
  };
  async function poll(code, action, accept, milliseconds = 15000, cleanup = false) {
    const until = performance.now() + milliseconds;
    do {
      if (!cleanup) check(!expired, 'workload_deadline');
      const value = await within(action);
      if (accept(value)) return value;
      await delay(100);
    } while (performance.now() < until);
    throw new SoakFailure(code);
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
    return serverCounts(Buffer.concat(chunks, size).toString('utf8'));
  }
  const status = (client) =>
    within(() => client.page.evaluate(() => window.__sessionSoak.status()));
  const roster = (client) =>
    within(() =>
      client.page.evaluate(() =>
        ['#participant-list', '#classic-users-panel .classic-user-list'].map((selector) =>
          [...document.querySelectorAll(`${selector} > li[data-participant-id]`)]
            .map((element) => ({
              id: element.dataset.participantId,
              role: element.querySelector('.role-badge')?.className || 'unbadged',
            }))
            .sort((left, right) => left.id.localeCompare(right.id)),
        ),
      ),
    );
  async function connected(client) {
    await client.page.locator('#room-screen').waitFor({ state: 'visible' });
    await client.page.waitForFunction(
      () =>
        document.querySelector('#connection-status')?.textContent === 'Connected' &&
        !document.querySelector('#chat-input')?.disabled,
    );
  }
  async function makeClient(label, viewport) {
    const context = await browser.newContext({ viewport, permissions: [] });
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    const authentication = {
      client: label,
      registeredAtMs: null,
      originalExpiresAtMs: null,
      refreshes: [],
      refreshFailures: 0,
    };
    const client = {
      label,
      context,
      page,
      viewport,
      navigationCount: 0,
      authentication,
      draft: `Unsent ${label} continuity draft`,
    };
    clients.push(client);
    report.authentication.push(authentication);
    await context.route('**/*', (route) => {
      if (new URL(route.request().url()).origin === config.base) return route.continue();
      report.unexpectedOriginCount++;
      return route.abort();
    });
    await page.addInitScript(installSessionObservation);
    page.on('pageerror', () => {
      report.pageErrorCount++;
    });
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) client.navigationCount++;
    });
    page.on('response', (response) => {
      const url = new URL(response.url());
      if (
        url.origin !== config.base ||
        !['/api/auth/register', '/api/auth/refresh'].includes(url.pathname)
      )
        return;
      // Startup restore is expected to return 401 before registration.
      const registration = url.pathname === '/api/auth/register';
      if (!registration && authentication.originalExpiresAtMs === null) return;
      const task = within(async () => {
        if (response.status() !== 200) {
          if (registration) fail('registration_response_failed', label);
          else authentication.refreshFailures++;
          return;
        }
        const body = await response.body();
        check(body.byteLength <= 32768, 'auth_response_limit');
        const atMs = Date.now();
        const expiresAtMs = sessionExpiry(JSON.parse(body.toString('utf8')), atMs);
        if (registration) {
          check(authentication.originalExpiresAtMs === null, 'repeated_registration');
          authentication.registeredAtMs = atMs;
          authentication.originalExpiresAtMs = expiresAtMs;
        } else {
          check(authentication.refreshes.length < 8, 'refresh_observation_limit');
          authentication.refreshes.push({ atMs, expiresAtMs });
        }
      }).catch(() => fail('auth_observation_failed', label));
      authPending.add(task);
      void task.finally(() => authPending.delete(task));
    });
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
    await page.locator('#register-name').fill(`Soak ${label}`);
    await page.locator('#register-password').fill(password);
    await page.locator('#register-confirm').fill(password);
    await page.locator('#register-submit').click();
    await page.locator('#register-modal').waitFor({ state: 'hidden' });
    await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
    await poll(
      'registration_not_observed',
      async () => client.authentication.originalExpiresAtMs,
      (value) => Number.isFinite(value),
    );
    await poll(
      'authenticated_socket_unavailable',
      () => status(client),
      (value) => value.openSockets === 1,
    );
  }
  async function deleteRoom() {
    if (!roomCreated || roomDeleted || !clients[0] || clients[0].page.isClosed()) return;
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
  async function sample(retain = false) {
    if (report.pageErrorCount) fail('browser_page_error');
    if (report.unexpectedOriginCount) fail('unexpected_browser_origin');
    const observations = [];
    for (const client of clients) {
      const observation = {
        ...(await status(client)),
        initialNavigationCount: client.initialNavigationCount,
        navigationCount: client.navigationCount,
      };
      const code = continuityFailure(observation);
      if (code) fail(code, client.label);
      if (client.authentication.refreshFailures) fail('scheduled_refresh_failed', client.label);
      const input = client.page.locator('#chat-input');
      if (!observation.inputDisabled && (await input.inputValue()) !== client.draft)
        fail('unsent_draft_lost', client.label);
      const actualRoster = await roster(client);
      if (JSON.stringify(actualRoster) !== client.initialRoster)
        fail('roster_or_role_changed', client.label);
      if (await client.page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1))
        fail('page_overflow', client.label);
      observations.push({ client: client.label, ...observation });
    }
    report.clients = observations;
    if (retain) {
      check(report.samples.length < 64, 'sample_limit');
      report.samples.push({
        atMs: Date.now(),
        acceptedMessages: accepted.length,
        clients: observations.map(
          ({ events: _events, participantId: _participantId, ...value }) => value,
        ),
        server: await counts(),
      });
      save();
    }
    return observations;
  }
  async function send(index) {
    const sender = clients[index % 2];
    const peer = clients[(index + 1) % 2];
    if (await sender.page.locator('#chat-input').isDisabled()) {
      fail('chat_input_disabled', sender.label);
      return;
    }
    const content = `Session soak ${String(index + 1).padStart(4, '0')}`;
    for (const client of clients)
      await client.page.evaluate((value) => window.__sessionSoak.expect(value), {
        content,
        senderId: sender.participantId,
        own: client === sender,
      });
    const at = performance.now();
    await sender.page.locator('#chat-input').fill(content);
    await sender.page.locator('#chat-send-btn').click();
    const ack = await poll(
      'message_ack_missing',
      () => sender.page.evaluate((text) => window.__sessionSoak.delivery(text), content),
      (value) => value?.ack === true,
    );
    const ackMs = performance.now() - at;
    const delivered = await poll(
      'peer_live_delivery_missing',
      () => peer.page.evaluate((text) => window.__sessionSoak.delivery(text), content),
      (value) => value?.received === true,
    );
    check(delivered.id === ack.id, 'peer_ack_identity_mismatch');
    for (const client of clients) {
      const row = client.page.locator(`#chat-messages .chat-msg[data-message-id="${ack.id}"]`);
      await row.waitFor({ state: 'visible' });
      check(
        (await row.count()) === 1 && (await row.locator('.msg-text').textContent()) === content,
        'message_dom_mismatch',
      );
    }
    const deliveryMs = performance.now() - at;
    accepted.push({ id: ack.id, content });
    report.messages.push({
      sequence: index + 1,
      sender: sender.label,
      atMs: Date.now(),
      ackMs,
      deliveryMs,
    });
    await sender.page.locator('#chat-input').fill(sender.draft);
  }

  const deadline = setTimeout(() => {
    expired = true;
    fail('workload_deadline');
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
    report.initialServer = await counts();
    check(
      Object.values(report.initialServer).every((value) => value === 0),
      'test_server_not_empty',
    );
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
    stage = 'create_and_join_room';
    await owner.page.locator('#create-room-btn').click();
    await owner.page.locator('#cr-id').fill(runId);
    await owner.page.locator('#cr-name').fill('Owned session continuity room');
    await owner.page.locator('#create-room-submit').click();
    await connected(owner);
    roomCreated = true;
    await delay(6300); // Keep ordinary per-IP room admission limits intact.
    await member.page.locator('#name-input').fill('Soak member');
    await member.page.locator('#room-input').fill(runId);
    await member.page.locator('#join-btn').click();
    await connected(member);
    for (const client of clients) {
      await poll(
        'initial_roster_incomplete',
        () => roster(client),
        (value) =>
          value.every(
            (list) => list.length === 2 && new Set(list.map((entry) => entry.id)).size === 2,
          ),
      );
      const lists = await roster(client);
      check(JSON.stringify(lists[0]) === JSON.stringify(lists[1]), 'roster_views_disagree');
      check(
        lists[0].some((entry) => entry.role.includes('role-owner')),
        'owner_role_missing',
      );
      client.initialRoster = JSON.stringify(lists);
      client.participantId = (await status(client)).participantId;
      await client.page.locator('#chat-input').fill(client.draft);
      // Initial room entry legitimately changes the same-document room hash.
      // Only navigation after the fully joined cohort can break continuity.
      client.initialNavigationCount = client.navigationCount;
      check(
        Number.isSafeInteger(client.initialNavigationCount) && client.initialNavigationCount > 0,
        'initial_navigation_not_observed',
      );
      await client.page.evaluate(() => window.__sessionSoak.arm());
    }
    check(owner.participantId !== member.participantId, 'accounts_not_distinct');
    check(owner.initialRoster === member.initialRoster, 'peer_rosters_disagree');
    await poll(
      'cohort_counts_incomplete',
      counts,
      (value) => value.rooms === 1 && value.participants === 2 && value.connections === 2,
    );
    report.checks.authenticatedCohort = true;
    stage = 'continuity_traffic';
    trafficStarted = performance.now();
    let nextSend = trafficStarted;
    let nextSample = trafficStarted;
    let index = 0;
    while (
      performance.now() - trafficStarted < config.durationMs &&
      (firstFailureAt === null || performance.now() - firstFailureAt < config.recoveryMs)
    ) {
      check(!expired, 'workload_deadline');
      const now = performance.now();
      await sample(now >= nextSample);
      if (now >= nextSample) nextSample = performance.now() + 30000;
      if (now >= nextSend) {
        try {
          await send(index++);
        } catch (error) {
          fail(error instanceof SoakFailure ? error.code : 'message_operation_failed');
          // Do not retry a failed message or clear a draft during recovery.
        }
        nextSend = performance.now() + config.messageIntervalMs;
      }
      await delay(250);
    }
    report.traffic = {
      elapsedMs: performance.now() - trafficStarted,
      attemptedMessages: index,
      acceptedMessages: accepted.length,
      acknowledgement: distribution(report.messages.map((entry) => entry.ackMs)),
      delivery: distribution(report.messages.map((entry) => entry.deliveryMs)),
    };
    await sample(true);
    stage = 'coverage_and_retention';
    await Promise.all([...authPending]);
    report.checks.refreshAndOriginalExpiry = expiryCoverage(report.authentication, Date.now());
    if (config.profile === 'full' && !report.checks.refreshAndOriginalExpiry)
      fail('refresh_and_expiry_coverage_incomplete');
    check(
      accepted.length > 0 && new Set(accepted.map((entry) => entry.id)).size === accepted.length,
      'accepted_message_count',
    );
    for (const client of clients) {
      const actual = await client.page
        .locator('#chat-messages .chat-msg:not(.system)')
        .evaluateAll((elements) =>
          elements.map((element) => ({
            id: element.dataset.messageId,
            content: element.querySelector('.msg-text').textContent,
          })),
        );
      retainedMessages(actual, accepted, 300);
    }
    report.checks.exactRetainedHistory = true;
    report.checks.seamlessContinuity = !report.failure;
  } catch (error) {
    fail(error instanceof SoakFailure ? error.code : 'operation_failed');
  } finally {
    stage = 'cleanup';
    finalWatchdog ??= setTimeout(() => {
      fail('finalization_deadline');
      try {
        save();
      } catch {}
      void browserServer?.kill().catch(() => {});
      process.exit(1);
    }, 110000);
    report.cleanup = {
      explicitLeaves: 0,
      ownedRoomDeleted: false,
      browserClosed: false,
      serverReturnedToZero: false,
      passed: false,
      errors: [],
    };
    for (const client of clients) {
      try {
        if (client.page.isClosed()) continue;
        const observation = await status(client);
        report.clients = report.clients.filter((value) => value.client !== client.label);
        report.clients.push({
          client: client.label,
          ...observation,
          initialNavigationCount: client.initialNavigationCount,
          navigationCount: client.navigationCount,
        });
        await within(() => client.page.evaluate(() => window.__sessionSoak.cleanup()), 2000);
        if (await client.page.locator('#room-screen').isVisible()) {
          await client.page.locator('#leave-btn').click({ timeout: 5000 });
          await client.page.locator('#join-screen').waitFor({ state: 'visible', timeout: 5000 });
          report.cleanup.explicitLeaves++;
        }
      } catch {
        report.cleanup.errors.push('browser_leave_failed');
        fail('browser_leave_failed', client.label);
      }
    }
    try {
      await within(deleteRoom, 20000);
      report.cleanup.ownedRoomDeleted = roomDeleted;
      if (roomCreated) check(roomDeleted, 'owned_room_not_deleted');
    } catch {
      report.cleanup.errors.push('owned_room_cleanup_failed');
      fail('owned_room_cleanup_failed');
    }
    try {
      await closeBrowser();
      report.cleanup.browserClosed = true;
    } catch {
      report.cleanup.errors.push('browser_cleanup_failed');
      fail('browser_cleanup_failed');
    }
    try {
      report.finalServer = await poll(
        'server_cleanup_timeout',
        counts,
        (value) => Object.values(value).every((count) => count === 0),
        35000,
        true,
      );
      report.cleanup.serverReturnedToZero = true;
    } catch {
      report.cleanup.errors.push('server_cleanup_failed');
      fail('server_cleanup_failed');
    }
    report.cleanup.passed =
      report.cleanup.errors.length === 0 &&
      report.cleanup.browserClosed &&
      report.cleanup.serverReturnedToZero &&
      (!roomCreated || (report.cleanup.ownedRoomDeleted && report.cleanup.explicitLeaves === 2));
    report.completed = true;
    report.finishedAt = new Date().toISOString();
    report.durationMs = performance.now() - started;
    report.passed =
      report.checks.seamlessContinuity === true && !report.failure && report.cleanup.passed;
    save();
    clearTimeout(deadline);
    clearTimeout(finalWatchdog);
  }
  return report;
}

if (require.main === module)
  run()
    .then((report) => {
      console.log(
        JSON.stringify({
          passed: report.passed,
          profile: report.workload.profile,
          acceptedMessages: report.traffic?.acceptedMessages ?? 0,
          refreshAndOriginalExpiry: report.checks.refreshAndOriginalExpiry ?? false,
          failure: report.failure ?? null,
        }),
      );
      if (!report.passed) process.exitCode = 1;
    })
    .catch(() => {
      console.error('Session soak configuration or final evidence could not be validated.');
      process.exitCode = 1;
    });

module.exports = { run, within };
