const {
  communityStartupReady,
  navigateCommunityPage,
  installCommunityCaptureObservation,
  reconnectMediaIdentity,
  waitForMediaStart,
  advancingReconnectMedia,
  createCommunityStep,
  createTestNetworkOwnership,
  closeCommunityClients,
  httpRoute: classifyCommunityHttpRoute,
} = require('./community-runtime.cjs');
/**
 * Opt-in real-browser smoke. Use ONLY an isolated database/server: this creates
 * disposable accounts and creates/deletes its own uniquely named room.
 * COMMUNITY_E2E=1 BASE_URL=http://127.0.0.1:3109
 * PLAYWRIGHT_MODULE=/path/to/node_modules/playwright node web/e2e/community.cjs
 * PLAYWRIGHT_BROWSERS_PATH can point at an isolated browser installation.
 */
const { openRoomMenu } = require('./room-menu.cjs');
const { installPwaFixture, checkPwa, collectPushFixtureEvidence } = require('./pwa-checks.cjs');
const { checkContactsBeforeRoom, checkSavedRoomsAcrossDevices } = require('./discovery-checks.cjs');
const { privateAttachmentChecks, publicAttachmentChecks } = require('./attachment-checks.cjs');
const { checkNotificationPreferences } = require('./notification-checks.cjs');
const {
  checkAccountPrivacyAcrossDevices,
  checkFollowSpeakerPreference,
} = require('./privacy-follow-checks.cjs');
const { checkRoomAdministration } = require('./room-administration-checks.cjs');
const {
  checkColdPhoneStartup,
  checkHomepageAccountUpdates,
  checkAccountNavigation,
  rejectFirstMediaChunk,
  checkInRoomBackNavigation,
} = require('./app-navigation-checks.cjs');
const { sheet, sheetRow, sheetScreen, closeSheet } = require('./settings-sheet.cjs');
const { participantHovercardChecks } = require('./participant-hovercard-checks.cjs');
const assert = require('node:assert/strict');
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
/** Runs one serial membership journey; browser/service ownership may be supplied by the test runner. */
async function run(runner = {}) {
  const env = runner.env ?? process.env;
  if (env.COMMUNITY_E2E !== '1')
    throw new Error('Set COMMUNITY_E2E=1 against a disposable local server/database.');
  const base = env.BASE_URL || 'http://127.0.0.1:3109';
  if (!['localhost', '127.0.0.1', '[::1]'].includes(new URL(base).hostname))
    throw new Error('This smoke is restricted to local test servers.');
  const { browserOptions, webKitTestNetworkEnabled } = require('./browser-options.cjs');
  const { collectPeerDiagnostics } = require('./peer-diagnostics.cjs');
  const { installPeerEventTracing } = require('./peer-events.cjs');
  const { installSignalingReconnectObservation } = require('./signaling-reconnect.cjs');
  const { installRelayPolicy, waitForRelayPaths } = require('./turn-relay.cjs');
  const { openAccountMenu, accountMenuButton, signOut } = require('./account-menu.cjs');
  const turnRelay = env.TURN_E2E === '1';
  const options = browserOptions(env.E2E_BROWSER);
  const webKitTestNetwork = webKitTestNetworkEnabled(options.name);
  const playwright = require(env.PLAYWRIGHT_MODULE || 'playwright');
  const artifacts =
    runner.artifacts ||
    env.E2E_ARTIFACTS ||
    fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-community-e2e.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const runId = `e2e-${Date.now().toString(36)}`;
  const password = 'Disposable-browser-password-2026!';
  const clients = [];
  const additionalBrowsers = [];
  let browser;
  let activeStep = 'launch';
  const report = {
    runId,
    browser: options.name,
    browserVersion: null,
    playwrightVersion: require(
      path.join(
        path.dirname(require.resolve(env.PLAYWRIGHT_MODULE || 'playwright')),
        'package.json',
      ),
    ).version,
    platform: process.platform,
    arch: process.arch,
    baseUrl: base,
    launchOptions: options.launchOptions,
    contextOptions: options.contextOptions,
    announcedIp: env.TEST_ANNOUNCE_IP || null,
    turnRelay,
    startedAt: new Date().toISOString(),
    complete: false,
    passed: false,
    steps: [],
    playbackRecoveries: [],
    signalingReconnects: [],
    limitations: [
      'Push transport and permission use controlled fixtures; real OS notification delivery remains a manual device check.',
      'Fake capture devices; not physical device or permission-prompt coverage.',
      'External capture termination is simulated with stop() plus an ended event on an owned fake local-stream track.',
      'Mobile checks resize a desktop viewport; they do not run a mobile browser.',
      'Signaling recovery closes an owned WebSocket only; it is not a UDP outage or reconnect-grace-expiry test.',
      'Interrupted setup closes a separate routed signaling connection after real transport creation; protocol replies and native RTC are not synthesized.',
      turnRelay
        ? 'TURN uses an owned local coturn relay and native relay-only ICE; no external NAT, TLS relay, or physical network outage is exercised.'
        : 'ICE restarts use real legacy commands and native credential changes; no UDP failure or TURN relay is exercised.',
    ],
  };
  // All fixture accounts share one loopback address. Keep their combined workload
  // within the real server guards instead of weakening limits or retrying writes.
  // Limits match the harness's clean server environment (xtask test-server) and router defaults.
  const HTTP_WINDOW_MS = 61_000;
  const MAX_HTTP_PACING_MS = 90_000;
  const httpLanes = new Map(
    [
      ['auth', 300, 100],
      ['account', 300, 100],
      ['rooms', 120, 40],
      ['profiles', 600, 100],
      ['telemetry', 30, 10],
    ].map(([name, limit, reserve]) => [name, { limit, reserve, requests: [] }]),
  );
  const httpCounts = new Map();
  const httpRoute = (request) => classifyCommunityHttpRoute(request, base, httpCounts);
  const httpPacing = [];
  let httpObserved = 0;
  let httpWaitMs = 0;
  const { testNetworkReady, retireTestNetwork } = createTestNetworkOwnership(clients);
  const step = createCommunityStep({
    report,
    paceHttpRequests,
    testNetworkReady,
    saveReport,
    get observedHttpRequests() {
      return httpObserved;
    },
    setActiveStep(name) {
      activeStep = name;
    },
    runStep: runner.step,
  });

  function observeHttpRequest(request) {
    const route = httpRoute(request);
    if (!route) return;
    httpObserved++;
    route.count.requests++;
    const requests = httpLanes.get(route.lane).requests;
    requests.push(Date.now());
    if (requests.length > 2000) requests.shift();
  }
  function observeHttpResponse(response) {
    const route = httpRoute(response.request());
    if (!route) return;
    const status = response.status();
    route.count.statuses[status] = (route.count.statuses[status] ?? 0) + 1;
  }
  async function paceHttpRequests() {
    let pacing;
    for (;;) {
      assert.ok(httpObserved <= 5000, 'the owned HTTP workload must stay bounded');
      const now = Date.now();
      const recent = {};
      let wait = 0;
      for (const [name, lane] of httpLanes) {
        while (lane.requests.length && now - lane.requests[0] >= HTTP_WINDOW_MS)
          lane.requests.shift();
        recent[name] = lane.requests.length;
        const target = lane.limit - lane.reserve;
        if (lane.requests.length > target)
          wait = Math.max(
            wait,
            lane.requests[lane.requests.length - target - 1] + HTTP_WINDOW_MS - now,
          );
      }
      if (wait <= 0) return;
      if (!pacing) {
        pacing = { beforeStep: activeStep, recent, waitedMs: 0 };
        httpPacing.push(pacing);
        console.log(`PACE shared HTTP workload before ${activeStep}: ${JSON.stringify(recent)}`);
      }
      const delay = Math.min(wait, 30_000);
      assert.ok(
        httpWaitMs + delay <= MAX_HTTP_PACING_MS,
        'HTTP scenario pacing exceeds its 90-second workload bound',
      );
      saveReport();
      await new Promise((resolve) => setTimeout(resolve, delay));
      pacing.waitedMs += delay;
      httpWaitMs += delay;
    }
  }

  function saveReport() {
    report.httpWorkload = {
      observation:
        'Native browser requests; controlled Push fetch is reported separately. Direct fixture API calls and helper tabs use reserved headroom.',
      windowMs: HTTP_WINDOW_MS,
      lanes: Object.fromEntries(
        [...httpLanes].map(([name, lane]) => [name, { limit: lane.limit, reserve: lane.reserve }]),
      ),
      observedRequests: httpObserved,
      pacingMs: httpWaitMs,
      pacing: httpPacing,
      endpoints: Object.fromEntries(
        [...httpCounts].sort(([left], [right]) => left.localeCompare(right)),
      ),
    };
    report.webKitTestNetwork = {
      enabled: webKitTestNetwork,
      pages: clients
        .filter((item) => item.testNetwork)
        .map((item) => ({
          label: item.label,
          setting: item.testNetwork.setting,
          value: item.testNetwork.value,
          playwrightVersion: item.testNetwork.playwrightVersion,
          appliedCount: item.testNetwork.appliedCount(),
          retired: item.testNetworkRetired === true,
        })),
    };
    fs.writeFileSync(
      path.join(artifacts, 'community-results.json'),
      `${JSON.stringify(report, null, 2)}\n`,
      { mode: 0o600 },
    );
  }

  async function deadline(work, milliseconds = 5000) {
    let timer;
    try {
      return await Promise.race([
        work,
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new Error('Browser diagnostics deadline exceeded')),
            milliseconds,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }
  async function savedPrivateRowCount(sender, recipient, clientMessageId) {
    assert.equal(env.DISPOSABLE_TEST_DATABASE, '1');
    let database;
    try {
      database = new URL(env.TEST_DATABASE_URL);
    } catch {
      throw new Error('Saved PM verification requires an owned test database URL');
    }
    assert.ok(
      ['postgres:', 'postgresql:'].includes(database.protocol) &&
        ['localhost', '127.0.0.1', '[::1]'].includes(database.hostname) &&
        /^\/[A-Za-z0-9_]+_test$/.test(database.pathname) &&
        ['', '?sslmode=disable'].includes(database.search) &&
        !database.hash,
      'Saved PM verification is restricted to a loopback test database',
    );
    for (const id of [sender, recipient, clientMessageId])
      assert.match(id, /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i);
    const count = await new Promise((resolve, reject) => {
      const child = execFile(
        'psql',
        [
          '-X',
          '--no-password',
          '--tuples-only',
          '--no-align',
          '--quiet',
          '--set=ON_ERROR_STOP=1',
          `--set=sender=${sender}`,
          `--set=recipient=${recipient}`,
          `--set=attempt=${clientMessageId}`,
          '--file=-',
        ],
        {
          env: {
            PATH: env.PATH,
            LC_ALL: 'C',
            PGHOST: database.hostname.replace(/^\[|\]$/g, ''),
            PGPORT: database.port || '5432',
            PGDATABASE: database.pathname.slice(1),
            PGUSER: decodeURIComponent(database.username),
            PGPASSWORD: decodeURIComponent(database.password),
            PGSSLMODE: 'disable',
            PGCONNECT_TIMEOUT: '5',
            PGPASSFILE: '/dev/null',
            PGSERVICEFILE: '/dev/null',
            PGOPTIONS: '-c default_transaction_read_only=on -c statement_timeout=5000',
          },
          timeout: 10000,
          maxBuffer: 1024,
        },
        (error, stdout) => {
          if (error) reject(new Error('Owned saved PM row verification failed'));
          else resolve(stdout.trim());
        },
      );
      child.stdin.on('error', () => reject(new Error('Owned saved PM query input failed')));
      child.stdin.end(
        "SELECT count(*) FROM chat_messages WHERE room_id IS NULL AND sender_account=:'sender'::uuid AND recipient_account=:'recipient'::uuid AND client_message_id=:'attempt';\n",
      );
    });
    assert.match(count, /^\d+$/);
    return Number(count);
  }

  async function client(label, mobile = false, configureContext, independentCapture = false) {
    let clientBrowser = browser;
    if (independentCapture) {
      // WebKit silences existing capture when another tab starts using devices.
      // An independent browser represents a second caller without that conflict.
      clientBrowser = await (runner.launchBrowser
        ? runner.launchBrowser(options.name, options.launchOptions)
        : playwright[options.name].launch(options.launchOptions));
      additionalBrowsers.push(clientBrowser);
    }
    const context = await clientBrowser.newContext({
      ...options.contextOptions,
      viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 },
    });
    await configureContext?.(context);
    const page = await context.newPage();
    const entry = { label, context, page, errors: [], frames: [], warnings: [] };
    clients.push(entry);
    page.on('request', observeHttpRequest);
    page.on('response', observeHttpResponse);
    page.on('requestfailed', (request) => {
      const route = httpRoute(request);
      if (route) route.count.failed++;
    });
    if (webKitTestNetwork) {
      const { installWebKitTestNetwork } = require('./webkit-test-network.cjs');
      entry.testNetwork = await installWebKitTestNetwork(playwright, page, {
        browserName: options.name,
      });
    }
    // Every caller uses the same controlled push transport. Account rules still
    // use the real server; native OS push delivery remains a manual device check.
    await installPwaFixture(page);
    page.setDefaultTimeout(10000);
    if (turnRelay) await page.addInitScript(installRelayPolicy);
    // Observe native peer connections for failure diagnostics; no SDP/media mocking.
    await page.addInitScript(installPeerEventTracing, {
      announcedIp: env.TEST_ANNOUNCE_IP || null,
    });
    await page.addInitScript(installSignalingReconnectObservation);
    await page.addInitScript(installCommunityCaptureObservation);
    page.on('pageerror', (error) => entry.errors.push(error.stack || error.message));
    page.on('console', (message) => {
      if (['warning', 'error'].includes(message.type())) {
        entry.warnings.push(message.text());
        if (entry.warnings.length > 40) entry.warnings.shift();
      }
    });
    page.on('websocket', (socket) => {
      for (const direction of ['framesent', 'framereceived'])
        socket.on(direction, (frame) => {
          try {
            const message = JSON.parse(String(frame.payload));
            entry.frames.push({
              direction,
              type: message.type,
              content: message.content,
              message:
                typeof message.message === 'string' ? message.message : message.message?.content,
            });
            if (entry.frames.length > 40) entry.frames.shift();
          } catch {}
        });
    });
    page.on('dialog', (dialog) => dialog.dismiss());
    await navigateCommunityPage(page, base, clients);
    assert.deepEqual(entry.errors, [], `${label} startup page errors`);
    return page;
  }
  async function visible(page, text) {
    await page
      .getByText(text, { exact: true })
      .filter({ visible: true })
      .last()
      .waitFor({ state: 'visible' });
  }
  async function remotePlayback(page, kind) {
    await page.waitForFunction(
      (mediaKind) => {
        const playing = [...document.querySelectorAll(`.video-tile:not(.local) ${mediaKind}`)].some(
          (element) =>
            !element.paused &&
            (mediaKind === 'video'
              ? element.videoWidth > 0
              : element.readyState >= 2 && !element.muted && element.volume > 0),
        );
        const prompt = [...document.querySelectorAll('.personal-playback-blocked')].some(
          (notice) => !notice.hidden,
        );
        return playing || prompt;
      },
      kind,
      { timeout: 15000 },
    );
    const retry = page.locator('[data-control="retry-playback"]').filter({ visible: true });
    if (await retry.count()) {
      await retry.first().click();
      report.playbackRecoveries.push({ kind, step: activeStep });
    }
    await page.waitForFunction(
      (mediaKind) =>
        [...document.querySelectorAll(`.video-tile:not(.local) ${mediaKind}`)].some(
          (element) =>
            !element.paused &&
            (mediaKind === 'video'
              ? element.videoWidth > 0
              : element.readyState >= 2 && !element.muted && element.volume > 0),
        ),
      kind,
      { timeout: 15000 },
    );
    await page.waitForFunction(() =>
      [...document.querySelectorAll('.personal-playback-blocked')].every((notice) => notice.hidden),
    );
  }
  async function connected(page) {
    await page.locator('#room-screen').waitFor({ state: 'visible' });
    await page.waitForFunction(
      () => document.querySelector('#connection-status').textContent === 'Connected',
    );
    await page
      .getByRole('navigation', { name: 'Conversations', exact: true })
      .waitFor({ state: 'visible' });
  }

  async function signalingReconnect(interrupted, publisher, receiver, direction) {
    const evidence = { direction, passed: false };
    report.signalingReconnects.push(evidence);
    saveReport();
    const identities = [];
    try {
      await deadline(
        (async () => {
          identities.push(await reconnectMediaIdentity(publisher));
          identities.push(await reconnectMediaIdentity(receiver));
          await waitForMediaStart(receiver, identities[1]);
          evidence.before = await advancingReconnectMedia(receiver, identities[1], 3000);
          const baseline = await interrupted.evaluate(() =>
            window.__communitySignalingReconnect.snapshot(),
          );
          const closedAt = performance.now();
          const requested = await interrupted.evaluate(() =>
            window.__communitySignalingReconnect.closeCurrent(),
          );
          await interrupted.waitForFunction(
            (ordinal) => {
              const snapshot = window.__communitySignalingReconnect.snapshot();
              return snapshot.events.some(
                (event) => event.event === 'close' && event.socketOrdinal === ordinal,
              );
            },
            requested.socketOrdinal,
            { timeout: 1500 },
          );
          const ensureGap = async () => {
            const snapshot = await interrupted.evaluate(() =>
              window.__communitySignalingReconnect.snapshot(),
            );
            assert.equal(
              snapshot.openSocketOrdinals.length,
              0,
              'Media sample must finish before signaling reopens',
            );
            assert.equal(
              snapshot.counters.receivedReconnectResult,
              baseline.counters.receivedReconnectResult,
              'Media sample must precede any reconnect result',
            );
            assert.equal(
              snapshot.events.filter((event) => event.event === 'open').length,
              baseline.events.filter((event) => event.event === 'open').length,
              'No recovery socket may have opened during the outage sample',
            );
          };
          evidence.during = await advancingReconnectMedia(receiver, identities[1], 1000, ensureGap);
          for (const identity of identities) await identity.evaluate((state) => state.verify());
          await interrupted.waitForFunction(
            (previous) => {
              const snapshot = window.__communitySignalingReconnect.snapshot();
              return (
                snapshot.counters.reconnectSuccess > previous.reconnectSuccess &&
                snapshot.counters.receivedRoomSnapshot > previous.receivedRoomSnapshot &&
                snapshot.openSocketOrdinals.length === 1
              );
            },
            baseline.counters,
            { timeout: Math.max(1, 10000 - (performance.now() - closedAt)) },
          );
          // Host-observed resume + room snapshot, including automation overhead;
          // separate from the following decoded-media progress check.
          evidence.resumeObservedMs = performance.now() - closedAt;
          await connected(interrupted);
          evidence.after = await advancingReconnectMedia(receiver, identities[1], 3000);
          evidence.identities = [];
          for (const identity of identities)
            evidence.identities.push(await identity.evaluate((state) => state.verify()));
          const restored = await interrupted.evaluate(() =>
            window.__communitySignalingReconnect.snapshot(),
          );
          const delta = Object.fromEntries(
            Object.entries(restored.counters).map(([key, value]) => [
              key,
              value - baseline.counters[key],
            ]),
          );
          for (const key of [
            'sentJoinRoom',
            'sentCreateSendTransport',
            'sentCreateRecvTransport',
            'sentProduce',
            'reconnectFailure',
          ])
            assert.equal(delta[key], 0, `Signaling recovery must not invoke ${key}`);
          for (const key of [
            'sentReconnect',
            'receivedReconnectResult',
            'reconnectSuccess',
            'sentGetRoomSnapshot',
            'receivedRoomSnapshot',
          ])
            assert.equal(delta[key], 1, `Expected one successful signaling recovery: ${key}`);
          assert.equal(
            restored.socketsObserved - baseline.socketsObserved,
            1,
            'Expected exactly one replacement signaling socket',
          );
          evidence.counters = delta;
          const other = interrupted === publisher ? receiver : publisher;
          await publicChat(interrupted);
          await publicChat(other);
          const message = `${runId}-reconnected-${direction}`;
          await send(interrupted, message);
          await visible(other, message);
          evidence.passed = true;
          saveReport();
        })(),
        25000,
      );
    } finally {
      await deadline(
        Promise.all(identities.map((identity) => identity.dispose().catch(() => {}))),
      ).catch(() => {});
    }
  }
  async function offlineViewerControls(publisher, receiver) {
    const identities = [];
    try {
      identities.push(await reconnectMediaIdentity(publisher));
      // Explicitly hiding a broadcast retires its video consumer. Only this
      // scenario permits replacing incoming video; audio, capture and peers stay.
      identities.push(
        await reconnectMediaIdentity(receiver, { allowIncomingVideoReplacement: true }),
      );
      const before = await advancingReconnectMedia(receiver, identities[1], 3000);
      const controls = receiver.locator('.personal-media-controls').first();
      if (!(await controls.evaluate((element) => element.open)))
        await controls.locator('summary').click();
      const baseline = await receiver.evaluate(() =>
        window.__communitySignalingReconnect.snapshot(),
      );
      await receiver.getByRole('button', { name: 'Hide for me', exact: true }).first().click();
      // Both server consumers must be paused before the socket closes; local
      // playback alone cannot prove that the subsequent resume reached the server.
      await receiver.waitForFunction(
        (previous) =>
          window.__communitySignalingReconnect.snapshot().counters.receivedConsumerPaused >=
          previous + 2,
        baseline.counters.receivedConsumerPaused,
      );
      await receiver.waitForFunction(
        () => document.querySelectorAll('.video-tile:not(.local) video').length === 0,
      );
      const hidden = await receiver.evaluate(() => window.__communitySignalingReconnect.snapshot());
      const requested = await receiver.evaluate(() =>
        window.__communitySignalingReconnect.closeCurrent(),
      );
      await receiver.waitForFunction(
        (ordinal) => {
          const state = window.__communitySignalingReconnect.snapshot();
          return (
            state.openSocketOrdinals.length === 0 &&
            state.events.some((event) => event.event === 'close' && event.socketOrdinal === ordinal)
          );
        },
        requested.socketOrdinal,
        { timeout: 1500 },
      );
      await receiver
        .getByRole('button', { name: 'Restore broadcast', exact: true })
        .first()
        .click();
      const changedOffline = await receiver.evaluate(() =>
        window.__communitySignalingReconnect.snapshot(),
      );
      assert.equal(
        changedOffline.openSocketOrdinals.length,
        0,
        'Restore must occur while signaling is offline',
      );
      assert.equal(
        changedOffline.counters.receivedConsumerResumed,
        hidden.counters.receivedConsumerResumed,
      );
      await receiver.waitForFunction(
        (previous) => {
          const state = window.__communitySignalingReconnect.snapshot();
          return (
            state.counters.reconnectSuccess > previous.reconnectSuccess &&
            state.counters.receivedRoomSnapshot > previous.receivedRoomSnapshot &&
            state.counters.receivedConsumerResumed >= previous.receivedConsumerResumed + 2
          );
        },
        hidden.counters,
        { timeout: 10000 },
      );
      await connected(receiver);
      await remotePlayback(receiver, 'video');
      // An accepted resume precedes renewed RTP. Unlike the continuous-media
      // reconnect check, this scenario deliberately paused both server consumers.
      await waitForMediaStart(receiver, identities[1]);
      const after = await advancingReconnectMedia(receiver, identities[1], 3000);
      for (const identity of identities) await identity.evaluate((state) => state.verify());
      report.offlineViewerControls = { passed: true, before, after };
    } finally {
      for (const identity of identities) await identity.dispose();
    }
  }
  async function register(page, email, name) {
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-to-register').click();
    await page.locator('#register-email').fill(email);
    await page.locator('#register-name').fill(name);
    await page.locator('#register-password').fill(password);
    await page.locator('#register-confirm').fill(password);
    await page.locator('#register-submit').click();
    await page.locator('#register-modal').waitFor({ state: 'hidden' });
    await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
  }
  async function login(page, email, value) {
    if (!(await page.locator('#login-modal').isVisible()))
      await page.locator('#sign-in-btn').click();
    await page.locator('#login-email').fill(email);
    await page.locator('#login-password').fill(value);
    await page.locator('#login-submit').click();
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
  }
  const lastJoin = new WeakMap();
  // The server admits at most 10 joins per address into one room per minute, and
  // every client here shares one address: joins pace themselves against that limit,
  // counting the room's creation and the rejoin an interrupted setup makes by itself.
  const JOIN_LIMIT = 10;
  const JOIN_WINDOW_MS = 61_000;
  const joinTimes = [];
  function recordJoin() {
    joinTimes.push(Date.now());
  }
  async function paceJoins(page) {
    while (joinTimes.length && Date.now() - joinTimes[0] >= JOIN_WINDOW_MS) joinTimes.shift();
    if (joinTimes.length < JOIN_LIMIT) return;
    await page.waitForTimeout(joinTimes[0] + JOIN_WINDOW_MS - Date.now());
    joinTimes.shift();
  }
  async function join(page, name) {
    const remaining = 1100 - (Date.now() - (lastJoin.get(page) || 0));
    if (remaining > 0) await page.waitForTimeout(remaining);
    await paceJoins(page);
    await page.locator('#name-input').fill(name);
    await page.locator('#room-input').fill(runId);
    await page.locator('#join-btn').click();
    lastJoin.set(page, Date.now());
    recordJoin();
    await connected(page);
  }
  async function leave(page) {
    await page.locator('#leave-btn').click();
    await page.locator('#join-screen').waitFor({ state: 'visible' });
  }
  const lastSend = new WeakMap();
  async function submitMessage(page) {
    // Human-paced messages preserve the server's 2/s chat flood protection.
    const remaining = 600 - (Date.now() - (lastSend.get(page) || 0));
    if (remaining > 0) await page.waitForTimeout(remaining);
    await page.locator('#chat-input').press('Enter');
    lastSend.set(page, Date.now());
  }
  async function send(page, text) {
    await page.locator('#chat-input').fill(text);
    await submitMessage(page);
  }
  function privateConversation(page, name) {
    return page.getByRole('dialog', { name: `Messages with ${name}`, exact: true });
  }
  async function sendPrivate(conversation, text) {
    const composer = conversation.getByLabel('Private message', { exact: true });
    await composer.fill(text);
    await composer.press('Enter');
  }
  async function dismissUnconfirmed(page, conversation) {
    await conversation
      .getByRole('button', { name: 'Dismiss unconfirmed send', exact: true })
      .click();
    const confirmation = page.getByRole('dialog', {
      name: 'Dismiss unconfirmed send?',
      exact: true,
    });
    await confirmation.getByRole('button', { name: 'Dismiss', exact: true }).click();
    await confirmation.waitFor({ state: 'hidden' });
  }
  async function publicChat(page) {
    await page.locator('.conversation-tab[data-conversation-id="public"]').click();
  }
  async function action(page, name, label) {
    const buttons = page.getByRole('button', { name: `Actions for ${name}`, exact: true });
    await buttons.filter({ visible: true }).first().click();
    await page.locator('#mod-menu').getByRole('button', { name: label, exact: true }).click();
  }
  async function header(page, name) {
    // Joined-room account and room actions share the More menu.
    const target = page.locator('#community-actions, #room-actions').getByRole('button', {
      name: name === 'Messages' ? /^Messages(?:, .* unread)?$/ : name,
      exact: name !== 'Messages',
    });
    if (!(await target.isVisible())) {
      if (await page.locator('#room-screen').isVisible()) await openRoomMenu(page);
      else await openAccountMenu(page);
    }
    await target.click();
    // Account and room management open the settings sheet; the rest are dialogs.
    if (name === 'Account' || name === 'Manage room') {
      const region = sheet(page);
      await region.waitFor({ state: 'visible' });
      return region;
    }
    return page.getByRole('dialog', { name, exact: true });
  }
  /** Opens the settings sheet on one screen, from a room or from the signed-in home page. */
  async function settingsScreen(page, label) {
    if (!(await sheet(page).isVisible())) {
      if (await page.locator('#room-screen').isVisible())
        await page.locator('#settings-btn').click();
      else await (await accountMenuButton(page, 'Account')).click();
    }
    return sheetScreen(page, label);
  }
  /** Whether a room action is offered, read from the menu and leaving it closed. */
  async function roomMenuOffers(page, name) {
    await openRoomMenu(page);
    await page
      .locator('#room-actions')
      .getByRole('button', { name, exact: true })
      .waitFor({ state: 'visible' });
    await page.keyboard.press('Escape');
  }
  /** Dialogs close with Close; the settings sheet closes from its own header. */
  async function close(dialog) {
    const page = dialog.page();
    const managed = await dialog.evaluate(
      (node) => node.id === 'settings-sheet' || node.classList.contains('community-dialog'),
    );
    const previous = managed
      ? await page.evaluate(() => history.state?.simplestchatNavigation)
      : null;
    await dialog
      .getByRole('button', { name: /^Close( settings)?$/ })
      .first()
      .click();
    await dialog.waitFor({ state: 'hidden' });
    if (typeof previous === 'number')
      await page.waitForFunction(
        (entry) => history.state?.simplestchatNavigation !== entry,
        previous,
      );
  }
  async function preferences(page, allow) {
    await page.getByRole('button', { name: 'Chat options', exact: true }).click();
    const privacy = await sheetScreen(page, 'Privacy');
    const toggle = privacy.getByLabel('Private messages from others', { exact: true });
    if ((await toggle.isChecked()) !== allow) {
      const stored = page.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === '/api/auth/preferences' &&
          response.request().method() === 'PATCH',
      );
      await toggle.setChecked(allow);
      await stored;
    }
    await closeSheet(page);
  }
  async function roomHistory(page) {
    await page.getByRole('button', { name: 'Chat options', exact: true }).click();
    const look = await sheetScreen(page, 'Chat look');
    await look.getByRole('button', { name: 'Room history', exact: true }).click();
    const history = page.getByRole('dialog', { name: 'Room history', exact: true });
    await history.waitFor({ state: 'visible' });
    return history;
  }
  async function chatEnabled(page, value) {
    await page.waitForFunction(
      (expected) => document.querySelector('#chat-input').disabled === !expected,
      value,
    );
  }
  const ACCESS_CONTROLS = [
    '#rs-guests',
    '#rs-require-reg',
    '#rs-invite-only',
    '#rs-secret',
    '#rs-lobby',
  ];
  async function roomSetting(page, id, checked) {
    await page.locator('#room-settings-btn').click();
    const region = await sheetScreen(
      page,
      ACCESS_CONTROLS.includes(id) ? 'Who can enter' : 'Chat & broadcasting',
    );
    const control = region.locator(id);
    const changed = (await control.isChecked()) !== checked;
    await control.setChecked(checked);
    if (changed) {
      // A switch updates before its correlated server acknowledgement. Closing
      // early would leave the next change blocked by the pending save.
      await page.waitForFunction(
        () =>
          document.querySelector('#settings-sheet .sheet-screen')?.getAttribute('aria-busy') ===
          'false',
      );
      assert.equal(
        await region.locator('#room-settings-result').textContent(),
        'Change saved',
        `${id} must be acknowledged before the scenario continues`,
      );
    }
    assert.equal(await control.isChecked(), checked, `${id} must retain the requested value`);
    await closeSheet(page);
  }
  async function dialogFitsViewport(page, dialog, footer) {
    const fit = await dialog.evaluate((node) => {
      const box = node.getBoundingClientRect();
      return (
        document.documentElement.scrollWidth <= innerWidth + 1 &&
        node.scrollWidth <= node.clientWidth + 1 &&
        box.left >= 0 &&
        box.top >= 0 &&
        box.right <= innerWidth + 1 &&
        box.bottom <= innerHeight + 1
      );
    });
    assert.equal(fit, true, 'page and dialog fit the viewport without horizontal overflow');
    await footer.scrollIntoViewIfNeeded();
    await footer.click({ trial: true });
    assert.equal(
      await footer.evaluate((node) => {
        const box = node.getBoundingClientRect();
        return (
          box.left >= 0 &&
          box.top >= 0 &&
          box.right <= innerWidth + 1 &&
          box.bottom <= innerHeight + 1
        );
      }),
      true,
      'dialog footer is reachable',
    );
  }
  async function upload(page, dialog, room = false) {
    const data = await page.evaluate(() => {
      const canvas = document.createElement('canvas');
      canvas.width = 64;
      canvas.height = 64;
      const ctx = canvas.getContext('2d');
      ctx.fillStyle = '#6644cc';
      ctx.fillRect(0, 0, 64, 64);
      return canvas.toDataURL('image/png').split(',')[1];
    });
    await dialog
      .getByLabel(room ? 'Room image (PNG, JPEG or WebP)' : 'Avatar (PNG, JPEG or WebP)', {
        exact: true,
      })
      .setInputFiles({
        name: 'test-raster.png',
        mimeType: 'image/png',
        buffer: Buffer.from(data, 'base64'),
      });
    await dialog
      .getByRole('img', { name: room ? 'Room image preview' : 'Avatar preview', exact: true })
      .waitFor({ state: 'visible' });
  }
  async function setRole(owner, name, role) {
    const dialog = await header(owner, 'Manage room');
    const row = dialog
      .locator('.management-entry')
      .filter({ has: owner.getByRole('combobox', { name: `Role for ${name}`, exact: true }) });
    await row.getByRole('combobox').selectOption(role);
    await row.getByRole('button', { name: 'Update role', exact: true }).click();
    if (role === 'user') await row.waitFor({ state: 'detached' });
    else await dialog.getByText(new RegExp(`${name} · ${role}`)).waitFor();
    await close(dialog);
  }

  saveReport();
  let cleanupFailure;
  try {
    browser = await (runner.launchBrowser
      ? runner.launchBrowser(options.name, options.launchOptions)
      : playwright[options.name].launch(options.launchOptions));
    report.browserVersion = browser.version();
    saveReport();
    console.log(
      `Browser: ${options.name} ${report.browserVersion}; Playwright ${report.playwrightVersion}`,
    );
    await step('constrained cold phone homepage is ready without the call engine', async () => {
      report.coldPhoneStartup = await checkColdPhoneStartup({
        browser,
        browserName: options.name,
        contextOptions: options.contextOptions,
        base,
        ready: communityStartupReady,
      });
      if (report.coldPhoneStartup.skipped) return { skip: report.coldPhoneStartup.reason };
    });
    const owner = await client('owner');
    const member = await client('member');
    const guest = await client('guest');
    let secondDevice;
    let outsider;
    let contactAccounts;
    const ownerEmail = `${runId}-owner@example.test`;
    const memberEmail = `${runId}-member@example.test`;
    const inviteeEmail = `${runId}-invitee@example.test`;
    await step('register owner/member and accept contacts without a room', async () => {
      await register(owner, ownerEmail, 'E2E Owner');
      await register(member, memberEmail, 'E2E Member');
      contactAccounts = await checkContactsBeforeRoom(owner, member, { base, header, close });
    });
    await step(
      'homepage account events and typed destinations remain live without joining',
      async () => {
        const otherMember = await client('member-home-second-device');
        await login(otherMember, memberEmail, password);
        report.homepageAccountUpdates = await checkHomepageAccountUpdates(
          owner,
          member,
          otherMember,
          {
            header,
            close,
            settingsScreen,
            ...contactAccounts,
          },
        );
        report.accountNavigation = await checkAccountNavigation(owner, {
          base,
          header,
          close,
          memberId: contactAccounts.memberId,
        });
        // This device's cross-device assertions are complete. Later room
        // scenarios use their own devices and need no idle account listener.
        await retireTestNetwork(otherMember);
        await otherMember.context().close();
      },
    );
    await step('create room, retry a failed media chunk, and guest join', async () => {
      outsider = await client('attachment-outsider');
      await register(
        outsider,
        `${runId}-attachment-outsider@example.test`,
        'E2E Attachment Outsider',
      );
      await privateAttachmentChecks(owner, member, outsider, { base, header, close, deadline });
      const retryMediaChunk = await rejectFirstMediaChunk(owner, {
        join,
        send,
        visible,
        ready: communityStartupReady,
      });
      await owner.locator('#create-room-btn').click();
      await owner.locator('#cr-id').fill(runId);
      await owner.locator('#cr-name').fill('E2E Community Room');
      await owner.locator('#create-room-submit').click();
      recordJoin();
      await connected(owner);
      report.deferredMediaRetry = await retryMediaChunk();
      await join(member, 'E2E Room Nickname');
      await join(guest, 'E2E Guest');
      await send(guest, 'Public hello');
      await visible(owner, 'Public hello');
      await visible(member, 'Public hello');
    });
    await step('browser Back and Forward pop account views while preserving the call', async () => {
      report.inRoomNavigation = await checkInRoomBackNavigation(owner, {
        header,
        close,
        settingsScreen,
      });
    });
    await step(
      'favorite rooms and accepted contacts follow an account to a second device',
      async () => {
        secondDevice = await client('owner-second-device');
        await login(secondDevice, ownerEmail, password);
        await checkSavedRoomsAcrossDevices(owner, secondDevice, { header, close });
        await checkNotificationPreferences(owner, secondDevice, member, {
          header,
          close,
          settingsScreen,
        });
      },
    );
    await step(
      'account privacy merges field changes and recovers a committed response loss',
      async () => {
        report.accountPrivacy = await checkAccountPrivacyAcrossDevices(owner, secondDevice, {
          settingsScreen,
          header,
          close,
          peerName: 'E2E Member',
        });
      },
    );
    await step('Follow speaker is explicit, persistent and capture-free', async () => {
      report.followSpeaker = await checkFollowSpeakerPreference(owner, { base, artifacts });
    });
    await step('home manages an owned room without moving the active call', async () => {
      report.roomAdministration = await checkRoomAdministration(owner, secondDevice, {
        base,
        runId,
        client,
        header,
        close,
        connected,
        paceJoins,
        recordJoin,
      });
    });
    await step('optional app notifications require an explicit Account action', async () => {
      report.pushFixture = await checkPwa(owner, {
        openAccount: () => settingsScreen(owner, 'Notifications'),
        closeDialog: close,
      });
    });
    await step('direct join respects the entered name without renaming the account', async () => {
      await owner
        .getByRole('button', { name: 'Actions for E2E Room Nickname', exact: true })
        .first()
        .waitFor({ state: 'visible' });
      const account = await header(member, 'Account');
      assert.equal(
        await account.getByLabel('Name', { exact: true }).inputValue(),
        'E2E Member',
        'the direct-join nickname leaves the account profile unchanged',
      );
      await close(account);
      await header(member, 'Nickname');
      const dialog = member.getByRole('dialog', { name: 'Room nickname', exact: true });
      assert.equal(
        await dialog.getByLabel('Nickname', { exact: true }).inputValue(),
        'E2E Room Nickname',
      );
      await dialog.getByLabel('Nickname', { exact: true }).fill('E2E Member');
      await dialog.getByRole('button', { name: 'Change nickname', exact: true }).click();
      await dialog.waitFor({ state: 'hidden' });
      await owner
        .getByRole('button', { name: 'Actions for E2E Member', exact: true })
        .first()
        .waitFor({ state: 'visible' });
    });
    await step('participant hovercards work from roster and chat names', async () => {
      await participantHovercardChecks(owner, { name: 'E2E Guest', message: 'Public hello' });
    });
    await step(
      'a roster account starts its first saved conversation from the current room',
      async () => {
        await join(outsider, 'E2E Attachment Outsider');
        await action(owner, 'E2E Attachment Outsider', 'Private message');
        const conversation = privateConversation(owner, 'E2E Attachment Outsider');
        const accepted = owner.waitForResponse(
          (response) =>
            response.request().method() === 'POST' &&
            /^\/api\/auth\/inbox\/[^/]+\/messages$/.test(new URL(response.url()).pathname),
        );
        await sendPrivate(conversation, 'First PM from a shared room');
        const response = await accepted;
        assert.equal(response.status(), 200);
        assert.equal(response.request().postDataJSON().roomId, runId);
        assert.equal((await response.json()).accountConversation, true);
        await conversation.getByText('First PM from a shared room', { exact: true }).waitFor();
        await close(conversation);
        await leave(outsider);
      },
    );
    await step(
      'authors edit public messages and moderators pin them in the compact chat controls',
      async () => {
        for (const page of [owner, member, guest]) await publicChat(page);
        await send(owner, 'Message before correction');
        await visible(member, 'Message before correction');
        // The recipient can receive the broadcast before the sender receives its
        // acknowledgement. Only the confirmed ID remains stable through editing.
        const original = owner.locator('.chat-msg:not([data-message-id^="pending:"])').filter({
          has: owner.locator('.msg-text').filter({ hasText: /^Message before correction$/ }),
        });
        const messageId = await original.getAttribute('data-message-id');
        assert.ok(messageId);
        const own = owner.locator(`.chat-msg[data-message-id="${messageId}"]`);
        await own.hover();
        await own.getByRole('button', { name: 'Edit', exact: true }).click();
        const edit = owner.getByRole('dialog', { name: 'Edit message', exact: true });
        await edit.getByLabel('Message', { exact: true }).fill('Corrected public message');
        await edit.getByRole('button', { name: 'Save changes', exact: true }).click();
        await edit.waitFor({ state: 'hidden' });
        await visible(member, 'Corrected public message');
        await own.locator('.msg-edited').waitFor({ state: 'visible' });
        await own.hover();
        await own.getByRole('button', { name: 'Pin', exact: true }).click();
        const toggle = member.getByRole('button', { name: 'Pinned (1)', exact: true });
        await toggle.waitFor({ state: 'visible' });
        assert.equal(await member.locator('.chat-pinned-messages').isVisible(), false);
        await toggle.click();
        await member
          .locator('.chat-pinned-link')
          .filter({ hasText: 'Corrected public message' })
          .waitFor({ state: 'visible' });
        assert.equal(
          await member
            .locator('.chat-pinned-messages')
            .getByRole('button', { name: 'Unpin', exact: true })
            .count(),
          0,
        );
        await own.hover();
        await own.getByRole('button', { name: 'Unpin', exact: true }).click();
        await toggle.waitFor({ state: 'hidden' });
      },
    );
    await step(
      'public removal confirms, scrubs quotes and remains removed after reconnect replay',
      async () => {
        for (const page of [owner, member, guest]) await publicChat(page);
        await send(guest, 'Removal fixture original');
        await visible(member, 'Removal fixture original');
        const original = member.locator('.chat-msg').filter({
          has: member.locator('.msg-text').filter({ hasText: /^Removal fixture original$/ }),
        });
        const messageId = await original.getAttribute('data-message-id');
        assert.ok(messageId);
        assert.equal(
          await original.getByRole('button', { name: 'Remove message', exact: true }).count(),
          0,
        );
        await original.hover();
        await original.getByRole('button', { name: 'Reply', exact: true }).click();
        await send(member, 'Removal fixture reply');
        await owner
          .locator('.msg-reply')
          .filter({ hasText: 'Removal fixture original' })
          .waitFor({ state: 'visible' });
        const ownerRow = owner.locator(`[data-message-id="${messageId}"]`);
        await ownerRow.hover();
        await ownerRow.getByRole('button', { name: 'Remove message', exact: true }).click();
        let dialog = owner.getByRole('dialog', { name: 'Remove message?', exact: true });
        await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
        await visible(owner, 'Removal fixture original');
        await ownerRow.hover();
        await ownerRow.getByRole('button', { name: 'Remove message', exact: true }).click();
        dialog = owner.getByRole('dialog', { name: 'Remove message?', exact: true });
        await dialog.getByRole('button', { name: 'Remove message', exact: true }).click();
        await dialog.waitFor({ state: 'hidden' });
        const checkRemoved = async (page) => {
          await page
            .locator(`[data-message-id="${messageId}"] .msg-text`)
            .filter({ hasText: /^Message removed$/ })
            .waitFor({ state: 'visible' });
          await page
            .locator('.msg-reply')
            .filter({ hasText: 'Message removed' })
            .waitFor({ state: 'visible' });
          assert.equal(
            await page
              .locator('#chat-messages')
              .getByText('Removal fixture original', { exact: false })
              .count(),
            0,
          );
        };
        for (const page of [owner, member, guest]) await checkRemoved(page);
        const before = await guest.evaluate(
          () => window.__communitySignalingReconnect.snapshot().counters.receivedRoomSnapshot,
        );
        await guest.evaluate(() => window.__communitySignalingReconnect.closeCurrent());
        await guest.waitForFunction(
          (previous) =>
            window.__communitySignalingReconnect.snapshot().counters.receivedRoomSnapshot >
            previous,
          before,
        );
        await connected(guest);
        await checkRemoved(guest);
      },
    );
    await step(
      'saved public history supports retention, guest reading, search, removal and purge',
      async () => {
        let history = await roomHistory(owner);
        assert.equal(await history.locator('.history-retention select').inputValue(), '0');
        await history.locator('.history-retention select').selectOption('7');
        await history.getByRole('button', { name: 'Save retention', exact: true }).click();
        await history
          .getByRole('status')
          .filter({ hasText: 'Messages are kept for 7 days.' })
          .waitFor({ state: 'visible' });
        await close(history);
        await closeSheet(owner);
        await send(owner, 'Prejoin saved action fixture');
        await publicAttachmentChecks(owner, guest, { base, leave, join, send });
        assert.equal(
          await guest
            .locator('#chat-messages .chat-msg')
            .filter({ hasText: 'Prejoin saved action fixture' })
            .count(),
          0,
          'the new guest membership cannot replay an older public row',
        );
        history = await roomHistory(guest);
        const prejoin = history.locator('.history-message').filter({
          has: guest.locator('.msg-text').filter({ hasText: /^Prejoin saved action fixture$/ }),
        });
        await prejoin.waitFor();
        const prejoinId = await prejoin.getAttribute('data-message-id');
        assert.ok(prejoinId);
        await prejoin.getByRole('button', { name: 'React', exact: true }).click();
        await prejoin.getByRole('button', { name: '👍', exact: true }).click();
        await prejoin.getByRole('button', { name: '👍 1', exact: true }).waitFor();
        await close(history);
        await closeSheet(guest);
        history = await roomHistory(guest);
        const restored = history.locator(`.history-message[data-message-id="${prejoinId}"]`);
        await restored.getByRole('button', { name: '👍 1', exact: true }).waitFor();
        await restored.getByRole('button', { name: 'Reply', exact: true }).click();
        await history.waitFor({ state: 'hidden' });
        await sheet(guest).waitFor({ state: 'hidden' });
        await send(guest, 'Reply to a saved prejoin public message');
        const answered = owner.locator('#chat-messages .chat-msg').filter({
          has: owner
            .locator('.msg-text')
            .filter({ hasText: /^Reply to a saved prejoin public message$/ }),
        });
        await answered
          .locator('.msg-reply')
          .filter({ hasText: 'Prejoin saved action fixture' })
          .waitFor();
        await send(guest, 'Context before the search result');
        await send(guest, 'Saved browser history message');
        await send(guest, 'Context after the search result');
        await visible(owner, 'Saved browser history message');
        history = await roomHistory(guest);
        await history
          .getByText('Saved browser history message', { exact: true })
          .waitFor({ state: 'visible' });
        assert.equal(await history.locator('.history-retention select').count(), 0);
        assert.equal(await history.getByRole('button', { name: 'Remove', exact: true }).count(), 0);
        await history.getByLabel('Search messages', { exact: true }).fill('browser history');
        await history.getByRole('button', { name: 'Search', exact: true }).click();
        await history
          .getByRole('status')
          .filter({ hasText: 'Searching the newest 10,000 saved messages.' })
          .waitFor({ state: 'visible' });
        await history
          .getByText('Saved browser history message', { exact: true })
          .waitFor({ state: 'visible' });
        await history.getByRole('button', { name: 'Show in conversation', exact: true }).click();
        await history
          .getByText('Context before the search result', { exact: true })
          .waitFor({ state: 'visible' });
        await history
          .getByText('Context after the search result', { exact: true })
          .waitFor({ state: 'visible' });
        await close(history);
        await closeSheet(guest);
        await owner.getByRole('button', { name: 'Earlier messages', exact: true }).click();
        assert.equal(
          await owner.getByRole('dialog', { name: 'Room history', exact: true }).count(),
          0,
        );
        history = await roomHistory(owner);
        const saved = history
          .locator('.history-message')
          .filter({ hasText: 'Saved browser history message' });
        const savedMessageId = await saved.getAttribute('data-message-id');
        assert.ok(savedMessageId);
        await saved.getByRole('button', { name: 'Remove', exact: true }).click();
        const confirmation = owner.getByRole('dialog', { name: 'Remove message', exact: true });
        await confirmation.getByRole('button', { name: 'Remove message', exact: true }).click();
        await confirmation.waitFor({ state: 'hidden' });
        await history
          .locator(`.history-message[data-message-id="${savedMessageId}"]`)
          .getByText('Message removed', { exact: true })
          .waitFor({ state: 'visible' });
        await history.locator('.history-retention select').selectOption('0');
        await history.getByRole('button', { name: 'Save retention', exact: true }).click();
        await history
          .getByRole('status')
          .filter({ hasText: 'Room history is off.' })
          .waitFor({ state: 'visible' });
        assert.equal(await history.locator('.history-message').count(), 0);
        await close(history);
        await closeSheet(owner);
      },
    );
    await step('personal settings share one capture-free keyboard-accessible dialog', async () => {
      const captures = await guest.evaluate(() => window.__communityCaptureRequests);
      assert.equal(
        await guest.locator('#settings-modal').count(),
        0,
        'legacy settings dialog removed',
      );
      assert.equal(
        await guest.getByRole('button', { name: 'Media setup', exact: true }).count(),
        0,
        'no duplicate toolbar setup',
      );
      for (const selector of ['#settings-btn', '#mic-setup-btn']) {
        // Mic setup lives in More and opens Camera & mic directly; the main settings
        // button stays below the video and opens the list.
        const inMenu = selector === '#mic-setup-btn';
        if (inMenu) await openRoomMenu(guest);
        const opener = guest.locator(selector);
        const returnsTo = inMenu ? guest.locator('#room-more-btn') : opener;
        await opener.focus();
        await opener.press('Enter');
        const region = sheet(guest);
        await region.waitFor({ state: 'visible' });
        assert.equal(await guest.locator('dialog[open]').count(), 0, 'the sheet is not modal');
        const title = region.getByRole('heading', { level: 2 });
        assert.equal(await title.textContent(), inMenu ? 'Camera & mic' : 'Settings');
        assert.equal(
          await title.evaluate((node) => node === document.activeElement),
          true,
          'opening moves focus to the sheet heading',
        );
        if (inMenu) {
          // Advanced capture options wait behind the Quality row.
          assert.equal(await region.locator('[name=resolution]').count(), 0);
          await sheetRow(guest, 'Quality').waitFor({ state: 'visible' });
          await guest.keyboard.press('Escape');
          assert.equal(await title.textContent(), 'Settings', 'Escape pops to the list');
        }
        await guest.keyboard.press('Escape');
        await region.waitFor({ state: 'hidden' });
        assert.equal(
          await returnsTo.evaluate((node) => node === document.activeElement),
          true,
          'Escape restores the opening control',
        );
      }
      await guest.locator('#settings-btn').click();
      let region = await sheetScreen(guest, 'Camera & mic');
      const pressed = async (id) =>
        (await region.locator(`#${id}`).getAttribute('aria-pressed')) === 'true';
      const originalMode = (await pressed('talk-ptt')) ? 'ptt' : 'open';
      const nextMode = originalMode === 'open' ? 'ptt' : 'open';
      await region.locator(`#talk-${nextMode}`).click();
      assert.equal(
        await guest.evaluate(() => localStorage.getItem('micMode')),
        nextMode,
        'talk mode applies immediately',
      );
      await guest.screenshot({
        path: path.join(artifacts, 'settings-audio-desktop.png'),
        fullPage: true,
      });
      region = await sheetScreen(guest, 'Layout');
      const originalLayout = (await pressed('layout-modern')) ? 'modern' : 'classic';
      const nextLayout = originalLayout === 'modern' ? 'classic' : 'modern';
      await region.locator(`#layout-${nextLayout}`).click();
      assert.equal(
        await guest.evaluate(
          (value) =>
            localStorage.getItem('layout') === value &&
            document.querySelector('#room-screen').classList.contains(`layout-${value}`),
          nextLayout,
        ),
        true,
        'layout applies immediately',
      );
      await guest.screenshot({
        path: path.join(artifacts, 'settings-appearance-desktop.png'),
        fullPage: true,
      });
      await closeSheet(guest);
      await guest.locator('#settings-btn').click();
      region = await sheetScreen(guest, 'Camera & mic');
      assert.equal(await pressed(`talk-${nextMode}`), true, 'the talk mode survives reopening');
      await region.locator(`#talk-${originalMode}`).click();
      region = await sheetScreen(guest, 'Layout');
      assert.equal(await pressed(`layout-${nextLayout}`), true, 'the layout survives reopening');
      await region.locator(`#layout-${originalLayout}`).click();
      await closeSheet(guest);
      assert.equal(
        await guest.evaluate(() => window.__communityCaptureRequests),
        captures,
        'opening, navigating and immediate preferences never capture',
      );
    });
    await step(
      'room settings screens expose the correct controls and restore keyboard focus',
      async () => {
        const opener = owner.locator('#room-settings-btn');
        await opener.focus();
        await opener.press('Enter');
        const region = sheet(owner);
        await region.waitFor({ state: 'visible' });
        assert.equal(
          await region
            .getByRole('group', { name: 'Settings for', exact: true })
            .getByRole('button', { name: 'This room', exact: true })
            .getAttribute('aria-pressed'),
          'true',
          'the room control opens the This room list',
        );
        for (const [label, controls] of [
          ['Who can enter', ['#rs-guests', '#rs-lobby']],
          ['Chat & broadcasting', ['#rs-screen', '#rs-guests-broadcast', '#rs-max-participants']],
        ]) {
          await sheetScreen(owner, label);
          for (const selector of controls)
            await region.locator(selector).waitFor({ state: 'visible' });
        }
        await sheetScreen(owner, 'Who can enter');
        await sheetRow(owner, 'Password').click();
        await region.locator('#rs-password').waitFor({ state: 'visible' });
        await region.locator('#rs-password-remove').waitFor({ state: 'attached' });
        await owner.screenshot({
          path: path.join(artifacts, 'room-settings-desktop.png'),
          fullPage: true,
        });
        // Escape climbs Password, Who can enter and the list, then closes.
        for (let depth = 0; depth < 3; depth++) await owner.keyboard.press('Escape');
        await region.waitFor({ state: 'hidden' });
        assert.equal(await opener.evaluate((node) => node === document.activeElement), true);
      },
    );
    await step('personal and room settings fit mobile and short landscape viewports', async () => {
      const captures = await owner.evaluate(() => window.__communityCaptureRequests);
      try {
        for (const viewport of [
          { width: 390, height: 844 },
          { width: 844, height: 390 },
        ]) {
          await owner.setViewportSize(viewport);
          await owner.locator('#settings-btn').click();
          const region = await sheetScreen(owner, 'Camera & mic');
          await sheetRow(owner, 'Quality').click();
          const level = region.locator('[name=autoGainControl]');
          await level.waitFor({ state: 'visible' });
          await dialogFitsViewport(owner, region, level);
          if (viewport.width === 390)
            await owner.screenshot({
              path: path.join(artifacts, 'settings-mobile.png'),
              fullPage: true,
            });
          await sheetScreen(owner, 'Layout');
          await dialogFitsViewport(owner, region, region.locator('#layout-modern'));
          await closeSheet(owner);
          await owner.locator('#room-settings-btn').click();
          await sheetScreen(owner, 'Chat & broadcasting');
          await dialogFitsViewport(owner, region, region.locator('#rs-max-participants'));
          await closeSheet(owner);
        }
      } finally {
        await owner.setViewportSize({ width: 1440, height: 1000 });
      }
      assert.equal(
        await owner.evaluate(() => window.__communityCaptureRequests),
        captures,
        'responsive settings checks never capture',
      );
    });
    await step(
      'private conversations, unread, acknowledgements and recipient isolation',
      async () => {
        await action(owner, 'E2E Member', 'Private message');
        const sent = privateConversation(owner, 'E2E Member');
        await sendPrivate(sent, 'Private owner hello');
        await member.waitForFunction(() =>
          [...document.querySelectorAll('.conversation-tab')].some(
            (node) =>
              node.getAttribute('aria-label')?.includes('E2E Owner') &&
              node.querySelector('.conversation-unread')?.textContent === '1',
          ),
        );
        const ownerId = await member
          .locator('[data-participant-id]')
          .filter({
            has: member.getByRole('button', { name: 'Actions for E2E Owner', exact: true }),
          })
          .getAttribute('data-participant-id');
        assert.ok(ownerId);
        const conversation = member.locator(`.conversation-tab[data-conversation-id="${ownerId}"]`);
        await conversation.click();
        const received = privateConversation(member, 'E2E Owner');
        await received.waitFor({ state: 'visible' });
        assert.equal(
          await member
            .locator('.conversation-tab[data-conversation-id="public"]')
            .getAttribute('aria-pressed'),
          'true',
          'the shared account dialog preserves the underlying public conversation',
        );
        await visible(member, 'Private owner hello');
        await conversation.locator('.conversation-unread').waitFor({ state: 'hidden' });
        assert.equal(await conversation.getAttribute('aria-label'), 'E2E Owner');
        await visible(owner, 'Private owner hello');
        assert.equal(
          await sent
            .locator('.history-message')
            .filter({ hasText: 'Private owner hello' })
            .getByRole('button', { name: 'Remove', exact: true })
            .count(),
          0,
        );
        assert.equal(
          await sent
            .locator('.msg-text')
            .filter({ hasText: /^Private owner hello$/ })
            .count(),
          1,
        );
        assert.equal(await guest.getByText('Private owner hello', { exact: true }).count(), 0);
        await sendPrivate(received, 'Private member reply');
        await sent.getByText('Private member reply', { exact: true }).waitFor();
        await close(sent);
        await close(received);
      },
    );
    await step('account inbox reads retained PMs and sends through its composer', async () => {
      const inbox = await header(owner, 'Messages');
      await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
      const conversation = owner.getByRole('dialog', {
        name: 'Messages with E2E Member',
        exact: true,
      });
      await conversation
        .locator('.history-messages')
        .getByText('Private owner hello', { exact: true })
        .waitFor({ state: 'visible' });
      await conversation
        .locator('.history-messages')
        .getByText('Private member reply', { exact: true })
        .waitFor({ state: 'visible' });
      await conversation
        .getByLabel('Private message', { exact: true })
        .fill('Sent through saved inbox');
      await conversation.getByLabel('Private message', { exact: true }).press('Enter');
      await conversation
        .locator('.history-messages')
        .getByText('Sent through saved inbox', { exact: true })
        .waitFor({ state: 'visible' });
      await action(member, 'E2E Owner', 'Private message');
      const received = privateConversation(member, 'E2E Owner');
      await received.getByText('Sent through saved inbox', { exact: true }).waitFor();
      assert.equal(await guest.getByText('Sent through saved inbox', { exact: true }).count(), 0);
      assert.equal(
        await conversation.getByRole('button', { name: 'Remove', exact: true }).count(),
        0,
      );
      await conversation
        .getByLabel('Private message', { exact: true })
        .fill('An inbox draft for later');
      await close(conversation);
      await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
      assert.equal(
        await conversation.getByLabel('Private message', { exact: true }).inputValue(),
        'An inbox draft for later',
      );
      const sent = conversation
        .locator('.history-message')
        .filter({ hasText: 'Sent through saved inbox' });
      await sent.getByRole('button', { name: 'Edit', exact: true }).click();
      const edit = owner.getByRole('dialog', { name: 'Edit message', exact: true });
      await edit.getByLabel('Message', { exact: true }).fill('Corrected saved private message');
      await edit.getByRole('button', { name: 'Save changes', exact: true }).click();
      await edit.waitFor({ state: 'hidden' });
      await conversation
        .locator('.history-messages')
        .getByText('Corrected saved private message', { exact: true })
        .waitFor({ state: 'visible' });
      await received.getByText('Corrected saved private message', { exact: true }).waitFor();
      assert.equal(
        await guest.getByText('Corrected saved private message', { exact: true }).count(),
        0,
      );
      await conversation.getByLabel('Private message', { exact: true }).fill('');
      await close(conversation);
      await close(inbox);
      await close(received);
    });
    await step(
      'saved PM response loss survives close, reopen and reload without duplicate delivery',
      async () => {
        assert.ok(secondDevice, 'the account has an existing unjoined second device');
        const sender = secondDevice;
        const text = `Unconfirmed saved PM ${runId}`;
        const draft = `A separate saved PM draft ${runId}`;
        let inbox = await header(sender, 'Messages');
        const firstHistory = sender.waitForResponse(
          (response) =>
            response.request().method() === 'GET' &&
            /^\/api\/auth\/inbox\/[^/]+\/messages$/.test(new URL(response.url()).pathname),
        );
        await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
        const endpoint = new URL((await firstHistory).url()).pathname;
        const peer = endpoint.split('/')[4];
        const conversation = sender.getByRole('dialog', {
          name: 'Messages with E2E Member',
          exact: true,
        });
        const composer = conversation.getByLabel('Private message', { exact: true });
        const original = conversation.locator('.history-message').filter({
          has: sender.locator('.msg-text').filter({ hasText: /^Private owner hello$/ }),
        });
        await original.waitFor();
        const replyTarget = await original.getAttribute('data-message-id');
        assert.ok(replyTarget);
        const retry = conversation.getByRole('button', {
          name: 'Retry unconfirmed send',
          exact: true,
        });
        const attempts = [];
        const receipts = [];
        let committed;
        const routePattern = `**${endpoint}`;
        const intercept = async (route) => {
          if (route.request().method() !== 'POST') return route.fallback();
          const request = route.request().postDataJSON();
          attempts.push(request.clientMessageId);
          const firstAttempt = attempts.length === 1;
          assert.ok(request.content === text, 'a retry preserves the submitted PM content');
          assert.equal(
            request.replyTo,
            replyTarget,
            'a retry preserves its submitted reply target',
          );
          const response = await route.fetch({ timeout: 10000, maxRetries: 0 });
          assert.equal(response.status(), 200, 'the real backend accepts the saved PM');
          const saved = await response.json();
          receipts.push({ messageId: saved.messageId, clientMessageId: saved.clientMessageId });
          assert.equal(saved.clientMessageId, request.clientMessageId);
          assert.equal(saved.recipientId, peer);
          assert.equal(saved.replyTo.messageId, replyTarget);
          if (firstAttempt) {
            committed = {
              messageId: saved.messageId,
              clientMessageId: saved.clientMessageId,
              sender: saved.participantId,
            };
            // The transaction and recipient delivery are real. Only delivery
            // of the first HTTP response back to this browser is interrupted.
            await route.abort('failed');
          } else await route.fulfill({ response });
        };
        await sender.route(routePattern, intercept);
        try {
          await original.getByRole('button', { name: 'Reply', exact: true }).click();
          await composer.fill(text);
          await composer.press('Enter');
          await retry.click({ trial: true });
          assert.ok(committed, 'the backend committed before its response was dropped');
          assert.equal(attempts.length, 1);
          await conversation.getByText('Unconfirmed send', { exact: true }).waitFor();
          assert.equal(await composer.inputValue(), '');
          await composer.fill(draft);
          assert.equal(await conversation.locator('.history-send').isDisabled(), true);
          await action(member, 'E2E Owner', 'Private message');
          const received = privateConversation(member, 'E2E Owner');
          await received.getByText(text, { exact: true }).waitFor();
          assert.equal(
            await savedPrivateRowCount(committed.sender, peer, committed.clientMessageId),
            1,
            'the interrupted response follows a real single-row commit',
          );

          await close(conversation);
          await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
          await conversation
            .locator(`.history-message[data-message-id="${committed.messageId}"]`)
            .waitFor();
          await retry.click({ trial: true });
          assert.equal(await composer.inputValue(), draft);
          await conversation
            .getByText('Replying to E2E Owner: Private owner hello', { exact: true })
            .waitFor();
          assert.equal(attempts.length, 1, 'reopening never resends an unconfirmed PM');
          await close(conversation);
          await close(inbox);

          await sender.reload({ waitUntil: 'domcontentloaded' });
          await testNetworkReady();
          await sender.waitForFunction(
            () =>
              document.getElementById('auth-display-name').textContent === 'E2E Owner' &&
              document.getElementById('connection-status').classList.contains('connected'),
          );
          assert.equal(await sender.locator('#room-screen').isVisible(), false);
          assert.equal(await sender.evaluate(() => window.__communityCaptureRequests), 0);
          inbox = await header(sender, 'Messages');
          await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
          await conversation
            .locator(`.history-message[data-message-id="${committed.messageId}"]`)
            .waitFor();
          await retry.click({ trial: true });
          assert.equal(await composer.inputValue(), draft);
          assert.equal(attempts.length, 1, 'reload never resends an unconfirmed PM');

          const confirmedHistory = sender.waitForResponse(
            (response) =>
              response.request().method() === 'GET' &&
              new URL(response.url()).pathname === endpoint,
          );
          await retry.click();
          await retry.waitFor({ state: 'hidden' });
          const history = await (await confirmedHistory).json();
          assert.deepEqual(attempts, [committed.clientMessageId, committed.clientMessageId]);
          assert.deepEqual(receipts, [
            { messageId: committed.messageId, clientMessageId: committed.clientMessageId },
            { messageId: committed.messageId, clientMessageId: committed.clientMessageId },
          ]);
          assert.equal(
            history.messages.filter(
              (message) => message.clientMessageId === committed.clientMessageId,
            ).length,
            1,
          );
          assert.equal(
            await savedPrivateRowCount(committed.sender, peer, committed.clientMessageId),
            1,
            'explicit retry confirms the original row instead of inserting a duplicate',
          );
          assert.equal(
            await conversation
              .locator(`.history-message[data-message-id="${committed.messageId}"]`)
              .count(),
            1,
          );
          assert.equal(await composer.inputValue(), draft, 'confirmation preserves the new draft');
          assert.equal(await guest.getByText(text, { exact: true }).count(), 0);
          report.savedPmResponseLoss = {
            passed: true,
            requests: attempts.length,
            clientMessageId: committed.clientMessageId,
            messageId: committed.messageId,
            storedRows: 1,
            closeReopenWithoutResend: true,
            reloadWithoutResend: true,
            separateDraftPreserved: true,
            replyTargetPreserved: true,
          };
          const reactionPath = `${endpoint}/${committed.messageId}/reactions`;
          const reactionAttempts = [];
          let reactionFailure;
          let releaseCanonical;
          let canonicalStarted;
          let heldCanonicalReads = 0;
          const canonicalGate = new Promise((resolve) => {
            releaseCanonical = resolve;
          });
          const canonicalRequested = new Promise((resolve) => {
            canonicalStarted = resolve;
          });
          const historyRoute = (url) =>
            url.origin === new URL(base).origin && url.pathname === endpoint;
          const holdCanonical = async (route) => {
            if (route.request().method() !== 'GET') return route.fallback();
            heldCanonicalReads++;
            canonicalStarted();
            await canonicalGate;
            // A newer invalidation can retire a held read. The subsequent UI
            // and recipient assertions still require authoritative results.
            await route.continue().catch(() => {});
          };
          const setReaction = async (route) => {
            try {
              const body = route.request().postDataJSON();
              reactionAttempts.push(body);
              assert.deepEqual(body, { emoji: '👍', present: true });
              const response = await route.fetch({ timeout: 10000, maxRetries: 0 });
              assert.equal(response.status(), 200);
              const result = await response.json();
              assert.equal(result.messageId, committed.messageId);
              assert.deepEqual(result.reactions, [
                { emoji: '👍', participantIds: [committed.sender] },
              ]);
              if (reactionAttempts.length === 1) await route.abort('failed');
              else {
                await route.fulfill({ response });
                releaseCanonical();
              }
            } catch (error) {
              reactionFailure = error;
              await route.abort('failed').catch(() => {});
            }
          };
          // This subcase loses both the mutation receipt and its independent
          // history confirmation until the explicit retry. With a confirmed
          // live reaction, a second click would correctly request removal.
          await sender.route(historyRoute, holdCanonical);
          await sender.route(`**${reactionPath}`, setReaction);
          try {
            const row = conversation.locator(
              `.history-message[data-message-id="${committed.messageId}"]`,
            );
            await row.getByRole('button', { name: 'React', exact: true }).click();
            await row.getByRole('button', { name: '👍', exact: true }).click();
            await conversation.locator('.auth-error').waitFor({ state: 'visible' });
            await deadline(canonicalRequested);
            assert.ifError(reactionFailure);
            await row.getByRole('button', { name: 'React', exact: true }).click();
            await row.getByRole('button', { name: '👍', exact: true }).click();
            await row.getByRole('button', { name: '👍 1', exact: true }).waitFor();
            assert.ifError(reactionFailure);
            assert.equal(reactionAttempts.length, 2);
            await received
              .locator(`.history-message[data-message-id="${committed.messageId}"]`)
              .getByRole('button', { name: '👍 1', exact: true })
              .waitFor();
            report.savedPmReactionResponseLoss = {
              passed: true,
              requests: 2,
              explicitPresence: true,
              retainedParticipantCount: 1,
              canonicalReadsWithheld: heldCanonicalReads,
            };
          } catch (error) {
            throw reactionFailure ?? error;
          } finally {
            releaseCanonical();
            await sender.unroute(`**${reactionPath}`, setReaction);
            await sender.unroute(historyRoute, holdCanonical);
          }
          await composer.fill('');
          await close(conversation);
          await close(inbox);
          await close(received);
        } finally {
          await sender.unroute(routePattern, intercept);
        }
      },
    );
    await step('private drafts and sent-input recall stay in their conversation', async () => {
      await publicChat(owner);
      await send(owner, 'Public recall fixture');
      await owner.locator('#chat-input').fill('Unsent public draft');
      await action(owner, 'E2E Member', 'Private message');
      let conversation = privateConversation(owner, 'E2E Member');
      await conversation
        .getByLabel('Private message', { exact: true })
        .fill('Unsent private draft');
      await close(conversation);
      assert.equal(await owner.locator('#chat-input').inputValue(), 'Unsent public draft');
      await owner.locator('#chat-input').fill('');
      await owner.locator('#chat-input').press('ArrowUp');
      // Public recall contains the earlier public send, never the private send or draft.
      assert.equal(await owner.locator('#chat-input').inputValue(), 'Public recall fixture');
      await action(owner, 'E2E Member', 'Private message');
      conversation = privateConversation(owner, 'E2E Member');
      assert.equal(
        await conversation.getByLabel('Private message', { exact: true }).inputValue(),
        'Unsent private draft',
      );
      await conversation.getByLabel('Private message', { exact: true }).fill('');
      await close(conversation);
      await owner.locator('#chat-input').fill('');
    });
    await step('PM opt-out returns explicit delivery failure', async () => {
      await preferences(member, false);
      await action(owner, 'E2E Member', 'Private message');
      const conversation = privateConversation(owner, 'E2E Member');
      await sendPrivate(conversation, 'Blocked by opt-out');
      await conversation
        .locator('.auth-error')
        .filter({ hasText: 'Private message unavailable' })
        .waitFor();
      assert.equal(await member.getByText('Blocked by opt-out', { exact: true }).count(), 0);
      await dismissUnconfirmed(owner, conversation);
      await close(conversation);
      await preferences(member, true);
    });
    await step('ignore/unignore hides public messages and blocks private delivery', async () => {
      const ignored = member.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === '/api/auth/preferences' &&
          response.request().method() === 'PATCH' &&
          response.ok(),
      );
      await action(member, 'E2E Owner', 'Ignore messages');
      await ignored;
      await publicChat(owner);
      await publicChat(member);
      await send(owner, 'Ignored public text');
      await visible(guest, 'Ignored public text');
      assert.equal(await member.getByText('Ignored public text', { exact: true }).count(), 0);
      await action(owner, 'E2E Member', 'Private message');
      const conversation = privateConversation(owner, 'E2E Member');
      await sendPrivate(conversation, 'Ignored private text');
      await conversation
        .locator('.auth-error')
        .filter({ hasText: 'Private message unavailable' })
        .waitFor();
      assert.equal(await member.getByText('Ignored private text', { exact: true }).count(), 0);
      await dismissUnconfirmed(owner, conversation);
      await close(conversation);
      const unignored = member.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === '/api/auth/preferences' &&
          response.request().method() === 'PATCH' &&
          response.ok(),
      );
      await action(member, 'E2E Owner', 'Unignore messages');
      await unignored;
      await publicChat(owner);
      await send(owner, 'Visible after unignore');
      await visible(member, 'Visible after unignore');
    });
    await step('nickname, mentions, emoji and sent-input recall', async () => {
      await header(guest, 'Nickname');
      const dialog = guest.getByRole('dialog', { name: 'Room nickname', exact: true });
      await dialog.getByLabel('Nickname', { exact: true }).fill('E2ERenamed');
      await dialog.getByRole('button', { name: 'Change nickname', exact: true }).click();
      await dialog.waitFor({ state: 'hidden' });
      await owner
        .getByRole('button', { name: 'Actions for E2ERenamed', exact: true })
        .first()
        .waitFor({ state: 'visible' });
      await owner.locator('#chat-input').fill('@E2ER');
      await owner.locator('#chat-input').press('Tab');
      assert.equal(await owner.locator('#chat-input').inputValue(), '@E2ERenamed ');
      await send(owner, '@E2ERenamed welcome');
      await guest
        .locator('.chat-msg.mentioned')
        .filter({ hasText: '@E2ERenamed welcome' })
        .waitFor();
      await owner.getByRole('button', { name: 'Choose emoji', exact: true }).click();
      await owner.getByRole('button', { name: '👋', exact: true }).click();
      await submitMessage(owner);
      await visible(guest, '👋');
      await owner.locator('#chat-input').press('ArrowUp');
      assert.equal(await owner.locator('#chat-input').inputValue(), '👋');
      await owner.locator('#chat-input').fill('');
    });
    await step(
      'a chat color reaches chat and the people list and stays with the account',
      async () => {
        await member.getByRole('button', { name: 'Chat options', exact: true }).click();
        const lookScreen = await sheetScreen(member, 'Chat look');
        // Each choice applies at once and reports Saved once the room confirms it.
        const saved = lookScreen.locator('.chat-look-block .sheet-row-saved');
        const applyLook = async (name) => {
          await saved.evaluate((node) => {
            node.textContent = '';
          });
          await lookScreen.getByRole('radio', { name, exact: true }).check();
          await saved.filter({ hasText: 'Saved' }).waitFor();
        };
        await applyLook('Violet');
        await applyLook('Tinted bubble');
        await closeSheet(member);
        const violet = '#a78bfa';
        const look = (page, text) =>
          page
            .locator('.chat-msg.look-bubble')
            .filter({ hasText: text })
            .evaluate((node) => node.style.getPropertyValue('--sender-color'));
        await publicChat(member);
        await send(member, 'Violet hello');
        assert.equal(await look(owner, 'Violet hello'), violet);
        assert.equal(
          await owner
            .locator('li[data-participant-id]')
            .filter({ hasText: 'E2E Member' })
            .first()
            .evaluate((node) => node.style.getPropertyValue('--person-color')),
          violet,
        );
        // Forget this browser's copy: the next join must get the look from the account.
        await leave(member);
        await member.evaluate(() => {
          for (const key of Object.keys(localStorage).filter((name) =>
            name.startsWith('simplestchat.chat.v1.'),
          )) {
            const saved = JSON.parse(localStorage.getItem(key) ?? '{}');
            delete saved.look;
            localStorage.setItem(key, JSON.stringify(saved));
          }
        });
        await join(member, 'E2E Member');
        await send(member, 'Still violet');
        assert.equal(await look(guest, 'Still violet'), violet);
      },
    );
    await step('report submission and moderator review/resolve', async () => {
      await action(guest, 'E2E Member', 'Report to moderators');
      const report = guest.getByRole('dialog', { name: 'Report E2E Member', exact: true });
      await report
        .getByLabel('Reason and details', { exact: true })
        .fill('Disposable browser test report');
      await report.getByRole('button', { name: 'Send report', exact: true }).click();
      await report.waitFor({ state: 'hidden' });
      const manage = await header(owner, 'Manage room');
      await sheetScreen(owner, 'Reports');
      await visible(owner, 'Disposable browser test report');
      await manage.getByRole('button', { name: 'Mark resolved', exact: true }).click();
      await visible(owner, 'E2E Member · resolved');
      await sheetScreen(owner, 'History');
      await visible(owner, 'E2E Member · report resolved');
      await close(manage);
    });
    await step('membership roles, live moderation gates and allow-chat toggle', async () => {
      await action(owner, 'E2E Member', 'Moderator');
      await roomMenuOffers(member, 'Manage room');
      await setRole(owner, 'E2E Member', 'member');
      await setRole(owner, 'E2E Member', 'user');
      await roomSetting(owner, '#rs-moderated', true);
      await chatEnabled(member, false);
      await chatEnabled(guest, false);
      await action(owner, 'E2E Member', 'Member');
      await chatEnabled(member, true);
      await send(member, 'Member has voice');
      await visible(owner, 'Member has voice');
      await roomSetting(owner, '#rs-chat', false);
      await chatEnabled(owner, false);
      await chatEnabled(member, false);
      await roomSetting(owner, '#rs-chat', true);
      await roomSetting(owner, '#rs-moderated', false);
      await chatEnabled(guest, true);
      await action(owner, 'E2E Member', 'Mute Text');
      await chatEnabled(member, false);
      await action(owner, 'E2E Member', 'Text Unmute');
      await chatEnabled(member, true);
    });
    await step(
      'a saved account conversation remains usable after its peer leaves the room',
      async () => {
        await action(owner, 'E2E Member', 'Private message');
        const conversation = privateConversation(owner, 'E2E Member');
        await leave(member);
        await sendPrivate(conversation, 'Saved PM after leaving the room');
        await conversation.getByText('Saved PM after leaving the room', { exact: true }).waitFor();
        const inbox = await header(member, 'Messages');
        await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Owner' }).click();
        const received = privateConversation(member, 'E2E Owner');
        await received.getByText('Saved PM after leaving the room', { exact: true }).waitFor();
        await close(received);
        await close(inbox);
        await close(conversation);
        await setRole(owner, 'E2E Member', 'moderator');
        await chatEnabled(owner, true);
        await join(member, 'E2E Member');
        await roomMenuOffers(member, 'Manage room');
      },
    );
    await step('ban review/unban and rejoin', async () => {
      await action(owner, 'E2E Member', 'Ban…');
      const ban = owner.getByRole('dialog', { name: 'Ban E2E Member', exact: true });
      await ban
        .getByLabel('Reason (optional)', { exact: true })
        .fill('Disposable browser test ban');
      await ban.getByRole('button', { name: 'Ban from room', exact: true }).click();
      await member.locator('#join-screen').waitFor({ state: 'visible' });
      // Removal is explained in an owned dialog rather than a native alert.
      const removed = member.getByRole('dialog', { name: 'Removed from the room', exact: true });
      await removed
        .getByText('You were banned from this room. Reason: Disposable browser test ban', {
          exact: true,
        })
        .waitFor();
      await removed.getByRole('button', { name: 'Back to home', exact: true }).click();
      await removed.waitFor({ state: 'hidden' });
      const manage = await header(owner, 'Manage room');
      await sheetScreen(owner, 'Bans');
      await visible(owner, 'Disposable browser test ban');
      await manage.getByRole('button', { name: 'Unban', exact: true }).click();
      await visible(owner, 'No bans on this page.');
      await sheetScreen(owner, 'History');
      await visible(owner, 'E2E Member · unbanned');
      await visible(owner, 'E2E Member · banned');
      await visible(owner, 'Disposable browser test ban');
      await close(manage);
      await join(member, 'E2E Member');
    });
    await step(
      'local media preview, real camera/audio receive and personal hide/restore',
      async () => {
        await openRoomMenu(guest);
        await guest.locator('#mic-setup-btn').click();
        let setup = sheet(guest);
        await setup.getByRole('heading', { level: 2, name: 'Camera & mic', exact: true }).waitFor();
        await setup.getByRole('button', { name: 'Test camera', exact: true }).click();
        await guest.waitForFunction(
          () => document.querySelector('.media-preview-video')?.videoWidth > 0,
        );
        assert.equal(
          await owner.locator('.video-tile:not(.local) video').count(),
          0,
          'preview must not broadcast',
        );
        const previewTracks = await setup
          .locator('.media-preview-video')
          .evaluateHandle((video) => video.srcObject.getTracks());
        const previewCaptures = await guest.evaluate(() => window.__communityCaptureRequests);
        try {
          await setup.getByRole('button', { name: 'Back', exact: true }).click();
          assert.equal(
            await previewTracks.evaluate(
              (tracks) =>
                tracks.length > 0 && tracks.every((track) => track.readyState === 'ended'),
            ),
            true,
            'leaving the screen stops its capture',
          );
          assert.equal(await guest.locator('.media-preview-video').count(), 0);
          setup = await sheetScreen(guest, 'Camera & mic');
          assert.equal(
            await guest.evaluate(() => window.__communityCaptureRequests),
            previewCaptures,
            'returning to the screen does not recapture',
          );
        } finally {
          await previewTracks.dispose();
        }
        await setup.getByRole('button', { name: 'Test camera', exact: true }).click();
        await guest.waitForFunction(
          () => document.querySelector('.media-preview-video')?.videoWidth > 0,
        );
        await setup.getByRole('button', { name: 'Stop preview', exact: true }).click();
        await guest.waitForFunction(
          () => !document.querySelector('.media-preview-video')?.srcObject,
        );
        // Firefox and WebKit keep microphone identifiers private until that
        // device kind is explicitly captured, independently of camera access.
        await setup.getByRole('button', { name: 'Test microphone', exact: true }).click();
        await setup
          .getByText('Microphone test is live. Only you see the level; nothing plays back.', {
            exact: true,
          })
          .waitFor();
        const microphonePreviewTracks = await setup
          .locator('.media-preview-video')
          .evaluateHandle((video) => video.srcObject.getAudioTracks());
        try {
          assert.equal(
            await microphonePreviewTracks.evaluate(
              (tracks) => tracks.length > 0 && tracks.every((track) => track.readyState === 'live'),
            ),
            true,
            'microphone preview captures a live local track',
          );
          assert.equal(
            await owner.locator('.video-tile:not(.local) audio').count(),
            0,
            'microphone preview must not broadcast',
          );
          await setup.getByRole('button', { name: 'Stop preview', exact: true }).click();
          assert.equal(
            await microphonePreviewTracks.evaluate((tracks) =>
              tracks.every((track) => track.readyState === 'ended'),
            ),
            true,
            'stopping microphone preview ends its capture',
          );
        } finally {
          await microphonePreviewTracks.dispose();
        }
        const devicesListed = (name) =>
          guest.waitForFunction(
            (name) =>
              [...document.querySelector(`#settings-sheet select[name="${name}"]`).options].some(
                (option) => option.value,
              ),
            name,
          );
        const selectedDevices = {};
        for (const name of ['cameraDeviceId', 'microphoneDeviceId']) {
          await devicesListed(name);
          const select = setup.locator(`select[name="${name}"]`);
          selectedDevices[name] = await select.evaluate(
            (select) =>
              [...select.options].find((option) => option.value && option.value !== 'default')
                ?.value ?? [...select.options].find((option) => option.value).value,
          );
          await select.selectOption(selectedDevices[name]);
          await select.locator('..').locator('.sheet-row-saved', { hasText: 'Saved' }).waitFor();
        }
        const capturesAfterPreview = await guest.evaluate(() => window.__communityCaptureRequests);
        await closeSheet(guest);
        await guest.locator('#settings-btn').click();
        setup = await sheetScreen(guest, 'Camera & mic');
        for (const name of Object.keys(selectedDevices)) {
          await devicesListed(name);
          assert.equal(
            await setup.locator(`select[name="${name}"]`).inputValue(),
            selectedDevices[name],
            'a chosen device survives closing and reopening',
          );
          await setup.locator(`select[name="${name}"]`).selectOption('');
        }
        await closeSheet(guest);
        await guest.locator('#settings-btn').click();
        setup = await sheetScreen(guest, 'Camera & mic');
        for (const name of Object.keys(selectedDevices)) {
          await devicesListed(name);
          assert.equal(
            await setup.locator(`select[name="${name}"]`).inputValue(),
            '',
            'explicit default device survives reopening',
          );
        }
        await closeSheet(guest);
        assert.equal(
          await guest.evaluate(() => window.__communityCaptureRequests),
          capturesAfterPreview,
          'choosing devices never starts inactive capture',
        );
        await guest.locator('#cam-btn').click();
        await remotePlayback(owner, 'video');
        await guest.locator('#mic-btn').click();
        await remotePlayback(owner, 'audio');
        const audioTime = await owner
          .locator('.video-tile:not(.local) audio')
          .first()
          .evaluate((audio) => audio.currentTime);
        await owner.waitForFunction(
          (previous) =>
            [...document.querySelectorAll('.video-tile:not(.local) audio')].some(
              (audio) =>
                !audio.paused &&
                !audio.muted &&
                audio.volume > 0 &&
                audio.currentTime > previous + 0.25,
            ),
          audioTime,
        );
        const viewingControls = owner.locator('.personal-media-controls').first();
        const viewingTrigger = viewingControls.locator('summary');
        await viewingTrigger.click();
        // Deterministic UI recovery control: inject a policy rejection on this one
        // element, then restore native play() before the real user-gesture retry.
        // This is separate from the unmodified native playback assertion above.
        const remoteAudio = owner.locator('.video-tile:not(.local) audio').first();
        await remoteAudio.evaluate((audio) => {
          audio.autoplay = false;
          audio.pause();
          audio.play = () =>
            Promise.reject(new DOMException('Simulated autoplay denial', 'NotAllowedError'));
        });
        try {
          await owner.locator('[data-control="volume"]').first().press('ArrowLeft');
          const retry = owner.getByRole('button', {
            name: 'Enable playback for E2ERenamed',
            exact: true,
          });
          await retry.waitFor({ state: 'visible' });
          await owner.screenshot({
            path: path.join(artifacts, 'playback-recovery.png'),
            fullPage: true,
          });
          await remoteAudio.evaluate((audio) => {
            delete audio.play;
          });
          await retry.click();
          await retry.waitFor({ state: 'hidden' });
          // Playback recovery is outside the viewing popover, so its click
          // light-dismisses the menu before the next viewing action.
          await viewingControls.locator('.personal-media-panel').waitFor({ state: 'hidden' });
          assert.equal(await viewingTrigger.getAttribute('aria-expanded'), 'false');
          const previous = await remoteAudio.evaluate((audio) => audio.currentTime);
          await owner.waitForFunction(
            (time) =>
              [...document.querySelectorAll('.video-tile:not(.local) audio')].some(
                (audio) =>
                  !audio.paused &&
                  !audio.muted &&
                  audio.volume > 0 &&
                  audio.currentTime > time + 0.25,
              ),
            previous,
          );
          report.simulatedAutoplayRecovery = true;
        } finally {
          await deadline(
            remoteAudio.evaluate((audio) => {
              delete audio.play;
              audio.autoplay = true;
            }),
          ).catch(() => {});
        }
        await viewingTrigger.click();
        await owner
          .getByRole('combobox', { name: 'Video quality for E2ERenamed', exact: true })
          .selectOption('low');
        await owner.getByRole('button', { name: 'Hide for me', exact: true }).first().click();
        await owner.waitForFunction(() =>
          [...document.querySelectorAll('.video-tile:not(.local) video')].every(
            (video) => video.paused,
          ),
        );
        await owner.getByRole('button', { name: 'Restore broadcast', exact: true }).first().click();
        await owner.waitForFunction(() =>
          [...document.querySelectorAll('.video-tile:not(.local) video')].some(
            (video) => !video.paused && video.videoWidth > 0,
          ),
        );
        await owner.screenshot({ path: path.join(artifacts, 'remote-media.png'), fullPage: true });
        await guest.locator('#cam-btn').click();
        await guest.locator('#mic-btn').click();
      },
    );
    await step('publisher and receiver signaling resume preserve live media', async () => {
      // Reuse owned fake devices, but make this a separate result from viewer UI
      // controls. Only native signaling is interrupted; media UDP stays intact.
      await guest.locator('#cam-btn').click();
      await remotePlayback(owner, 'video');
      await guest.locator('#mic-btn').click();
      await remotePlayback(owner, 'audio');
      await signalingReconnect(guest, guest, owner, 'publisher');
      await signalingReconnect(owner, guest, owner, 'receiver');
    });
    await step('viewer controls changed offline apply after signaling recovery', async () => {
      await offlineViewerControls(guest, owner);
      await guest.locator('#cam-btn').click();
      await guest.locator('#mic-btn').click();
    });
    let checkUnacknowledgedControl;
    let checkIceRestart;
    await step(
      'interrupted transport setup rejoins with working media and no automatic capture',
      async () => {
        let transportReplies = 0;
        let interrupted = false;
        let closing = Promise.resolve();
        let closeFailed = false;
        let dropPause = false;
        let droppedPause = false;
        let acknowledgedPauses = 0;
        const transportIds = [];
        let restartCurrentTransports;
        const probe = await client(
          'interrupted-setup',
          false,
          async (context) => {
            await context.routeWebSocket(`${base.replace(/^http/, 'ws')}/ws`, (route) => {
              const server = route.connectToServer();
              let retired = false;
              restartCurrentTransports = () => {
                assert.equal(retired, false, 'ICE restart must use the current signaling route');
                assert.equal(transportIds.length, 2);
                // Exercise real server restarts through the supported legacy
                // no-ID command path. Forward the resulting replies unchanged.
                for (const transportId of transportIds)
                  server.send(JSON.stringify({ type: 'restartIce', transportId }));
              };
              const interrupt = (delayMs = 0) => {
                retired = true;
                closing = (async () => {
                  if (delayMs) await new Promise((resolve) => setTimeout(resolve, delayMs));
                  await Promise.all([server.close(), route.close()]);
                })();
                closing.catch(() => {
                  closeFailed = true;
                });
              };
              route.onMessage((message) => {
                if (retired) return;
                const parsed = JSON.parse(String(message));
                if (dropPause && !droppedPause && parsed.type === 'pauseProducer') {
                  droppedPause = true;
                  // Keep the socket apparently open beyond the client's request
                  // deadline, as can happen before network failure is detected.
                  interrupt(5500);
                  return;
                }
                server.send(message);
              });
              server.onMessage((message) => {
                if (retired) return;
                const parsed = JSON.parse(String(message));
                if (parsed.type === 'transportCreated') {
                  transportReplies++;
                  transportIds.push(parsed.transportId);
                  if (transportIds.length > 2) transportIds.shift();
                  if (!interrupted && transportReplies === 2) {
                    // The real server created both transports. Lose its receive
                    // reply by closing both owned signaling ends, without forging
                    // any protocol response or changing native RTC behavior.
                    interrupted = true;
                    interrupt();
                    return;
                  }
                }
                if (parsed.type === 'producerPaused' && typeof parsed.requestId === 'string')
                  acknowledgedPauses++;
                route.send(message);
              });
            });
          },
          turnRelay && options.name === 'webkit',
        );
        await join(probe, 'Setup recovery');
        // The interrupted setup rejoins by itself: a second join from this address.
        recordJoin();
        await probe.waitForFunction(() => {
          const counts = window.__communitySignalingReconnect.snapshot().counters;
          return (
            counts.reconnectSuccess === 1 &&
            counts.sentJoinRoom === 2 &&
            counts.sentCreateSendTransport === 2 &&
            counts.sentCreateRecvTransport === 2
          );
        });
        await closing;
        assert.equal(closeFailed, false);
        assert.equal(interrupted, true);
        assert.equal(transportReplies, 4);
        await connected(probe);
        assert.equal(await probe.evaluate(() => window.__communityCaptureRequests), 0);
        await probe.locator('#cam-btn').click();
        await probe.locator('#cam-btn:not(.muted)').waitFor({ state: 'visible' });
        assert.equal(
          await probe.locator('#settings-sheet').isVisible(),
          false,
          'first camera activation requests capture without a settings prerequisite',
        );
        assert.equal(await probe.evaluate(() => window.__communityCaptureRequests), 1);
        await probe.locator('#mic-btn').click();
        await remotePlayback(owner, 'video');
        await remotePlayback(owner, 'audio');
        const identity = await reconnectMediaIdentity(owner);
        try {
          // This is a new receiver, so wait for its first native RTP report
          // before measuring progress. The retained-reconnect checks already
          // have live counters and deliberately do not need this startup wait.
          await waitForMediaStart(owner, identity);
          report.interruptedSetup = {
            passed: true,
            transportReplies,
            media: await advancingReconnectMedia(owner, identity, 3000),
          };
        } finally {
          await identity.dispose();
        }
        checkUnacknowledgedControl = async () => {
          const peers = await probe.evaluateHandle(() => window.__communityPeers.slice());
          const before = await probe.evaluate(
            () => window.__communitySignalingReconnect.snapshot().counters,
          );
          const previousAcks = acknowledgedPauses;
          try {
            dropPause = true;
            await probe.locator('#mic-btn').click();
            await probe.waitForFunction(
              (previous) =>
                window.__communitySignalingReconnect.snapshot().counters.reconnectSuccess ===
                previous + 1,
              before.reconnectSuccess,
            );
            await connected(probe);
            await owner.waitForFunction(
              () => document.querySelectorAll('.video-tile:not(.local) audio').length === 0,
            );
            await closing;
            assert.equal(closeFailed, false);
            assert.equal(droppedPause, true);
            assert.equal(acknowledgedPauses, previousAcks + 1);
            assert.equal(
              await peers.evaluate((before) => {
                const after = window.__communityPeers;
                return (
                  before.length === after.length &&
                  before.every((peer, index) => peer === after[index])
                );
              }),
              true,
              'control recovery must preserve native peer connections',
            );
            const after = await probe.evaluate(
              () => window.__communitySignalingReconnect.snapshot().counters,
            );
            for (const key of [
              'sentJoinRoom',
              'sentCreateSendTransport',
              'sentCreateRecvTransport',
              'sentProduce',
            ])
              assert.equal(after[key], before[key]);
            await probe.locator('#mic-btn').click();
            await remotePlayback(owner, 'audio');
            const resumed = await reconnectMediaIdentity(owner);
            try {
              await waitForMediaStart(owner, resumed);
              report.unacknowledgedControl = {
                passed: true,
                media: await advancingReconnectMedia(owner, resumed, 3000),
              };
            } finally {
              await resumed.dispose();
            }
          } finally {
            await peers.dispose();
          }
        };
        checkIceRestart = async () => {
          if (turnRelay) {
            // Give the probe a live return stream too. A paused receive
            // transport can legitimately have zero RTP bytes after restarting.
            await owner.locator('#cam-btn').click();
            await owner.locator('#mic-btn').click();
            await remotePlayback(probe, 'video');
            await remotePlayback(probe, 'audio');
            const returning = await reconnectMediaIdentity(probe);
            try {
              await waitForMediaStart(probe, returning);
              await advancingReconnectMedia(probe, returning, 3000);
            } finally {
              await returning.dispose();
            }
            // The owned server issues 60-second credentials. Wait on real wall
            // time so the restart must obtain fresh credentials after expiry.
            await probe.waitForFunction(
              () => {
                const peers = window.__communityPeers.filter(
                  (peer) => peer.connectionState === 'connected' && peer.remoteDescription,
                );
                return (
                  peers.length === 2 &&
                  peers.every((peer) => {
                    const username = peer.getConfiguration().iceServers?.[0]?.username;
                    const expiry = Number(username?.split(':')[0]);
                    return expiry > 0 && expiry * 1000 + 1000 < Date.now();
                  })
                );
              },
              null,
              { timeout: 70000 },
            );
            report.relayBeforeRestart = await waitForRelayPaths(probe);
            assert.equal(report.relayBeforeRestart.length, 2);
          }
          if (turnRelay && options.name === 'firefox') {
            const before = await probe.evaluateHandle(() => ({
              peers: window.__communityPeers.filter((peer) => peer.connectionState !== 'closed'),
              tracks: document.querySelector('#local-tile video').srcObject.getTracks(),
              captures: window.__communityCaptureRequests,
              counts: window.__communitySignalingReconnect.snapshot().counters,
            }));
            try {
              restartCurrentTransports();
              await probe.waitForFunction(({ peers, counts }) => {
                const current = window.__communitySignalingReconnect.snapshot().counters;
                return (
                  peers.every((peer) => peer.connectionState === 'closed') &&
                  current.sentJoinRoom === counts.sentJoinRoom + 1 &&
                  current.sentCreateSendTransport === counts.sentCreateSendTransport + 1 &&
                  current.sentCreateRecvTransport === counts.sentCreateRecvTransport + 1
                );
              }, before);
              await connected(probe);
              await probe.locator('#mic-btn.muted').waitFor({ state: 'visible' });
              await probe.locator('#cam-btn.muted').waitFor({ state: 'visible' });
              assert.equal(
                await before.evaluate(
                  ({ captures, tracks }) =>
                    window.__communityCaptureRequests === captures &&
                    tracks.length > 0 &&
                    tracks.every((track) => track.readyState === 'ended'),
                ),
                true,
                'TURN rebuild must stop old capture and never recapture automatically',
              );
              await owner.locator('.video-tile:not(.local) audio').waitFor({ state: 'detached' });
              await owner.locator('.video-tile:not(.local) video').waitFor({ state: 'detached' });
              await probe.locator('#cam-btn').click();
              await probe.locator('#mic-btn').click();
              await remotePlayback(owner, 'video');
              await remotePlayback(owner, 'audio');
              const receiver = await reconnectMediaIdentity(owner);
              const returning = await reconnectMediaIdentity(probe);
              try {
                await waitForMediaStart(owner, receiver);
                await waitForMediaStart(probe, returning);
                report.iceRestart = {
                  passed: true,
                  rebuilt: true,
                  media: await advancingReconnectMedia(owner, receiver, 3000),
                  relay: await waitForRelayPaths(probe),
                  returnMedia: await advancingReconnectMedia(probe, returning, 3000),
                };
              } finally {
                await receiver.dispose();
                await returning.dispose();
              }
            } finally {
              await before.dispose();
            }
            await leave(probe);
            await retireTestNetwork(probe);
            await probe.context().close();
            await owner.locator('#cam-btn').click();
            await owner.locator('#mic-btn').click();
            return;
          }
          const identity = await reconnectMediaIdentity(probe);
          const receiver = await reconnectMediaIdentity(owner);
          const credentials = await probe.evaluateHandle(() =>
            window.__communityPeers
              .filter((peer) => peer.connectionState !== 'closed' && peer.remoteDescription)
              .map((peer) => ({
                peer,
                ufrag: peer.remoteDescription.sdp.match(/^a=ice-ufrag:(.+)$/m)?.[1],
                turnUsername: peer.getConfiguration().iceServers?.[0]?.username,
              })),
          );
          try {
            assert.equal(await credentials.evaluate((entries) => entries.length), 2);
            assert.equal(
              await credentials.evaluate((entries) =>
                entries.every(({ ufrag }) => typeof ufrag === 'string'),
              ),
              true,
            );
            restartCurrentTransports();
            await probe.waitForFunction(
              (entries) =>
                entries.every(({ peer, ufrag, turnUsername }) => {
                  const current = peer.remoteDescription?.sdp.match(/^a=ice-ufrag:(.+)$/m)?.[1];
                  return (
                    peer.connectionState === 'connected' &&
                    typeof current === 'string' &&
                    current !== ufrag &&
                    (turnUsername === undefined ||
                      peer.getConfiguration().iceServers?.[0]?.username !== turnUsername)
                  );
                }),
              credentials,
            );
            await waitForMediaStart(owner, receiver);
            report.iceRestart = {
              passed: true,
              identity: await identity.evaluate((state) => state.verify()),
              media: await advancingReconnectMedia(owner, receiver, 3000),
              ...(turnRelay && {
                relay: await waitForRelayPaths(probe, report.relayBeforeRestart),
                returnMedia: await advancingReconnectMedia(probe, identity, 3000),
              }),
            };
          } finally {
            await credentials.dispose();
            await receiver.dispose();
            await identity.dispose();
          }
          await leave(probe);
          await retireTestNetwork(probe);
          await probe.context().close();
          if (turnRelay) {
            await owner.locator('#cam-btn').click();
            await owner.locator('#mic-btn').click();
          }
        };
      },
    );
    await step(
      'a media control lost before server receipt is replayed after reconnect',
      checkUnacknowledgedControl,
    );
    await step(
      'real ICE restarts recover both native transports with working media',
      checkIceRestart,
    );
    await step(
      'simulated capture termination updates both clients and permits explicit restart',
      async () => {
        await guest.locator('#cam-btn').click();
        await guest.locator('#cam-btn:not(.muted)').waitFor({ state: 'visible' });
        await remotePlayback(owner, 'video');
        await guest.locator('#mic-btn').click();
        await guest.locator('#mic-btn:not(.muted)').waitFor({ state: 'visible' });
        await remotePlayback(owner, 'audio');
        report.simulatedCaptureStops = [];
        // Probe an unpublished clone, not the broadcast. Firefox can suppress
        // dispatchEvent() delivery to track.addEventListener() (Mozilla 1473457).
        // Do not replace real handlers or count an unsupported simulation as a pass.
        report.syntheticTrackEndedSupported = await guest.evaluate(() => {
          const stream = document.querySelector('#local-tile video')?.srcObject;
          if (!stream) throw new Error('Expected the active local capture stream');
          return stream.getTracks().every((track) => {
            const probe = track.clone();
            let observed = false;
            probe.addEventListener(
              'ended',
              () => {
                observed = true;
              },
              { once: true },
            );
            probe.stop();
            probe.dispatchEvent(new Event('ended'));
            return observed;
          });
        });
        if (!report.syntheticTrackEndedSupported) {
          await guest.locator('#cam-btn').click();
          await guest.locator('#mic-btn').click();
          return {
            skip: 'Engine suppresses synthetic track ended events; native device termination still needs manual testing.',
          };
        }
        for (const kind of ['audio', 'video']) {
          const stopped = await guest.evaluate((mediaKind) => {
            const stream = document.querySelector('#local-tile video')?.srcObject;
            const tracks =
              stream
                ?.getTracks()
                .filter((track) => track.kind === mediaKind && track.readyState === 'live') || [];
            if (tracks.length !== 1)
              throw new Error(
                `Expected one live owned ${mediaKind} capture track, found ${tracks.length}`,
              );
            const requestsBeforeStop = window.__communityCaptureRequests;
            // stop() is deliberately silent in browsers. Dispatch the external-end
            // event separately; this is not a physical unplug/permission-revoke test.
            tracks[0].stop();
            tracks[0].dispatchEvent(new Event('ended'));
            return { requestsBeforeStop };
          }, kind);
          const button = kind === 'audio' ? '#mic-btn' : '#cam-btn';
          const otherButton = kind === 'audio' ? '#cam-btn' : '#mic-btn';
          await guest.locator(`${button}.muted`).waitFor({ state: 'visible' });
          await visible(
            guest,
            kind === 'audio'
              ? 'Microphone stopped. Press M or the mic button to turn it back on.'
              : 'Camera stopped. Press V or the camera button to turn it back on.',
          );
          assert.equal(
            await guest.locator(`${otherButton}.muted`).count(),
            0,
            'other capture stays enabled',
          );
          await owner.locator(`.video-tile:not(.local) ${kind}`).waitFor({ state: 'detached' });
          const otherKind = kind === 'audio' ? 'video' : 'audio';
          await remotePlayback(owner, otherKind);
          assert.equal(
            await guest.evaluate(() => window.__communityCaptureRequests),
            stopped.requestsBeforeStop,
            'external stop must not recapture',
          );
          await guest.screenshot({
            path: path.join(artifacts, `capture-stopped-${kind}.png`),
            fullPage: true,
          });
          await guest.locator(button).click();
          await guest.locator(`${button}:not(.muted)`).waitFor({ state: 'visible' });
          await remotePlayback(owner, kind);
          assert.equal(
            await guest.evaluate(() => window.__communityCaptureRequests),
            stopped.requestsBeforeStop + 1,
            'explicit restart captures once',
          );
          report.simulatedCaptureStops.push({ kind, stopped: true, restarted: true });
        }
        await guest.locator('#cam-btn').click();
        await guest.locator('#mic-btn').click();
      },
    );
    await step(
      'account profile/avatar appearance is separate from chat and labels both names',
      async () => {
        await publicChat(owner);
        await publicChat(guest);
        await send(owner, 'Before profile appearance');
        const message = guest.locator('.chat-msg').filter({ hasText: 'Before profile appearance' });
        await message.waitFor({ state: 'visible' });
        const ownerName = guest
          .locator('#classic-users-panel [data-participant-hovercard]')
          .filter({ hasText: 'E2E Owner' });
        const rosterColor = await ownerName.evaluate((node) => getComputedStyle(node).color);
        const chatColor = await message
          .locator('.sender')
          .evaluate((node) => getComputedStyle(node).color);
        const account = await header(owner, 'Account');
        await account.getByLabel('Name', { exact: true }).fill('E2E Profile Owner');
        await account.getByLabel('Bio', { exact: true }).fill('Bio from the browser smoke.');
        const appearance = account.locator('.appearance-picker');
        await appearance.getByRole('radio', { name: 'Green', exact: true }).check();
        await appearance.getByRole('radio', { name: 'Tinted background', exact: true }).check();
        await upload(owner, account);
        const saved = owner.waitForResponse(
          (response) =>
            response.url().endsWith('/api/auth/profile') && response.request().method() === 'PATCH',
        );
        await account.getByRole('button', { name: 'Save profile', exact: true }).click();
        assert.equal((await saved).status(), 200);
        await close(account);
        await ownerName.hover();
        const card = guest.locator('.participant-hovercard');
        await card
          .locator('.participant-hovercard-profile-name')
          .filter({ hasText: /^E2E Profile Owner$/ })
          .waitFor();
        await card.getByText('Nickname', { exact: true }).waitFor();
        await card.getByText('Account name', { exact: true }).waitFor();
        assert.equal(await card.locator('.participant-hovercard-name').textContent(), 'E2E Owner');
        assert.equal(
          await card.locator('.appearance-custom').getAttribute('data-appearance'),
          'bubble',
        );
        assert.match(
          await card
            .locator('.appearance-custom')
            .evaluate((node) => getComputedStyle(node).backgroundColor),
          /^rgba\(74, 222, 128, 0\.[1-9]/,
        );
        assert.equal(await ownerName.evaluate((node) => getComputedStyle(node).color), rosterColor);
        await guest.screenshot({ path: path.join(artifacts, 'profile-appearance.png') });
        await guest.keyboard.press('Escape');
        await send(owner, 'After profile appearance');
        const nextMessage = guest
          .locator('.chat-msg')
          .filter({ hasText: 'After profile appearance' });
        await nextMessage.waitFor({ state: 'visible' });
        assert.equal(
          await nextMessage.locator('.sender').evaluate((node) => getComputedStyle(node).color),
          chatColor,
        );
        await action(guest, 'E2E Owner', 'View profile');
        const profile = guest.getByRole('dialog', { name: 'Profile', exact: true });
        await visible(guest, 'E2E Profile Owner');
        await visible(guest, 'Bio from the browser smoke.');
        await profile.getByRole('img').waitFor({ state: 'visible' });
        assert.equal(await profile.getByText(ownerEmail, { exact: true }).count(), 0);
        await close(profile);
      },
    );
    await step(
      'room owners save separate room name and header description appearances',
      async () => {
        const mine = await header(owner, 'My rooms');
        await mine.getByRole('button', { name: 'Edit room', exact: true }).click();
        // Edit room closes My rooms and opens the sheet on the room; each field saves as it changes.
        await mine.waitFor({ state: 'hidden' });
        const edit = await sheetScreen(owner, 'Name & topic');
        const identitySaved = async (action) => {
          const saved = owner.waitForResponse(
            (response) =>
              /\/api\/rooms\/[^/]+\/identity$/.test(new URL(response.url()).pathname) &&
              response.request().method() === 'PATCH',
          );
          await action();
          assert.equal((await saved).status(), 200);
        };
        const nameField = edit.getByLabel('Name', { exact: true });
        await identitySaved(async () => {
          await nameField.fill('E2E Edited Room');
          await nameField.press('Tab');
        });
        const topic = 'Edited live topic. ' + 'Everyone can read the full room topic. '.repeat(8);
        const topicField = edit.getByLabel('Topic', { exact: true });
        await identitySaved(async () => {
          await topicField.fill(topic);
          await topicField.press('Tab');
        });
        const nameAppearance = edit
          .locator('.appearance-picker')
          .filter({ hasText: 'Room name appearance' });
        for (const choice of ['Violet', 'Colored text'])
          await identitySaved(() =>
            nameAppearance.getByRole('radio', { name: choice, exact: true }).check(),
          );
        const topicAppearance = edit
          .locator('.appearance-picker')
          .filter({ hasText: 'Header description appearance' });
        for (const choice of ['Teal', 'Tinted background'])
          await identitySaved(() =>
            topicAppearance.getByRole('radio', { name: choice, exact: true }).check(),
          );
        const description = edit.getByLabel('Description', { exact: true });
        await identitySaved(async () => {
          await description.fill('Browser-tested room description');
          await description.press('Tab');
        });
        await identitySaved(() => upload(owner, edit, true));
        await closeSheet(owner);
        await visible(owner, 'E2E Edited Room');
        await guest.locator('#room-topic').filter({ hasText: topic }).waitFor({ state: 'visible' });
        await guest.waitForFunction(
          () =>
            getComputedStyle(document.querySelector('#room-label')).color === 'rgb(167, 139, 250)',
        );
        assert.match(
          await guest
            .locator('#room-topic')
            .evaluate((node) => getComputedStyle(node).backgroundColor),
          /^rgba\(45, 212, 191, 0\.[1-9]/,
        );
        const topicBounds = await guest.locator('#room-topic').boundingBox();
        const headerBounds = await guest.locator('header').boundingBox();
        assert.ok(topicBounds.width > 0, 'topic remains a visible keyboard-accessible control');
        assert.ok(
          headerBounds.height <= 96 &&
            topicBounds.y + topicBounds.height <= headerBounds.y + headerBounds.height + 1,
          'a long topic keeps the room header compact',
        );
        await guest.locator('#room-topic').focus();
        await guest.keyboard.press('Enter');
        const topicDialog = guest.getByRole('dialog', { name: 'Room topic', exact: true });
        await topicDialog.getByText(topic, { exact: true }).waitFor({ state: 'visible' });
        assert.equal(await topicDialog.getByRole('button', { name: 'Edit topic' }).count(), 0);
        await guest.keyboard.press('Escape');
        await topicDialog.waitFor({ state: 'hidden' });
        assert.equal(
          await guest.locator('#room-topic').evaluate((node) => node === document.activeElement),
          true,
        );
        await header(owner, 'Manage room');
        const reopened = await sheetScreen(owner, 'Name & topic');
        const savedName = reopened
          .locator('.appearance-picker')
          .filter({ hasText: 'Room name appearance' });
        const savedTopic = reopened
          .locator('.appearance-picker')
          .filter({ hasText: 'Header description appearance' });
        assert.equal(
          await savedName.getByRole('radio', { name: 'Violet', exact: true }).isChecked(),
          true,
        );
        assert.equal(
          await savedName.getByRole('radio', { name: 'Colored text', exact: true }).isChecked(),
          true,
        );
        assert.equal(
          await savedTopic.getByRole('radio', { name: 'Teal', exact: true }).isChecked(),
          true,
        );
        assert.equal(
          await savedTopic
            .getByRole('radio', { name: 'Tinted background', exact: true })
            .isChecked(),
          true,
        );
        await close(reopened);
      },
    );
    await step(
      'a room invitation previews its role and requires acceptance before a separate join',
      async () => {
        const mine = await header(owner, 'My rooms');
        await mine.getByRole('button', { name: 'Invites…', exact: true }).click();
        await mine.waitFor({ state: 'hidden' });
        const invites = await sheetScreen(owner, 'Invitations');
        await invites.getByLabel('Uses', { exact: true }).fill('2');
        await invites.getByRole('button', { name: 'Create invitation', exact: true }).click();
        const code = (await invites.locator('.invite-code').first().textContent()).trim();
        assert.match(code, /^[abcdefghjkmnpqrstuvwxyz023456789]{32}$/);
        await invites
          .locator('.management-entry')
          .filter({ hasText: 'member · 2 uses left' })
          .first()
          .waitFor({ state: 'visible' });
        await closeSheet(owner);
        const invitee = await client('invitee');
        const authResponses = [];
        invitee.on('response', (response) => {
          if (
            response.url().includes('/api/auth/') ||
            response.url().includes('/api/rooms/invites/')
          )
            authResponses.push(
              `${response.request().method()} ${new URL(response.url()).pathname} ${response.status()}`,
            );
        });
        await register(invitee, inviteeEmail, 'E2E Invitee');
        await paceJoins(invitee);
        await navigateCommunityPage(invitee, `${base}/#invite=${code}`, clients);
        const review = invitee.getByRole('dialog', { name: 'Review room invitation', exact: true });
        await review
          .getByText(`Room: E2E Edited Room (${runId}). Offered role: member.`, { exact: true })
          .waitFor();
        assert.equal(await invitee.locator('#join-screen').isVisible(), true);
        await review.getByRole('button', { name: 'Accept invitation', exact: true }).click();
        await visible(
          invitee,
          'Invitation confirmed for E2E Edited Room. Current permissions apply. Choose Join when ready.',
        );
        assert.equal(await invitee.locator('#join-screen').isVisible(), true);
        await invitee.locator('#join-btn').click();
        recordJoin();
        await connected(invitee);
        const manage = await header(owner, 'Manage room');
        await visible(owner, 'E2E Invitee · member · online');
        await close(manage);
        await navigateCommunityPage(invitee, `${base}/#invite=${code}`, clients);
        try {
          const review = invitee.getByRole('dialog', {
            name: 'Review room invitation',
            exact: true,
          });
          await review.getByRole('button', { name: 'Accept invitation', exact: true }).click();
          await visible(
            invitee,
            'Invitation confirmed for E2E Edited Room. Current permissions apply.',
          );
          await connected(invitee);
          const manage = await header(owner, 'Manage room');
          await visible(owner, 'E2E Invitee · member · online');
          assert.equal(
            await manage
              .locator('.management-entry')
              .filter({ hasText: 'E2E Invitee · member' })
              .count(),
            1,
          );
          await close(manage);
          const mine = await header(owner, 'My rooms');
          await mine.getByRole('button', { name: 'Invites…', exact: true }).click();
          await mine.waitFor({ state: 'hidden' });
          const invites = await sheetScreen(owner, 'Invitations');
          await invites
            .locator('.management-entry')
            .filter({ hasText: 'member · 1 use left' })
            .waitFor({ state: 'visible' });
          await closeSheet(owner);
        } catch (error) {
          console.log('invitee auth responses:', authResponses.join(' | '));
          throw error;
        }
        await retireTestNetwork(invitee);
        await invitee.close();
      },
    );
    let recoveryKey;
    await step('save-once recovery key and password change signs out current room', async () => {
      await header(owner, 'Account');
      const account = await sheetScreen(owner, 'Sign-in & security');
      await account.getByLabel('Current password for verification', { exact: true }).fill(password);
      await account.getByRole('button', { name: 'Generate recovery key', exact: true }).click();
      const keyField = account.getByLabel('Recovery key', { exact: true });
      recoveryKey = await keyField.inputValue();
      assert.ok(
        /^sc-recovery-/.test(recoveryKey),
        'the displayed recovery key has the expected format',
      );
      const savedField = await keyField.elementHandle();
      assert.ok(savedField, 'the recovery key was displayed in the owned account dialog');
      await account.getByRole('button', { name: 'I saved my recovery key', exact: true }).click();
      await keyField.waitFor({ state: 'detached' });
      assert.equal(
        await savedField.evaluate((node) => node.value === ''),
        true,
        'acknowledgment clears the detached secret',
      );
      await savedField.dispose();
      await account.getByLabel('Current password', { exact: true }).fill(password);
      await account.getByLabel('New password', { exact: true }).fill(`${password}changed`);
      await account.getByLabel('Confirm new password', { exact: true }).fill(`${password}changed`);
      await account.getByRole('button', { name: 'Change password', exact: true }).click();
      await owner.locator('#sign-in-btn').waitFor({ state: 'visible' });
      await login(owner, ownerEmail, `${password}changed`);
      await signOut(owner);
      await owner.locator('#sign-in-btn').waitFor({ state: 'visible' });
    });
    await step('saved key recovery, one-time replay rejection and new-password login', async () => {
      await owner.locator('#sign-in-btn').click();
      await owner.getByRole('button', { name: 'Recover with a saved key', exact: true }).click();
      let recovery = owner.getByRole('dialog', { name: 'Recover account', exact: true });
      for (let attempt = 0; attempt < 2; attempt++) {
        await recovery.getByLabel('Email', { exact: true }).fill(ownerEmail);
        await recovery.getByLabel('Saved recovery key', { exact: true }).fill(recoveryKey);
        await recovery.getByLabel('New password', { exact: true }).fill(`${password}recovered`);
        await recovery
          .getByLabel('Confirm new password', { exact: true })
          .fill(`${password}recovered`);
        await recovery
          .getByRole('button', { name: 'Reset password with key', exact: true })
          .click();
        if (!attempt) {
          await recovery.waitFor({ state: 'hidden' });
          await owner
            .getByRole('button', { name: 'Recover with a saved key', exact: true })
            .click();
          recovery = owner.getByRole('dialog', { name: 'Recover account', exact: true });
        } else {
          await recovery.getByRole('alert').waitFor({ state: 'visible' });
          await close(recovery);
        }
      }
      recoveryKey = '';
      await login(owner, ownerEmail, `${password}recovered`);
      await join(owner, 'E2E Profile Owner');
    });
    await step('mobile layout and community dialog fit viewport', async () => {
      const mobile = await client('mobile', true);
      await join(mobile, 'Mobile Guest');
      assert.ok(
        await mobile.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
        'mobile page must not overflow horizontally',
      );
      await header(mobile, 'Nickname');
      const nickname = mobile.getByRole('dialog', { name: 'Room nickname', exact: true });
      const bounds = await nickname.boundingBox();
      assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 391, 'mobile dialog must fit viewport');
      await mobile.screenshot({
        path: path.join(artifacts, 'mobile-nickname.png'),
        fullPage: true,
      });
      await close(nickname);
      await mobile.getByRole('link', { name: 'simplestChat home', exact: true }).focus();
      await mobile.keyboard.press('Enter');
      await mobile.locator('#join-screen').waitFor({ state: 'visible' });
      assert.equal(new URL(mobile.url()).pathname, '/');
      assert.equal(new URL(mobile.url()).search, '');
      await join(mobile, 'Mobile Guest');
      await leave(mobile);
      await owner.setViewportSize({ width: 390, height: 844 });
      assert.ok(
        await owner.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
        'signed-in mobile header must not overflow',
      );
      const account = await header(owner, 'Account');
      await account.getByRole('button', { name: 'Save profile', exact: true }).waitFor();
      const accountBounds = await account.boundingBox();
      assert.ok(
        accountBounds.x >= 0 && accountBounds.x + accountBounds.width <= 391,
        'mobile account dialog must fit',
      );
      await owner.screenshot({ path: path.join(artifacts, 'mobile-account.png'), fullPage: true });
      await close(account);
      await owner.setViewportSize({ width: 1440, height: 1000 });
      // The chat panel, its composer and its lines follow the resize handle.
      const chatHandle = owner.getByRole('separator', { name: 'Resize chat panel', exact: true });
      await chatHandle.waitFor({ state: 'visible' });
      await chatHandle.focus();
      for (let i = 0; i < 4; i++) await owner.keyboard.press('ArrowLeft');
      await owner.waitForFunction(
        () => document.querySelector('#sidebar-content').getBoundingClientRect().width >= 390,
      );
      const widths = await owner.evaluate(() =>
        ['#sidebar-content', '#chat-panel', '#chat-input-row', '.conversation-toolbar'].map(
          (selector) => Math.round(document.querySelector(selector).getBoundingClientRect().width),
        ),
      );
      assert.ok(widths[0] >= 390, `chat panel widened by keyboard: ${widths.join(',')}`);
      assert.ok(
        widths.every((width) => Math.abs(width - widths[0]) <= 1),
        `chat composer and toolbar follow the panel width: ${widths.join(',')}`,
      );
    });
    await step('owned room typed-confirmation deletion disconnects participants', async () => {
      let mine = await header(owner, 'My rooms');
      await mine.getByRole('button', { name: 'Delete room…', exact: true }).click();
      const remove = owner.getByRole('dialog', { name: 'Delete room', exact: true });
      await remove.getByLabel(`Type ${runId} to confirm`, { exact: true }).fill(runId);
      await remove.getByRole('button', { name: 'Permanently delete room', exact: true }).click();
      await remove.waitFor({ state: 'hidden' });
      await owner.locator('#join-screen').waitFor({ state: 'visible' });
      if (!(await mine.isVisible())) mine = await header(owner, 'My rooms');
      await visible(owner, 'No owned rooms yet. Use Create Room on the join screen.');
      await close(mine);
      await guest.locator('#join-screen').waitFor({ state: 'visible' });
      await member.locator('#join-screen').waitFor({ state: 'visible' });
    });
    await testNetworkReady();
    for (const item of clients) assert.deepEqual(item.errors, [], `${item.label} page errors`);
    report.complete = true;
    report.passed = true;
    console.log(`PASS complete community smoke (${runId}); artifacts: ${artifacts}`);
  } catch (error) {
    report.failedStep = activeStep;
    report.error = error.message;
    if (error.cause?.mediaProgress) report.mediaProgressFailure = error.cause.mediaProgress;
    saveReport();
    report.diagnostics = [];
    for (const item of clients) {
      const media = await deadline(item.page.evaluate(collectPeerDiagnostics)).catch((failure) => ({
        error: failure.message,
      }));
      report.diagnostics.push({
        label: item.label,
        pageErrors: item.errors,
        warnings: item.warnings,
        media,
        pushFixture: await deadline(item.page.evaluate(collectPushFixtureEvidence)).catch(
          () => null,
        ),
      });
      saveReport();
    }
    console.error(`FAIL ${activeStep}`);
    for (const item of clients) {
      await item.page
        .screenshot({
          path: path.join(artifacts, `failure-${item.label}.png`),
          fullPage: true,
          mask: [item.page.locator('input[type="password"], input[readonly], textarea[readonly]')],
        })
        .catch(() => {});
      console.error(`${item.label} page errors:`, item.errors);
      console.error(`${item.label} last signaling events:`, JSON.stringify(item.frames));
    }
    console.error(`Failure screenshots: ${artifacts}`);
    throw error;
  } finally {
    cleanupFailure = await closeCommunityClients({
      clients,
      browser,
      additionalBrowsers,
      report,
      deadline,
      saveReport,
      closeBrowser: runner.closeBrowser,
    });
  }

  if (cleanupFailure) throw cleanupFailure;

  return report;
}

module.exports = { run };
if (require.main === module) {
  try {
    require('./runner-cli.cjs').run('community');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
