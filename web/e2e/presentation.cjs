/** Real encoded presentation text, voice and camera on owned hosted services.
 * No native screen picker or physical/audio-output claim is made. */
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { browserOptions, webKitTestNetworkEnabled } = require('./browser-options.cjs');
const { installPeerEventTracing } = require('./peer-events.cjs');
const { openRoomMenu } = require('./room-menu.cjs');
const { sheetScreen, closeSheet } = require('./settings-sheet.cjs');
const { presentationWindow, presentationProgress } = require('./presentation-window.cjs');
const {
  installPresentationSignalStats,
  presentationReceiverStats,
  presentationSenderStats,
} = require('./presentation-stats.cjs');
const {
  text,
  glyphs,
  installPresentationCapture,
  decodePresentationFrame,
} = require('./presentation-pattern.cjs');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:3119');
const impaired = process.env.PRESENTATION_IMPAIRMENT === '1';
if (
  process.env.PRESENTATION_E2E !== '1' ||
  process.env.DISPOSABLE_TEST_DATABASE !== '1' ||
  !['http:', 'https:'].includes(origin.protocol) ||
  (origin.protocol === 'https:' && process.env.TEST_BROWSER_HTTPS !== '1') ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash ||
  !process.env.E2E_ARTIFACTS ||
  !['0', '1'].includes(process.env.PRESENTATION_IMPAIRMENT || '0') ||
  (impaired && process.platform !== 'linux')
)
  throw new Error(
    'Presentation check requires explicit owned disposable loopback services and Linux for impairment.',
  );
const artifacts = process.env.E2E_ARTIFACTS;
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
const options = browserOptions(process.env.E2E_BROWSER);
const testNetwork = webKitTestNetworkEnabled(options.name);
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const contexts = [],
  clients = [];
const report = {
  browser: options.name,
  passed: false,
  completed: false,
  checks: [],
  failureStage: null,
  impairment: impaired ? { downlinkKbit: 800, delayMs: 30 } : null,
  limitations: [
    'Owned canvas display content and native synthetic camera/microphone; no physical devices, native picker, display audio or audible-output proof.',
    'Text is read from decoded remote video pixels. Native inbound audio sample counters prove playout processing, not speaker audibility.',
    impaired
      ? 'Linux netem changes only owned server media UDP downlink; signaling and publisher uplink are unchanged.'
      : 'No network impairment in this browser compatibility run; data saver still checks native lower-layer delivery.',
    'Default negotiated codec is retained; VP9 SVC capability decisions and unavailable-API fallback also have pure source tests.',
  ],
};
let stage = 'launch',
  browser;
