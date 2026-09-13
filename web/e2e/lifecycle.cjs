/** Owned, opt-in repeated-session checks; synthetic Chromium, never real devices. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { browserOptions } = require('./browser-options.cjs');
const { installLifecycleObservation } = require('./lifecycle-observer.cjs');
const {
  closeOwnedBrowser,
  startFinalizationWatchdog,
  canCancelFinalizationWatchdog,
} = require('./lifecycle-cleanup.cjs');
const {
  configuration,
  serverCounts,
  mediaProgress,
  releasedResources,
  handshakeGate,
} = require('./lifecycle-checks.cjs');
const {
  initializePerformanceReport,
  persistPerformanceReport,
  finalizePerformanceReport,
} = require('./performance-report.cjs');

async function run(env = process.env) {
  const config = configuration(env);
  const artifacts =
    env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-lifecycle.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const filename = path.join(artifacts, 'lifecycle-results.json');
  const sha = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const workdir = env.TEST_SERVER_WORKDIR || process.cwd();
  const report = {
    schemaVersion: 1,
    startedAt: new Date().toISOString(),
    completed: false,
    passed: false,
    workload: {
      clients: 2,
      cycles: config.cycles,
      mediaSecondsPerCycle: config.mediaSeconds,
      sampleIntervalMs: 3000,
      graceSeconds: 30,
    },
    provenance: {
      serverBinarySha256: env.TEST_SERVER_BINARY ? sha(env.TEST_SERVER_BINARY) : null,
      cargoLockSha256: sha(path.join(workdir, 'Cargo.lock')),
      webLockSha256: sha(path.join(workdir, 'web/package-lock.json')),
      entryHtmlSha256: sha(path.join(workdir, 'web/dist/index.html')),
      harnessSha256: sha(__filename),
      observerSha256: sha(path.join(__dirname, 'lifecycle-observer.cjs')),
      checksSha256: sha(path.join(__dirname, 'lifecycle-checks.cjs')),
      cleanupSha256: sha(path.join(__dirname, 'lifecycle-cleanup.cjs')),
      reportHelpersSha256: sha(path.join(__dirname, 'performance-report.cjs')),
      node: process.version,
      platform: process.platform,
      arch: process.arch,
    },
    limitations: [
      'Synthetic Chromium on loopback; not physical permissions, devices, mobile engines, capacity or an indefinite soak.',
      'Only an owned replacement signaling handshake is delayed. UDP, OS networking and server grace are unchanged; no protocol reply is forged.',
      'Signaling uses Playwright-routed page-visible sockets with real upstream connections; RTC peers and stats are native.',
      'Media progress is sampled, not proof of uninterrupted rendering or acoustic output.',
      'JS heap, DOM counters and sampled server RSS are informational trends without forced GC or hard leak thresholds; not whole-browser memory.',
      'Observer uses weak references and releases terminal resources. Attached media counts cover the live document, not detached DOM.',
      'Zero server gauges establish memberships/connections/rooms, not an exhaustive native allocation or transport-map audit.',
    ],
    cycles: [],
    memory: [],
    recovery: [],
    pageErrorCount: 0,
  };
  initializePerformanceReport(filename, report);
  const save = () => persistPerformanceReport(filename, report);
  const clients = [];
  let browser;
  let browserServer;
  let closePromise;
  let cancelWatchdog;
  let expired = false;
  const closeBrowser = () => {
    for (const client of clients) client.gate.dispose();
    cancelWatchdog ??= startFinalizationWatchdog(report, save, () => {
      // The final watchdog cannot report success. Escalate only the browser
      // created by this runner; exiting lets the outer service helpers clean up.
      try {
        void browserServer?.kill().catch(() => {});
      } finally {
        process.exit(1);
      }
    });
    closePromise ??= browserServer
      ? closeOwnedBrowser(browserServer, report, save)
      : Promise.resolve();
    return closePromise;
  };
  const deadlineMs = config.cycles * (config.mediaSeconds * 1000 + 45000) + 150000;
  const deadline = setTimeout(() => {
    expired = true;
    void closeBrowser().catch(() => {});
  }, deadlineMs);
  const wait = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
  const healthy = () => {
    assert.equal(expired, false, 'Lifecycle workload deadline exceeded');
    assert.equal(report.pageErrorCount, 0, 'Browser page error');
    for (const client of clients) assert.equal(client.gate.snapshot().failure, null);
  };
  async function poll(label, work, accept, milliseconds = 15000) {
    const until = performance.now() + milliseconds;
    let value;
    do {
      healthy();
      value = await work();
      if (accept(value)) return value;
      await wait(200);
    } while (performance.now() < until);
    throw new Error(`${label}: timed out`);
  }
  async function counts() {
    const response = await fetch(`${config.base}/metrics`, {
      headers: { Authorization: `Bearer ${env.TEST_METRICS_TOKEN}` },
      redirect: 'error',
      signal: AbortSignal.timeout(5000),
    });
    assert.equal(response.status, 200, 'Owned metrics endpoint unavailable');
    return serverCounts(await response.text());
  }
  const state = (client) => client.page.evaluate(() => window.__lifecycle.snapshot());
  async function memory(phase, cycle) {
    const raw = execFileSync('ps', ['-o', 'rss=', '-p', String(config.serverPid)], {
      encoding: 'utf8',
      timeout: 2000,
      maxBuffer: 4096,
    }).trim();
    assert.match(raw, /^[0-9]+$/, 'Owned server RSS unavailable');
    const samples = [];
    for (const client of clients) {
      const values = Object.fromEntries(
        (await client.cdp.send('Performance.getMetrics')).metrics.map((metric) => [
          metric.name,
          metric.value,
        ]),
      );
      assert.ok(Number.isFinite(values.JSHeapUsedSize) && values.JSHeapUsedSize > 0);
      const dom = await client.cdp.send('Memory.getDOMCounters');
      samples.push({
        client: client.label,
        jsHeapUsedBytes: values.JSHeapUsedSize,
        documents: dom.documents,
        nodes: dom.nodes,
        jsEventListeners: dom.jsEventListeners,
        resources: await state(client),
      });
    }
    const sample = {
      phase,
      cycle,
      elapsedMs: performance.now() - started,
      serverRssBytes: Number(raw) * 1024,
      server: await counts(),
      clients: samples,
    };
    report.memory.push(sample);
    save();
    return sample;
  }
  async function connected(client) {
    await client.page.locator('#room-screen').waitFor({ state: 'visible' });
    await client.page.waitForFunction(
      () => document.querySelector('#connection-status')?.textContent === 'Connected',
    );
  }
  async function playback(client) {
    const page = client.page;
    await page.waitForFunction(
      () =>
        document.querySelector('.video-tile:not(.local) video')?.videoWidth > 0 ||
        [...document.querySelectorAll('[data-control="retry-playback"]')].some(
          (button) => button.getClientRects().length,
        ),
    );
    const retry = page.locator('[data-control="retry-playback"]').filter({ visible: true });
    if (await retry.count()) await retry.first().click();
    await page.waitForFunction(() =>
      ['video', 'audio'].every((kind) =>
        [...document.querySelectorAll(`.video-tile:not(.local) ${kind}`)].some(
          (element) =>
            !element.paused &&
            (kind === 'video'
              ? element.videoWidth > 0
              : !element.muted && element.volume > 0 && element.readyState >= 2),
        ),
      ),
    );
  }
  async function capture(client, enabled) {
    for (const button of ['#cam-btn', '#mic-btn']) {
      const active = await client.page
        .locator(button)
        .evaluate((element) => element.classList.contains('active'));
      if (active !== enabled) await client.page.locator(button).click();
      await client.page.waitForFunction(
        ({ selector, enabled }) =>
          document.querySelector(selector).classList.contains('active') === enabled,
        { selector: button, enabled },
      );
    }
  }
  async function mediaSample(client) {
    return client.page.evaluate(async () => ({
      ...(await window.__lifecycle.mediaSample()),
      playback: [
        ...document.querySelectorAll(
          '.video-tile:not(.local) audio, .video-tile:not(.local) video',
        ),
      ].map((element) => ({
        kind: element.tagName.toLowerCase(),
        playing: !element.paused,
        currentTime: element.currentTime,
        audible: !element.muted && element.volume > 0,
      })),
    }));
  }
  async function progress(seconds, destination, selected = clients) {
    for (const client of selected) await playback(client);
    let previous = await Promise.all(selected.map(mediaSample));
    const until = performance.now() + seconds * 1000;
    do {
      await wait(3000);
      healthy();
      const current = await Promise.all(selected.map(mediaSample));
      const observation = {
        elapsedMs: performance.now() - started,
        before: previous,
        after: current,
        passed: false,
      };
      destination.push(observation);
      // Retain sanitized native evidence before evaluating progress assertions.
      save();
      observation.clients = current.map((sample, index) => {
        const delta = mediaProgress(previous[index], sample);
        for (const kind of ['video', 'audio']) {
          const old = previous[index].playback.find((element) => element.kind === kind);
          const now = sample.playback.find((element) => element.kind === kind);
          assert.ok(
            old && now && now.playing && now.currentTime > old.currentTime,
            `${kind} playback must advance`,
          );
          if (kind === 'audio') assert.equal(now.audible, true);
        }
        return { client: selected[index].label, ...delta };
      });
      observation.passed = true;
      previous = current;
      save();
    } while (performance.now() < until);
  }
  async function chat(cycle) {
    const content = `Lifecycle message ${cycle}`;
    await clients[0].page.locator('#chat-input').fill(content);
    await clients[0].page.locator('#chat-send-btn').click();
    await clients[1].page.getByText(content, { exact: true }).waitFor({ state: 'visible' });
  }
  async function retainedRecovery() {
    const client = clients[0];
    const before = await state(client);
    await client.page.evaluate(() => window.__lifecycle.closeCurrentSocket());
    await poll(
      'Retained recovery',
      () => state(client),
      (snapshot) => snapshot.reconnectSuccess === before.reconnectSuccess + 1,
    );
    await connected(client);
    const after = await state(client);
    for (const field of [
      'peersCreated',
      'capturesRequested',
      'sentJoinRoom',
      'sentProduce',
      'reconnectFailure',
    ])
      assert.equal(after[field], before[field], `${field} changed during retained recovery`);
    report.recovery.push({ kind: 'retained', before, after, passed: true });
    save();
  }
  async function expiredRecovery() {
    const client = clients[0];
    const before = await state(client);
    const evidence = { kind: 'expired', before, passed: false, mediaAfterRejoin: [] };
    report.recovery.push(evidence);
    save();
    client.gate.arm();
    const closedAt = performance.now();
    await client.page.evaluate(() => window.__lifecycle.closeCurrentSocket());
    evidence.retainedCounts = await poll(
      'Grace membership retained',
      counts,
      (value) => value.connections === 1 && value.participants === 2,
      5000,
    );
    await poll(
      'Replacement handshake held',
      async () => client.gate.snapshot(),
      (value) => value.held,
      5000,
    );
    evidence.expiredCounts = await poll(
      'Real server grace expiry',
      counts,
      (value) =>
        value.connections === 1 &&
        value.participants === 1 &&
        performance.now() - closedAt >= 30000,
      40000,
    );
    evidence.heldMilliseconds = performance.now() - closedAt;
    const heldState = await state(client);
    assert.equal(
      heldState.sentReconnect,
      before.sentReconnect,
      'Reconnect must not be sent before release',
    );
    assert.equal(heldState.sentJoinRoom, before.sentJoinRoom, 'Rejoin must not precede expiry');
    await clients[1].page.waitForFunction(() => !document.querySelector('.video-tile:not(.local)'));
    evidence.gateBeforeRelease = client.gate.snapshot();
    client.gate.release();
    await poll(
      'Actual rejected resume and fresh join',
      () => state(client),
      (value) =>
        value.reconnectFailure === before.reconnectFailure + 1 &&
        value.sentJoinRoom === before.sentJoinRoom + 1,
      20000,
    );
    await connected(client);
    await poll(
      'Stopped old media',
      () => state(client),
      (value) =>
        value.liveLocalTracks === 0 &&
        value.pendingCaptures === 0 &&
        value.peersClosed >= before.peersClosed + before.openPeers,
    );
    const after = await state(client);
    assert.equal(after.reconnectSuccess, before.reconnectSuccess);
    assert.equal(
      after.capturesRequested,
      before.capturesRequested,
      'Expired recovery must not request capture',
    );
    assert.equal(after.sentProduce, before.sentProduce, 'Expired recovery must not publish');
    // Device capability probing may create an additional short-lived peer.
    assert.ok(after.peersCreated >= before.peersCreated + 2, 'Fresh send/receive peers required');
    assert.equal(after.openPeers, 2, 'Only fresh send/receive peers may remain open');
    assert.equal(
      after.peersCreated - before.peersCreated,
      after.peersClosed - before.peersClosed,
      'Replacement peers must balance closed old/probe peers',
    );
    assert.equal(after.sentCreateSendTransport, before.sentCreateSendTransport + 1);
    assert.equal(after.sentCreateRecvTransport, before.sentCreateRecvTransport + 1);
    for (const selector of ['#cam-btn', '#mic-btn', '#screen-btn']) {
      assert.equal(
        await client.page
          .locator(selector)
          .evaluate((element) => element.classList.contains('active')),
        false,
      );
    }
    await client.page
      .getByText(
        'Room rejoined. Your microphone, camera, and screen sharing are off; turn them on when you are ready.',
        { exact: true },
      )
      .waitFor({ state: 'visible' });
    evidence.after = after;
    await progress(3, evidence.mediaAfterRejoin, [client]);
    assert.equal((await state(client)).capturesRequested, before.capturesRequested);
    await capture(client, true);
    await playback(clients[1]);
    evidence.afterExplicitRestart = await state(client);
    assert.ok(evidence.afterExplicitRestart.capturesRequested > before.capturesRequested);
    evidence.passed = true;
    save();
  }
  const started = performance.now();
  try {
    assert.deepEqual(
      await counts(),
      { rooms: 0, participants: 0, connections: 0 },
      'Server must be fresh and exclusively owned',
    );
    const modulePath = env.PLAYWRIGHT_MODULE || 'playwright';
    const { chromium } = require(modulePath);
    report.provenance.playwrightVersion = require(`${modulePath}/package.json`).version;
    const options = browserOptions('chromium');
    // Public ownership handle exposes actual child exit status, including any
    // forced termination, without relying on Playwright's private fields.
    browserServer = await chromium.launchServer({ ...options.launchOptions, host: '127.0.0.1' });
    browser = await chromium.connect(browserServer.wsEndpoint());
    report.provenance.browserVersion = browser.version();
    for (const label of ['one', 'two']) {
      const context = await browser.newContext({
        ...options.contextOptions,
        viewport: { width: 1440, height: 1000 },
      });
      const gate = handshakeGate();
      const client = { label, context, gate };
      clients.push(client);
      await context.routeWebSocket(`${config.base.replace(/^http/, 'ws')}/ws`, (route) =>
        gate.handle(route),
      );
      await context.addInitScript(installLifecycleObservation);
      client.page = await context.newPage();
      client.page.setDefaultTimeout(15000);
      client.page.on('pageerror', () => {
        report.pageErrorCount++;
      });
      client.cdp = await context.newCDPSession(client.page);
      await client.cdp.send('Performance.enable');
      await client.page.goto(config.base, { waitUntil: 'networkidle' });
    }
    report.baseline = await memory('initial', 0);
    for (let cycle = 1; cycle <= config.cycles; cycle++) {
      const item = { cycle, passed: false, progress: [] };
      report.cycles.push(item);
      const room = `lifecycle-${crypto.randomBytes(6).toString('hex')}`;
      for (const client of clients) {
        const before = await state(client);
        await client.page.locator('#name-input').fill(`Lifecycle ${client.label}`);
        await client.page.locator('#room-input').fill(room);
        await client.page.locator('#join-btn').click();
        await connected(client);
        assert.equal(
          (await state(client)).capturesRequested,
          before.capturesRequested,
          'Joining must not capture',
        );
        if (cycle === 1) {
          await client.page.locator('#mic-setup-btn').click();
          const dialog = client.page.getByRole('dialog', { name: 'Your settings', exact: true });
          await dialog.getByRole('button', { name: 'Save settings', exact: true }).click();
          await dialog.waitFor({ state: 'hidden' });
          assert.equal(
            (await state(client)).capturesRequested,
            before.capturesRequested,
            'Saving setup must not capture',
          );
        }
      }
      await poll(
        'Both memberships',
        counts,
        (value) => value.rooms === 1 && value.participants === 2 && value.connections === 2,
      );
      for (const client of clients) await capture(client, true);
      for (const client of clients) await playback(client);
      if (cycle === 2) await retainedRecovery();
      if (cycle === 3) await expiredRecovery();
      await chat(cycle);
      await progress(config.mediaSeconds, item.progress);
      await memory('active', cycle);
      for (const client of clients) {
        await capture(client, false);
        await poll(
          'Explicit capture stopped',
          () => state(client),
          (value) => value.liveLocalTracks === 0 && value.pendingCaptures === 0,
        );
        await client.page.locator('#leave-btn').click();
        await client.page.locator('#join-screen').waitFor({ state: 'visible' });
      }
      for (const client of clients) {
        const released = await poll(
          'Browser resources released',
          () => state(client),
          (value) =>
            ['openPeers', 'liveLocalTracks', 'pendingCaptures', 'attachedMediaElements'].every(
              (field) => value[field] === 0,
            ) && value.openSockets === 1,
        );
        releasedResources(released, 1);
      }
      item.cleanup = [];
      for (let sample = 0; sample < 3; sample++) {
        item.cleanup.push(
          await poll(
            'Server memberships released',
            counts,
            (value) => value.rooms === 0 && value.participants === 0 && value.connections === 2,
          ),
        );
        await wait(250);
      }
      await memory('released', cycle);
      item.passed = true;
      save();
      console.log(`PASS lifecycle cycle ${cycle}/${config.cycles}`);
    }
    await closeBrowser();
    report.finalServerCounts = await poll(
      'Final sockets released',
      counts,
      (value) => value.rooms === 0 && value.participants === 0 && value.connections === 0,
    );
    healthy();
    report.completed = true;
    report.passed = true;
  } catch (error) {
    report.failure = (error?.message || String(error))
      .replaceAll(env.TEST_METRICS_TOKEN, '[redacted]')
      .slice(0, 2048);
    throw error;
  } finally {
    clearTimeout(deadline);
    try {
      await finalizePerformanceReport(report, closeBrowser, save);
    } finally {
      if (canCancelFinalizationWatchdog(browserServer, report)) cancelWatchdog?.();
    }
  }
  return report;
}

module.exports = { run };
if (require.main === module)
  run().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
