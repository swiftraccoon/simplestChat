/**
 * Informational, small-workload browser/API measurement. No production target,
 * synthetic capacity inference, or noisy hard performance thresholds.
 * PERFORMANCE_E2E=1 BASE_URL=http://127.0.0.1:3119 node web/e2e/performance.cjs
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { decodedVideoFrames } = require('./performance-metrics.cjs');
const { browserOptions } = require('./browser-options.cjs');
const browserConfiguration = browserOptions('chromium');
if (process.env.PERFORMANCE_E2E !== '1')
  throw new Error('Set PERFORMANCE_E2E=1 against an owned disposable local server/database.');
const base = process.env.BASE_URL || 'http://127.0.0.1:3119';
const origin = new URL(base);
if (
  !['localhost', '127.0.0.1', '[::1]'].includes(origin.hostname) ||
  !['http:', 'https:'].includes(origin.protocol) ||
  origin.username ||
  origin.password
) {
  throw new Error('Browser performance tests require a credential-free loopback HTTP(S) URL.');
}
const playwrightModule = process.env.PLAYWRIGHT_MODULE || 'playwright';
const { chromium } = require(playwrightModule);
const artifacts =
  process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-browser-perf.'));
fs.mkdirSync(artifacts, { recursive: true });
const runId = `perf-${Date.now().toString(36)}`;
const sha256 = (value) => crypto.createHash('sha256').update(value).digest('hex');
const hashFile = (filename) => (filename ? sha256(fs.readFileSync(filename)) : null);
const command = (name, args) => {
  try {
    return execFileSync(name, args, { encoding: 'utf8' }).trim();
  } catch {
    return null;
  }
};
const report = {
  schemaVersion: 1,
  completed: false,
  passed: false,
  startedAt: new Date().toISOString(),
  runId,
  label: process.env.RUN_LABEL || '',
  provenance: {
    serverRevision: process.env.SERVER_REVISION || 'unknown',
    frontendRevision: process.env.FRONTEND_REVISION || 'unknown',
    serverBinarySha256: hashFile(process.env.TEST_SERVER_BINARY),
    cargoLockSha256: hashFile(
      path.join(process.env.TEST_SERVER_WORKDIR || process.cwd(), 'Cargo.lock'),
    ),
    webLockSha256: hashFile(
      path.join(process.env.TEST_SERVER_WORKDIR || process.cwd(), 'web/package-lock.json'),
    ),
    buildToolchain: process.env.BUILD_TOOLCHAIN || 'not supplied',
    harnessSha256: hashFile(__filename),
    measurementHelpersSha256: hashFile(path.join(__dirname, 'performance-metrics.cjs')),
    browserOptionsSha256: hashFile(path.join(__dirname, 'browser-options.cjs')),
    playwrightVersion: require(`${playwrightModule}/package.json`).version,
    node: process.version,
    os: os.type(),
    osRelease: os.release(),
    arch: os.arch(),
    cpu: os.cpus()[0]?.model,
    logicalCpus: os.cpus().length,
    totalMemoryBytes: os.totalmem(),
    postgres: command('postgres', ['--version']),
    baseUrl: origin.origin,
    serverWorkdir: process.env.TEST_SERVER_WORKDIR || process.cwd(),
    workload: {
      users: 2,
      rooms: 1,
      publicMessages: 10,
      messageIntervalMs: 650,
      fakeCameraSeconds: 5,
    },
  },
  limitations: [
    'Informational small local workload; not a capacity test or statistically stable regression budget.',
    'New browser and separate cold HTTP-cache contexts per run; operating-system file caches are not flushed.',
    'Main-thread TaskDuration and JSHeapUsedSize are Chromium CDP samples, not whole-browser CPU/RSS.',
    'Cross-page chat delay includes Playwright scheduling/polling; it is not network-only RTT.',
    'Fake camera and loopback ICE require Chromium-specific flags; real devices and other browsers are not covered.',
    'PeerConnection constructor instrumentation is identical across both revisions.',
  ],
  startup: [],
  api: [],
  chatDeliveryMs: [],
  media: null,
  pageErrors: [],
};
const clients = [];
const pendingBodies = [];
let browser;

async function makeClient(label) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    ...browserConfiguration.contextOptions,
  });
  await context.addInitScript(() => {
    window.__perfPeers = [];
    window.__perfLongTasks = [];
    const Original = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends Original {
      constructor(...args) {
        super(...args);
        window.__perfPeers.push(this);
      }
    };
    new PerformanceObserver((list) => {
      window.__perfLongTasks.push(
        ...list
          .getEntries()
          .map((item) => ({ startTime: item.startTime, duration: item.duration })),
      );
    }).observe({ type: 'longtask', buffered: true });
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  const cdp = await context.newCDPSession(page);
  await cdp.send('Performance.enable');
  const assets = [];
  page.on('pageerror', (error) =>
    report.pageErrors.push({ client: label, message: error.message }),
  );
  page.on('response', (response) => {
    if (['script', 'stylesheet'].includes(response.request().resourceType())) {
      pendingBodies.push(
        response.body().then((body) =>
          assets.push({
            path: new URL(response.url()).pathname,
            decodedBytes: body.length,
            sha256: sha256(body),
          }),
        ),
      );
    }
  });
  page.on('requestfinished', (request) => {
    const url = new URL(request.url());
    if (url.pathname.startsWith('/api/')) {
      report.api.push({
        client: label,
        method: request.method(),
        path: url.pathname,
        timing: request.timing(),
      });
    }
  });
  const item = { page, context, cdp, label };
  clients.push(item);
  const start = performance.now();
  await page.goto(base, { waitUntil: 'networkidle' });
  await page.locator('.conversation-toolbar').waitFor({ state: 'attached' });
  await page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );
  const timing = await page.evaluate(() => ({
    navigation: performance.getEntriesByType('navigation')[0]?.toJSON(),
    paint: performance.getEntriesByType('paint').map((item) => item.toJSON()),
    resources: performance.getEntriesByType('resource').map((item) => ({
      path: new URL(item.name).pathname,
      initiatorType: item.initiatorType,
      transferSize: item.transferSize,
      encodedBodySize: item.encodedBodySize,
      decodedBodySize: item.decodedBodySize,
      duration: item.duration,
    })),
    longTasks: window.__perfLongTasks,
  }));
  const metrics = Object.fromEntries(
    (await cdp.send('Performance.getMetrics')).metrics.map((item) => [item.name, item.value]),
  );
  report.startup.push({
    client: label,
    readyWallMs: performance.now() - start,
    ...timing,
    taskDurationSeconds: metrics.TaskDuration,
    scriptDurationSeconds: metrics.ScriptDuration,
    jsHeapUsedBytes: metrics.JSHeapUsedSize,
    jsHeapTotalBytes: metrics.JSHeapTotalSize,
    assets,
  });
  return page;
}

async function register(page, label) {
  await page.locator('#sign-in-btn').click();
  await page.locator('#login-to-register').click();
  await page.locator('#register-email').fill(`${runId}-${label}@example.test`);
  await page.locator('#register-name').fill(`Perf ${label}`);
  await page.locator('#register-password').fill('Disposable-browser-perf-password-2026!');
  await page.locator('#register-confirm').fill('Disposable-browser-perf-password-2026!');
  await page.locator('#register-submit').click();
  await page.locator('#register-modal').waitFor({ state: 'hidden' });
  await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
}

async function connected(page) {
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  await page.waitForFunction(
    () => document.querySelector('#connection-status').textContent === 'Connected',
  );
}

async function inboundStats(page) {
  return page.evaluate(async () => {
    const results = [];
    for (const peer of window.__perfPeers) {
      for (const stat of (await peer.getStats()).values()) {
        if (stat.type !== 'inbound-rtp') continue;
        const fields = [
          'id',
          'kind',
          'timestamp',
          'bytesReceived',
          'packetsReceived',
          'packetsLost',
          'jitter',
          'framesReceived',
          'framesDecoded',
          'framesDropped',
          'frameWidth',
          'frameHeight',
          'framesPerSecond',
          'totalDecodeTime',
          'totalFreezesDuration',
          'freezeCount',
        ];
        results.push(
          Object.fromEntries(
            fields.filter((key) => stat[key] !== undefined).map((key) => [key, stat[key]]),
          ),
        );
      }
    }
    return results;
  });
}

(async () => {
  try {
    browser = await chromium.launch(browserConfiguration.launchOptions);
    report.provenance.browserVersion = browser.version();
    const owner = await makeClient('owner');
    const member = await makeClient('member');
    await register(owner, 'owner');
    await register(member, 'member');
    await owner.locator('#create-room-btn').click();
    await owner.locator('#cr-id').fill(runId);
    await owner.locator('#cr-name').fill('Disposable browser performance room');
    const createStart = performance.now();
    await owner.locator('#create-room-submit').click();
    await connected(owner);
    report.roomCreateToConnectedMs = performance.now() - createStart;
    await member.locator('#name-input').fill('Perf member');
    await member.locator('#room-input').fill(runId);
    const joinStart = performance.now();
    await member.locator('#join-btn').click();
    await connected(member);
    report.memberJoinToConnectedMs = performance.now() - joinStart;
    for (let i = 0; i < 10; i++) {
      await owner.waitForTimeout(650);
      const text = `${runId}-message-${i}`;
      await owner.locator('#chat-input').fill(text);
      const start = performance.now();
      await owner.locator('#chat-send-btn').click();
      await member.getByText(text, { exact: true }).waitFor({ state: 'visible' });
      report.chatDeliveryMs.push(performance.now() - start);
    }

    // Mark setup complete without starting preview or including dialog interaction
    // in the camera-publish-to-first-decoded-frame measurement.
    await owner.locator('#mic-setup-btn').click();
    // Support the baseline and current product labels with the same harness.
    // Playwright's strict locator still requires one matching native dialog.
    const setup = owner.getByRole('dialog', {
      name: /^(Camera & microphone|Your settings)$/,
    });
    await setup.getByRole('button', { name: 'Save settings', exact: true }).click();
    await setup.waitFor({ state: 'hidden' });
    const cameraStart = performance.now();
    await owner.locator('#cam-btn').click();
    await member.waitForFunction(() =>
      [...document.querySelectorAll('.video-tile:not(.local) video')].some(
        (video) => video.videoWidth > 0,
      ),
    );
    const firstDecodedFrameMs = performance.now() - cameraStart;
    const before = await inboundStats(member);
    await member.waitForTimeout(5000);
    const after = await inboundStats(member);
    // Chromium can retain a capabilities/probation SSRC with zero decoded
    // frames before the actual receiver entry. Never assume the first is active.
    const framesDecoded = decodedVideoFrames(before, after);
    report.media = {
      firstDecodedFrameMs,
      before,
      after,
      framesDecodedInFiveSeconds: framesDecoded,
    };
    assert.ok(framesDecoded > 0, 'Receiver must decode sustained camera frames');
    await owner.locator('#cam-btn').click();
    for (const { page } of clients) {
      await page.locator('#leave-btn').click();
      await page.locator('#join-screen').waitFor({ state: 'visible' });
    }
    report.cleanup = await Promise.all(
      clients.map(async ({ page, label }) => ({
        client: label,
        ...(await page.evaluate(() => ({
          openPeerConnections: window.__perfPeers.filter(
            (peer) => peer.connectionState !== 'closed',
          ).length,
          attachedMediaStreams: [...document.querySelectorAll('video,audio')].filter(
            (element) => element.srcObject,
          ).length,
        }))),
      })),
    );
    for (const item of report.cleanup) {
      assert.equal(item.openPeerConnections, 0, `${item.client} must close peer connections`);
      assert.equal(item.attachedMediaStreams, 0, `${item.client} must release media elements`);
    }
    assert.deepEqual(report.pageErrors, []);
    await Promise.all(pendingBodies);
    report.completed = true;
    report.passed = true;
  } catch (error) {
    report.failure = error.stack || error.message;
    throw error;
  } finally {
    await browser?.close();
    report.finishedAt = new Date().toISOString();
    fs.writeFileSync(
      path.join(artifacts, 'browser-performance.json'),
      JSON.stringify(report, null, 2),
    );
    console.log(`${report.passed ? 'PASS' : 'FAIL'} browser/API performance: ${artifacts}`);
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