function save() {
  fs.writeFileSync(
    path.join(artifacts, 'presentation-results.json'),
    `${JSON.stringify(report, null, 2)}\n`,
    { mode: 0o600 },
  );
}
function impair(action) {
  if (!impaired) return;
  const output = execFileSync('sh', [path.join(__dirname, '../../build/impair.sh'), action], {
    env: {
      ...process.env,
      IMPAIR_UDP_PORT: process.env.TEST_MEDIA_PORT || '41010',
      IMPAIR_WORKERS: '1',
      IMPAIR_DIRECTION: 'downlink',
      IMPAIR_RATE_KBIT: '800',
      IMPAIR_DELAY_MS: '30',
      IMPAIR_LOSS: '0',
      IMPAIR_JITTER_MS: '0',
    },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  fs.appendFileSync(path.join(artifacts, 'impairment.log'), `${action}\n${output}`);
}
async function join(label, room) {
  const context = await browser.newContext({
    ...options.contextOptions,
    viewport: { width: 2560, height: 1440 },
  });
  contexts.push(context);
  const page = await context.newPage();
  clients.push({ label, page });
  if (testNetwork) {
    const { installWebKitTestNetwork } = require('./webkit-test-network.cjs');
    await installWebKitTestNetwork(playwright, page, { browserName: options.name });
  }
  page.setDefaultTimeout(15000);
  page.on('pageerror', () => {
    report.pageErrors = (report.pageErrors || 0) + 1;
  });
  await page.addInitScript(installPeerEventTracing, {
    announcedIp: process.env.TEST_ANNOUNCE_IP || null,
  });
  await page.addInitScript(installPresentationSignalStats);
  await page.addInitScript(installPresentationCapture, { glyphs, text });
  await page.goto(origin.toString());
  await page.locator('#name-input').fill(label);
  await page.locator('#room-input').fill(room);
  await page.locator('#join-btn').click();
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  await page.waitForFunction(
    () => document.querySelector('#connection-status').textContent === 'Connected',
  );
  assert.deepEqual(
    await page.evaluate(() => ({
      captures: window.__presentation.captures,
      displays: window.__presentation.displays,
    })),
    { captures: 0, displays: 0 },
  );
  return page;
}
async function profile(page, mode) {
  await page.locator('#settings-btn').click();
  const settings = await sheetScreen(page, 'Screen sharing');
  await settings.getByLabel('Optimize for', { exact: true }).selectOption(mode);
  await closeSheet(page);
}
async function share(page) {
  await openRoomMenu(page);
  const wasSharing = await page
    .locator('#screen-btn')
    .evaluate((button) => button.classList.contains('active'));
  await page.locator('#screen-btn').click();
  await page.waitForFunction(
    (previous) => document.querySelector('#screen-btn').classList.contains('active') !== previous,
    wasSharing,
  );
}
async function receiveMode(page, mode) {
  await openRoomMenu(page);
  await page.getByLabel('Incoming video', { exact: true }).selectOption(mode);
}
async function sample(page) {
  const frame = await page.evaluate(decodePresentationFrame, { glyphs, text });
  const counters = await page.evaluate(presentationReceiverStats);
  return { ...frame, ...counters };
}
async function progressing(page, name, accepts, timeoutMs = 30000, windowMs = 3000, publisher) {
  const startedAt = Date.now();
  let lastSenderAt = -Infinity,
    sender = null;
  const window = await presentationWindow({
    sample: async () => {
      const point = await sample(page);
      // Passive bounded diagnostics, including unsuccessful startup/recovery.
      // Sampling never changes encodings, requests layers or resets a peer.
      if (publisher && Date.now() - lastSenderAt >= 2000) {
        lastSenderAt = Date.now();
        sender = { elapsedMs: lastSenderAt - startedAt, ...(await senderState(publisher)) };
      }
      return { ...point, elapsedMs: Date.now() - startedAt, sender };
    },
    accepts: (point) => point.text === text && accepts(point),
    progresses: presentationProgress,
    wait: (ms) => page.waitForTimeout(ms),
    timeoutMs,
    windowMs,
    observe: (point) => {
      report.lastObservation = { name, ...point };
      report.observations ||= [];
      report.observations.push({
        name,
        width: point.width,
        height: point.height,
        readableText: point.text === text,
        screenFrames: point.screenFrames,
        cameraFrames: point.cameraFrames,
        audioSamples: point.audioSamples,
        elapsedMs: point.elapsedMs,
        availableIncomingBitrate: point.availableIncomingBitrate,
        serverAvailableBitrate: point.serverAvailableBitrate,
        serverEstimateAgeMs: point.serverEstimateAgeMs,
        screenInbound: point.screenInbound,
        sender: point.sender,
      });
      if (report.observations.length > 128) report.observations.shift();
    },
  });
  const { before, after } = window;
  assert.equal(before.text, text, `${name}: actual decoded text starts readable`);
  assert.equal(after.text, text, `${name}: actual decoded text remains readable`);
  assert.ok(after.screenFrames >= before.screenFrames + 2, `${name}: screen frames advance`);
  assert.ok(
    after.cameraFrames >= before.cameraFrames + 2,
    `${name}: simultaneous camera frames advance`,
  );
  assert.ok(after.audioPackets >= before.audioPackets + 10, `${name}: voice packets arrive`);
  assert.ok(
    before.audioSamples !== null && after.audioSamples >= before.audioSamples + 4800,
    `${name}: native audio sample processing advances`,
  );
  const samples = after.audioSamples - before.audioSamples;
  const concealed =
    before.concealedSamples !== null && after.concealedSamples !== null
      ? after.concealedSamples - before.concealedSamples
      : null;
  assert.ok(
    concealed === null || concealed / samples < 0.25,
    `${name}: most voice samples are received, not concealed`,
  );
  report.checks.push({
    name,
    passed: true,
    decodedText: after.text,
    width: after.width,
    height: after.height,
    screenFrames: after.screenFrames - before.screenFrames,
    cameraFrames: after.cameraFrames - before.cameraFrames,
    audioPackets: after.audioPackets - before.audioPackets,
    audioSamples: samples,
    concealedSamples: concealed,
    before,
    after,
    durationMs: window.durationMs,
    resolutionSamples: window.observations.map(({ width, height }) => ({ width, height })),
  });
  await page.screenshot({ path: path.join(artifacts, `${name}.png`) });
  save();
  return after;
}
async function senderState(page) {
  return page.evaluate(presentationSenderStats);
}
(async () => {
  try {
    browser = await playwright[options.name].launch(options.launchOptions);
    const room = `presentation-${Date.now().toString(36)}`;
    const publisher = await join('Presenter', room),
      viewer = await join('Reader', room);
    stage = 'detail-publication';
    await publisher.locator('#cam-btn').click();
    await publisher.locator('#mic-btn').click();
    await profile(publisher, 'detail');
    await share(publisher);
    await progressing(
      viewer,
      'detail-text-voice-camera',
      (read) => read.height === 1080,
      30000,
      3000,
      publisher,
    );
    const detail = await senderState(publisher);
    const hardwareSingle =
      detail.screenCodec?.toLowerCase() === 'video/h264' && detail.screenSpatialStreams === 1;
    const svc = /^L3T/.test(detail.screen[0]?.scalabilityMode || '');
    if (!hardwareSingle && !svc)
      assert.equal(detail.screen.length, 3, 'default VP8/H264 creates three screen encodings');
    assert.ok(
      detail.camera.length >= 3 && detail.camera[2].active === false,
      'presentation reserves uplink by disabling the largest existing camera stream',
    );
    report.screenEncodings = detail.screen;
    report.screenCodec = detail.screenCodec;
    report.encodedSpatialStreams = detail.screenSpatialStreams;
    report.hardwareSingleStreamFallback = hardwareSingle;
    assert.ok(
      !impaired || !hardwareSingle,
      'network adaptation proof needs actual layers; H264 hardware fallback is covered in browser compatibility',
    );
    const captures = detail.captures;
    stage = 'data-saver';
    await receiveMode(viewer, 'data-saver');
    await progressing(
      viewer,
      'data-saver-readable-text',
      (read) => hardwareSingle || read.height <= 270,
    );
    stage = 'explicit-audio-only';
    await receiveMode(viewer, 'audio-only');
    await viewer.waitForFunction(
      () => document.querySelectorAll('.video-tile:not(.local) video').length === 0,
    );
    const audioBefore = await sample(viewer);
    await viewer.waitForTimeout(2500);
    const audioAfter = await sample(viewer);
    assert.ok(
      audioAfter.audioPackets > audioBefore.audioPackets &&
        audioAfter.audioSamples > audioBefore.audioSamples,
    );
    report.checks.push({
      name: 'audio-only-keeps-voice-without-screen-subscription',
      passed: true,
    });
    await receiveMode(viewer, 'balanced');
    stage = 'motion-publication';
    await share(publisher); // Explicit stop, then a new explicit share adopts the new profile.
    await profile(publisher, 'motion');
    await share(publisher);
    const high = await progressing(
      viewer,
      'motion-readable-text',
      (read) => read.height === 1080,
      45000,
      3000,
      publisher,
    );
    if (impaired) {
      stage = 'constrained-downlink';
      impair('apply');
      // Allow the real estimator and worker to converge, then require progress
      // over an independent interval. Pixel reads alone could accept a freeze.
      await viewer.waitForTimeout(20000);
      const low = await progressing(
        viewer,
        'constrained-text-voice-camera',
        (read) => read.height < high.height,
        30000,
        4000,
        publisher,
      );
      report.constrainedReference = { width: low.width, height: low.height };
      stage = 'network-recovery';
      impair('clear');
      await progressing(
        viewer,
        'recovered-text-voice-camera',
        (read) => read.height === 1080 && read.height > low.height,
        45000,
        3000,
        publisher,
      );
    }
    stage = 'stop-restores-camera';
    await share(publisher);
    await publisher.waitForFunction(() => {
      const cameras = window.__communityPeers
        .flatMap((peer) => peer.getSenders())
        .filter(
          (sender) =>
            sender.track?.kind === 'video' && !window.__presentation.tracks.includes(sender.track),
        );
      return cameras.some(
        (sender) =>
          sender.getParameters().encodings.length >= 3 &&
          sender.getParameters().encodings.every((encoding) => encoding.active !== false),
      );
    });
    const final = await senderState(publisher);
    assert.equal(
      final.captures,
      captures,
      'profile, receive mode and restoration do not request fresh camera/microphone capture',
    );
    assert.equal(final.displays, 2, 'only the two explicit Share screen actions acquire a display');
    assert.equal(
      await viewer.evaluate(() => window.__presentation.captures + window.__presentation.displays),
      0,
    );
    assert.equal(report.pageErrors || 0, 0);
    report.checks.push({ name: 'stop-restores-camera-and-zero-automatic-capture', passed: true });
    report.passed = true;
    report.completed = true;
  } catch (error) {
    report.failureStage = stage;
    report.failure = { message: error.message };
    for (const { label, page } of clients)
      await page.screenshot({ path: path.join(artifacts, `failure-${label}.png`) }).catch(() => {});
    process.exitCode = 1;
  } finally {
    try {
      impair('clear');
    } catch (error) {
      report.failure ||= { message: error.message };
      report.passed = false;
      process.exitCode = 1;
    }
    save();
    await Promise.allSettled(contexts.map((context) => context.close()));
    await browser?.close();
  }
  assert.ok(report.passed, 'Presentation scenario failed; see presentation-results.json');
  console.log(`PASS presentation (${options.name})`);
})();
