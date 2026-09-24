/**
 * Adaptive-path check under an impaired downlink: a publisher sends simulcast
 * from a fake camera, a viewer receives, and build/impair.sh degrades the UDP
 * packets leaving the server's media port between phases. The viewer's native
 * inbound statistics and the client's layer-change log show whether decoding
 * continues under loss and whether the server steps the consumer's spatial
 * layer down under a bandwidth cap and back up after it lifts.
 *
 * Environment: BASE_URL (owned test server, default http://127.0.0.1:3109),
 * E2E_ARTIFACTS, E2E_BROWSER (chromium), IMPAIR_SCRIPT (build/impair.sh, or
 * "none" to exercise the mechanics without impairment), IMPAIR_UDP_PORT and
 * IMPAIR_WORKERS (the server's media ports), IMPAIRED_PROFILES (comma list of
 * baseline, lossy, constrained, severe, recovery, lossyJoin; default all),
 * IMPAIRED_ROOM (join this existing room instead of a new one, for canaries),
 * IMPAIRED_SILENT_AUDIO=1 (capture silence instead of the fake tone, for the
 * silentAudio profile), IMPAIRED_BLOCK_UDP=1 (drop UDP to and from the media
 * port before any client joins, so every client must connect over ICE-TCP;
 * the server needs WEBRTC_SERVER_TCP=true and only the baseline profile runs).
 * CANARY_CORRELATION_FILE optionally writes private numeric RTP identities for
 * a two-client baseline canary; absolute exclusive path outside E2E_ARTIFACTS,
 * unavailable in GitHub Actions. Keep it outside any parent upload directory.
 */
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { browserOptions } = require('./browser-options.cjs');
const { installPeerEventTracing } = require('./peer-events.cjs');
const { installSignalingReconnectObservation } = require('./signaling-reconnect.cjs');
const { collectCanaryMediaSample, openCanaryCorrelation } = require('./canary-correlation.cjs');

const base = process.env.BASE_URL || 'http://127.0.0.1:3109';
const impairScript =
  process.env.IMPAIR_SCRIPT || path.join(__dirname, '..', '..', 'build', 'impair.sh');
const impairmentEnabled = impairScript !== 'none';
const artifacts =
  process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-impaired.'));
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
// A fixed room (IMPAIRED_ROOM) lets the scenario run as a canary against a
// deployment whose ad-hoc rooms are closed; otherwise each run owns a new room.
const runId = process.env.IMPAIRED_ROOM || `impair-${Date.now().toString(36)}`;

/** Downlink profiles; assertions name the production change that would break them. */
const PROFILES = {
  baseline: { env: null, seconds: 15 },
  lossy: {
    env: { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10' },
    seconds: 25,
  },
  constrained: { env: { IMPAIR_RATE_KBIT: '400', IMPAIR_DELAY_MS: '30' }, seconds: 45 },
  severe: { env: { IMPAIR_RATE_KBIT: '150', IMPAIR_DELAY_MS: '30' }, seconds: 45 },
  recovery: { env: null, seconds: 45 },
  // A third client joins while both directions are lossy: connection
  // establishment (ICE, DTLS, first keyframe) under loss, not just steady state.
  lossyJoin: {
    env: {
      IMPAIR_LOSS: '5',
      IMPAIR_DELAY_MS: '50',
      IMPAIR_JITTER_MS: '10',
      IMPAIR_DIRECTION: 'both',
    },
    seconds: 20,
    join: true,
  },
  // With IMPAIRED_SILENT_AUDIO=1 the publisher's microphone is a file of
  // silence, so Opus DTX must reduce its packet rate to a few per second.
  silentAudio: { env: null, seconds: 20, silent: true },
};
const silentAudio = process.env.IMPAIRED_SILENT_AUDIO === '1';
const forceRelay = process.env.CANARY_FORCE_RELAY === '1';
const publicCanary = process.env.CANARY_MODE === '1';
const callOutcomes = process.env.CALL_OUTCOME_E2E === '1';
if (callOutcomes) {
  const origin = new URL(base);
  if (
    process.env.DISPOSABLE_TEST_DATABASE !== '1' ||
    publicCanary ||
    origin.protocol !== 'http:' ||
    !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
    origin.username ||
    origin.password ||
    origin.pathname !== '/' ||
    origin.search ||
    origin.hash ||
    impairmentEnabled ||
    silentAudio
  )
    throw new Error(
      'Call outcome regression requires owned loopback services, fake tone audio and IMPAIR_SCRIPT=none',
    );
}
const blockUdp = process.env.IMPAIRED_BLOCK_UDP === '1';
if (blockUdp && !impairmentEnabled)
  throw new Error('IMPAIRED_BLOCK_UDP needs the impairment script (IMPAIR_SCRIPT is none)');
// The silent-microphone phase only makes sense with a silent capture, so it
// joins the default list only when that is configured. With UDP blocked the
// netem profiles have nothing to shape, so only the clean phase runs.
const defaultProfiles = blockUdp
  ? ['baseline']
  : Object.keys(PROFILES).filter((name) => silentAudio || name !== 'silentAudio');
const profiles = (process.env.IMPAIRED_PROFILES || defaultProfiles.join(','))
  .split(',')
  .map((name) => name.trim())
  .filter(Boolean);
for (const name of profiles) if (!PROFILES[name]) throw new Error(`Unknown profile ${name}`);
const correlation = openCanaryCorrelation({
  file: process.env.CANARY_CORRELATION_FILE,
  artifacts,
  enabled:
    publicCanary &&
    !callOutcomes &&
    !blockUdp &&
    !silentAudio &&
    Boolean(process.env.IMPAIRED_ROOM),
  profiles,
  impairmentEnabled,
  githubActions: process.env.GITHUB_ACTIONS === 'true',
});
const consumerOrdinals = new Map();
const pendingCorrelation = new WeakMap();

const report = {
  schemaVersion: 2,
  runId,
  startedAt: new Date().toISOString(),
  base,
  forcedRelay: forceRelay,
  impairment: impairmentEnabled
    ? { script: impairScript, udpBlocked: blockUdp }
    : { script: null, note: 'mechanics only' },
  browser: null,
  passed: false,
  completed: false,
  phases: [],
  layerEvents: [],
  droppedLayerEvents: 0,
  callOutcomes: [],
  limitations: [
    publicCanary
      ? 'Two owned headless browser clients over the public network; synthetic camera and microphone, no physical device coverage.'
      : 'Two owned headless browser clients against the selected test server; synthetic capture, no physical device coverage.',
    'Impairment covers UDP leaving the server media port; signaling and the publisher uplink are untouched.',
    'The impairment script needs sudo and a platform that filters loopback (Linux netem); on macOS pf dummynet does not affect loopback and lossy/constrained assertions fail.',
    'Freeze counters are reported when the browser exposes them; absence is not a pass.',
  ],
  failure: null,
};
function save() {
  if (correlation) report.privateCorrelation = correlation.summary();
  fs.writeFileSync(
    path.join(artifacts, 'impaired-network-results.json'),
    `${JSON.stringify(report, null, 2)}\n`,
    { mode: 0o600 },
  );
}
function impair(action, env) {
  if (!impairmentEnabled) return;
  const output = execFileSync('sh', [impairScript, action], {
    env: {
      ...process.env,
      IMPAIR_UDP_PORT: process.env.IMPAIR_UDP_PORT || '41100',
      IMPAIR_WORKERS: process.env.IMPAIR_WORKERS || '1',
      IMPAIR_DIRECTION: 'downlink',
      ...(env || {}),
    },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  fs.appendFileSync(
    path.join(artifacts, 'impairment.log'),
    `${new Date().toISOString()} ${action}\n${output}`,
  );
}

/** Native inbound video/audio counters of every open peer, plus the selected pair's RTT. */
async function sample(page) {
  return page.evaluate(async () => {
    const totals = {
      transportProtocols: [],
      localCandidateTypes: [],
      framesDecoded: 0,
      framesDropped: 0,
      frameWidth: 0,
      frameHeight: 0,
      packetsReceived: 0,
      packetsLost: 0,
      nackCount: 0,
      pliCount: 0,
      firCount: 0,
      keyFramesDecoded: 0,
      freezeCount: null,
      totalFreezesDuration: null,
      audioPacketsReceived: 0,
      audioPacketsLost: 0,
      roundTripTime: null,
      videoReports: 0,
      // mediasoup-client's bandwidth probator (SSRC 1234) never carries a
      // decodable frame, so the browser keeps asking it for keyframes; count
      // it apart from the real stream.
      probatorPliCount: 0,
    };
    for (const peer of window.__communityPeers || []) {
      if (peer.connectionState === 'closed') continue;
      const stats = await peer.getStats();
      const selectedPairs = new Set(
        [...stats.values()]
          .filter((report) => report.type === 'transport' && report.selectedCandidatePairId)
          .map((report) => report.selectedCandidatePairId),
      );
      for (const stat of stats.values()) {
        if (stat.type === 'inbound-rtp' && stat.kind === 'video' && stat.ssrc === 1234) {
          totals.probatorPliCount += stat.pliCount || 0;
          continue;
        }
        if (stat.type === 'inbound-rtp' && stat.kind === 'video') {
          totals.videoReports++;
          totals.framesDecoded += stat.framesDecoded || 0;
          totals.framesDropped += stat.framesDropped || 0;
          totals.frameWidth = Math.max(totals.frameWidth, stat.frameWidth || 0);
          totals.frameHeight = Math.max(totals.frameHeight, stat.frameHeight || 0);
          totals.packetsReceived += stat.packetsReceived || 0;
          totals.packetsLost += stat.packetsLost || 0;
          totals.nackCount += stat.nackCount || 0;
          totals.pliCount += stat.pliCount || 0;
          totals.firCount += stat.firCount || 0;
          totals.keyFramesDecoded += stat.keyFramesDecoded || 0;
          if (Number.isFinite(stat.freezeCount))
            totals.freezeCount = (totals.freezeCount || 0) + stat.freezeCount;
          if (Number.isFinite(stat.totalFreezesDuration))
            totals.totalFreezesDuration =
              (totals.totalFreezesDuration || 0) + stat.totalFreezesDuration;
        }
        if (stat.type === 'inbound-rtp' && stat.kind === 'audio') {
          totals.audioPacketsReceived += stat.packetsReceived || 0;
          totals.audioPacketsLost += stat.packetsLost || 0;
        }
        if (
          stat.type === 'candidate-pair' &&
          selectedPairs.has(stat.id) &&
          Number.isFinite(stat.currentRoundTripTime)
        ) {
          totals.roundTripTime = stat.currentRoundTripTime;
        }
        if (stat.type === 'candidate-pair' && selectedPairs.has(stat.id)) {
          const remote = stats.get(stat.remoteCandidateId);
          if (remote && remote.protocol) totals.transportProtocols.push(remote.protocol);
          const local = stats.get(stat.localCandidateId);
          totals.localCandidateTypes.push(local?.candidateType ?? 'unknown');
        }
      }
    }
    totals.sampledAtMs = performance.now();
    return totals;
  });
}

/** Publisher-side encoder counters: keyframes and PLIs received per simulcast layer. */
async function samplePublisher(page) {
  return page.evaluate(async () => {
    const identities = (window.__canaryPublisherLayers ??= { peers: new WeakMap(), next: 0 });
    const layers = {};
    const transportProtocols = [];
    const localCandidateTypes = [];
    let keyFramesEncoded = 0;
    let pliCount = 0;
    let framesEncoded = 0;
    for (const peer of window.__communityPeers || []) {
      if (peer.connectionState === 'closed') continue;
      const stats = await peer.getStats();
      let layerNames = identities.peers.get(peer);
      if (!layerNames) {
        layerNames = new Map();
        identities.peers.set(peer, layerNames);
      }
      const selectedPairs = new Set(
        [...stats.values()]
          .filter((report) => report.type === 'transport' && report.selectedCandidatePairId)
          .map((report) => report.selectedCandidatePairId),
      );
      for (const stat of stats.values()) {
        if (stat.type === 'candidate-pair' && selectedPairs.has(stat.id)) {
          const remote = stats.get(stat.remoteCandidateId);
          if (remote && remote.protocol) transportProtocols.push(remote.protocol);
          const local = stats.get(stat.localCandidateId);
          localCandidateTypes.push(local?.candidateType ?? 'unknown');
        }
        if (stat.type !== 'outbound-rtp' || stat.kind !== 'video') continue;
        keyFramesEncoded += stat.keyFramesEncoded || 0;
        pliCount += stat.pliCount || 0;
        framesEncoded += stat.framesEncoded || 0;
        let layerName = layerNames.get(stat.id);
        if (!layerName) {
          if (identities.next === 64) continue;
          layerName = `layer-${++identities.next}`;
          layerNames.set(stat.id, layerName);
        }
        layers[layerName] = {
          active: stat.active,
          frameWidth: stat.frameWidth || 0,
          frameHeight: stat.frameHeight || 0,
          framesPerSecond: stat.framesPerSecond || 0,
          targetBitrate: stat.targetBitrate || 0,
          bytesSent: stat.bytesSent || 0,
          keyFramesEncoded: stat.keyFramesEncoded || 0,
          pliCount: stat.pliCount || 0,
          qualityLimitationReason: stat.qualityLimitationReason || null,
        };
      }
    }
    return {
      keyFramesEncoded,
      pliCount,
      framesEncoded,
      layers,
      transportProtocols,
      localCandidateTypes,
      sampledAtMs: performance.now(),
    };
  });
}

/** A mono 16-bit PCM WAV of digital silence, looped by Chromium's fake capture. */
function silentWav(sampleRate, seconds) {
  const frames = sampleRate * seconds;
  const data = Buffer.alloc(frames * 2);
  const header = Buffer.alloc(44);
  header.write('RIFF', 0);
  header.writeUInt32LE(36 + data.length, 4);
  header.write('WAVE', 8);
  header.write('fmt ', 12);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(sampleRate, 24);
  header.writeUInt32LE(sampleRate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write('data', 36);
  header.writeUInt32LE(data.length, 40);
  return Buffer.concat([header, data]);
}

function delta(before, after) {
  const seconds = (after.sampledAtMs - before.sampledAtMs) / 1000;
  return {
    seconds,
    framesDecoded: after.framesDecoded - before.framesDecoded,
    framesPerSecond: (after.framesDecoded - before.framesDecoded) / seconds,
    packetsReceived: after.packetsReceived - before.packetsReceived,
    packetsLost: after.packetsLost - before.packetsLost,
    nackCount: after.nackCount - before.nackCount,
    pliCount: after.pliCount - before.pliCount,
    firCount: after.firCount - before.firCount,
    keyFramesDecoded: after.keyFramesDecoded - before.keyFramesDecoded,
    probatorPliCount: after.probatorPliCount - before.probatorPliCount,
    freezeCount:
      after.freezeCount === null ||
      after.freezeCount === undefined ||
      before.freezeCount === null ||
      before.freezeCount === undefined
        ? null
        : after.freezeCount - before.freezeCount,
    totalFreezesDuration:
      after.totalFreezesDuration === null ||
      after.totalFreezesDuration === undefined ||
      before.totalFreezesDuration === null ||
      before.totalFreezesDuration === undefined
        ? null
        : after.totalFreezesDuration - before.totalFreezesDuration,
    audioPacketsLost: after.audioPacketsLost - before.audioPacketsLost,
    audioPacketsPerSecond: (after.audioPacketsReceived - before.audioPacketsReceived) / seconds,
  };
}

async function main() {
  const options = browserOptions(process.env.E2E_BROWSER);
  const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
  if (silentAudio) {
    if (options.name !== 'chromium')
      throw new Error('IMPAIRED_SILENT_AUDIO needs Chromium fake capture from a file');
    const wav = path.join(artifacts, 'silence.wav');
    fs.writeFileSync(wav, silentWav(48000, 5));
    options.launchOptions.args = [
      ...(options.launchOptions.args || []),
      `--use-file-for-fake-audio-capture=${wav}`,
    ];
  }
  const browser = await playwright[options.name].launch(options.launchOptions);
  report.browser = { name: options.name, version: browser.version() };
  const started = performance.now();
  const contexts = [];
  async function client(label) {
    const context = await browser.newContext({
      ...options.contextOptions,
      viewport: { width: 1280, height: 900 },
    });
    contexts.push(context);
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    await page.addInitScript(installPeerEventTracing, {
      announcedIp: process.env.TEST_ANNOUNCE_IP || null,
      forceRelay,
    });
    if (callOutcomes) {
      await page.addInitScript(() => localStorage.setItem('reliabilityTelemetry', 'true'));
      await page.addInitScript(installSignalingReconnectObservation);
    }
    page.on('console', (message) => {
      const match = /\[room\] consumer (\S+) layers: spatial=(\S+), temporal=(\S+)/.exec(
        message.text(),
      );
      if (match) {
        let consumerOrdinal = consumerOrdinals.get(match[1]);
        if (!consumerOrdinal) {
          if (consumerOrdinals.size === 64) {
            report.droppedLayerEvents++;
            return;
          }
          consumerOrdinal = consumerOrdinals.size + 1;
          consumerOrdinals.set(match[1], consumerOrdinal);
        }
        if (report.layerEvents.length === 256) {
          report.droppedLayerEvents++;
          return;
        }
        report.layerEvents.push({
          client: label,
          atSeconds: (performance.now() - started) / 1000,
          consumerOrdinal,
          spatial: match[2] === 'undefined' || match[2] === 'null' ? null : Number(match[2]),
          temporal: match[3] === 'undefined' || match[3] === 'null' ? null : Number(match[3]),
        });
      }
    });
    page.on('dialog', (dialog) => dialog.dismiss());
    await page.goto(base, { waitUntil: 'networkidle' });
    await page.locator('#name-input').fill(label);
    await page.locator('#room-input').fill(runId);
    await page.locator('#join-btn').click();
    await page.locator('#room-screen').waitFor({ state: 'visible' });
    await page.waitForFunction(
      () => document.querySelector('#connection-status').textContent === 'Connected',
    );
    return page;
  }
  async function correlate(publisher, viewer) {
    if (!correlation) return;
    await Promise.all(
      [
        ['publisher', publisher],
        ['viewer', viewer],
      ].map(async ([role, page]) => {
        const clock = { startedAtMs: performance.now() - started, epochAtStartMs: Date.now() };
        let sample;
        let timer;
        try {
          if (pendingCorrelation.has(page)) {
            sample = { status: 'incomplete', issues: ['collection_pending'], streams: [] };
          } else {
            const request = page
              .evaluate(collectCanaryMediaSample)
              .catch(() => ({ status: 'incomplete', issues: ['collection_failed'], streams: [] }))
              .finally(() => pendingCorrelation.delete(page));
            pendingCorrelation.set(page, request);
            sample = await Promise.race([
              request,
              new Promise((resolve) => {
                timer = setTimeout(
                  () =>
                    resolve({ status: 'incomplete', issues: ['collection_timeout'], streams: [] }),
                  2000,
                );
              }),
            ]);
          }
        } catch {
          sample = { status: 'incomplete', issues: ['collection_failed'], streams: [] };
        } finally {
          clearTimeout(timer);
        }
        clock.finishedAtMs = performance.now() - started;
        clock.epochAtFinishMs = Date.now();
        correlation.add(role, sample, clock);
      }),
    );
  }
  async function expectCall(page, name, outcome) {
    const deadline = performance.now() + 15000;
    while (performance.now() < deadline) {
      await page.locator('#diagnostics-btn').click();
      const dialog = page.getByRole('dialog', { name: 'Diagnostic summary', exact: true });
      const summary = JSON.parse(
        await dialog.getByLabel('Diagnostic summary preview').inputValue(),
      );
      await dialog.getByRole('button', { name: 'Close', exact: true }).click();
      const terminal = summary.events.filter(
        (event) => event.name === name && event.outcome !== 'started',
      );
      if (terminal.length) {
        assert.equal(terminal.length, 1, 'exactly one terminal result per owned call attempt');
        assert.equal(terminal[0].outcome, outcome);
        assert.ok(terminal[0].durationMs >= 0 && terminal[0].durationMs <= 30000);
        report.callOutcomes.push({ name, outcome, durationMs: terminal[0].durationMs });
        return;
      }
      await page.waitForTimeout(250);
    }
    throw new Error(`Missing ${name} ${outcome} observation`);
  }
  try {
    // With UDP dropped before anyone joins, ICE can only succeed over the
    // server's TCP candidates; every client and both directions prove it.
    if (blockUdp) impair('block-udp');
    const publisher = await client('publisher');
    if (callOutcomes) await expectCall(publisher, 'call_join', 'no_media_expected');
    // The first camera request opens the capture settings dialog; saving it
    // with the fake devices selected starts the camera.
    await publisher.locator('#cam-btn').click();
    const setup = publisher.getByRole('dialog', { name: 'Your settings', exact: true });
    if (
      await setup.waitFor({ state: 'visible', timeout: 3000 }).then(
        () => true,
        () => false,
      )
    ) {
      await setup.getByRole('button', { name: 'Save settings', exact: true }).click();
      await setup.waitFor({ state: 'hidden' });
    }
    await publisher.waitForFunction(
      () =>
        [...document.querySelectorAll('.video-tile.local video')].some(
          (element) => element.videoWidth > 0,
        ),
      null,
      { timeout: 20000 },
    );
    if (!callOutcomes) await publisher.locator('#mic-btn').click();
    await publisher.waitForTimeout(1200);
    const viewer = await client('viewer');
    await viewer.waitForFunction(
      () =>
        [...document.querySelectorAll('.video-tile:not(.local) video')].some(
          (element) => !element.paused && element.videoWidth > 0,
        ),
      null,
      { timeout: 30000 },
    );
    await viewer.waitForTimeout(3000);
    if (callOutcomes) {
      await expectCall(viewer, 'call_join', 'video_ready');
      await publisher.locator('#mic-btn').click();
    }
    let baselineWidth = null;
    let baselineFps = null;
    for (const name of profiles) {
      const profile = PROFILES[name];
      const phase = {
        name,
        seconds: profile.seconds,
        impairment: profile.env,
        startedAtSeconds: (performance.now() - started) / 1000,
        assertions: [],
        passed: false,
      };
      report.phases.push(phase);
      save();
      const check = (label, condition, detail) => {
        phase.assertions.push({ label, passed: Boolean(condition), detail: detail ?? null });
        return Boolean(condition);
      };
      if (name === 'recovery' || !profile.env) impair('clear');
      else impair('apply', profile.env);
      let latecomer = null;
      if (profile.join) {
        const joinStarted = performance.now();
        try {
          latecomer = await client('latecomer');
          await latecomer.waitForFunction(
            () =>
              [...document.querySelectorAll('.video-tile:not(.local) video')].some(
                (element) => !element.paused && element.videoWidth > 0,
              ),
            null,
            { timeout: 30000 },
          );
          phase.secondsToFirstFrame = (performance.now() - joinStarted) / 1000;
        } catch (error) {
          phase.joinFailure = error.message.split('\n')[0];
        }
      }
      const before = await sample(viewer);
      const beforeAt = (performance.now() - started) / 1000;
      const publisherBefore = await samplePublisher(publisher);
      await correlate(publisher, viewer);
      const series = [];
      const layerEventsBefore = report.layerEvents.length;
      const end = performance.now() + profile.seconds * 1000;
      while (performance.now() < end) {
        await viewer.waitForTimeout(1000);
        const current = await sample(viewer);
        await correlate(publisher, viewer);
        series.push({
          atSeconds: (performance.now() - started) / 1000,
          frameWidth: current.frameWidth,
          framesDecoded: current.framesDecoded,
          keyFramesDecoded: current.keyFramesDecoded,
          packetsLost: current.packetsLost,
          nackCount: current.nackCount,
          pliCount: current.pliCount,
          videoReports: current.videoReports,
          roundTripTime: current.roundTripTime,
        });
      }
      const after = await sample(viewer);
      const publisherAfter = await samplePublisher(publisher);
      await correlate(publisher, viewer);
      const change = delta(before, after);
      phase.publisher = {
        keyFramesEncoded: publisherAfter.keyFramesEncoded - publisherBefore.keyFramesEncoded,
        pliReceived: publisherAfter.pliCount - publisherBefore.pliCount,
        framesEncoded: publisherAfter.framesEncoded - publisherBefore.framesEncoded,
        layers: publisherAfter.layers,
      };
      const widths = series.map((point) => point.frameWidth);
      const layerEvents = report.layerEvents
        .slice(layerEventsBefore)
        .filter((event) => event.client === 'viewer');
      // The layer in force when the phase began: a phase that starts already
      // stepped down (the previous impairment left it there) has no new event
      // to show, so the entering layer counts as well.
      const enteringSpatial =
        report.layerEvents
          .slice(0, layerEventsBefore)
          .filter((event) => event.client === 'viewer' && event.spatial !== null)
          .at(-1)?.spatial ?? null;
      const lowestSpatial = layerEvents.reduce(
        (lowest, event) =>
          event.spatial !== null &&
          event.spatial !== undefined &&
          (lowest === null || lowest === undefined || event.spatial < lowest)
            ? event.spatial
            : lowest,
        enteringSpatial,
      );
      const lastSpatial = layerEvents.length ? layerEvents.at(-1).spatial : null;
      Object.assign(phase, {
        delta: change,
        enteringSpatial,
        minFrameWidth: Math.min(...widths),
        maxFrameWidth: Math.max(...widths),
        layerEvents,
        series,
        roundTripTimes: series
          .map((point) => point.roundTripTime)
          .filter((rtt) => rtt !== null && rtt !== undefined),
      });
      let ok = check(
        'video keeps decoding',
        change.framesDecoded > 0 && after.videoReports > 0,
        change.framesPerSecond,
      );
      if (forceRelay) {
        const viewer = after.localCandidateTypes;
        const publisher = publisherAfter.localCandidateTypes;
        ok =
          check(
            'publisher and viewer selected TURN relay candidates',
            viewer.length > 0 &&
              publisher.length > 0 &&
              [...viewer, ...publisher].every((kind) => kind === 'relay'),
            { viewer, publisher },
          ) && ok;
      }
      if (publicCanary && !forceRelay) {
        const viewer = after.localCandidateTypes;
        const publisher = publisherAfter.localCandidateTypes;
        ok =
          check(
            'publisher and viewer selected direct candidates',
            viewer.length > 0 &&
              publisher.length > 0 &&
              [...viewer, ...publisher].every((kind) => ['host', 'srflx', 'prflx'].includes(kind)),
            { viewer, publisher },
          ) && ok;
      }
      if (blockUdp) {
        const viewerProtocols = [...new Set(after.transportProtocols)];
        const publisherProtocols = [...new Set(publisherAfter.transportProtocols)];
        ok =
          check(
            'every transport runs over ICE-TCP while UDP is blocked',
            viewerProtocols.length > 0 &&
              publisherProtocols.length > 0 &&
              [...viewerProtocols, ...publisherProtocols].every((protocol) => protocol === 'tcp'),
            { viewerProtocols, publisherProtocols },
          ) && ok;
      }
      if (name === 'baseline') {
        baselineWidth = phase.maxFrameWidth;
        baselineFps = change.framesPerSecond;
        if (publicCanary) {
          let lastAdvance = beforeAt;
          let frames = before.framesDecoded;
          let longestGap = 0;
          let reset = false;
          for (const point of [
            ...series,
            {
              atSeconds: (performance.now() - started) / 1000,
              framesDecoded: after.framesDecoded,
            },
          ]) {
            reset ||= point.framesDecoded < frames;
            longestGap = Math.max(longestGap, point.atSeconds - lastAdvance);
            if (point.framesDecoded > frames) lastAdvance = point.atSeconds;
            frames = point.framesDecoded;
          }
          ok =
            check('public decoded video never stalls for three seconds', !reset && longestGap < 3, {
              longestGapSeconds: longestGap,
              counterReset: reset,
            }) && ok;
          const loss =
            Math.max(0, change.packetsLost) /
            Math.max(1, change.packetsReceived + Math.max(0, change.packetsLost));
          ok = check('public video loss stays at or below 2 percent', loss <= 0.02, loss) && ok;
          ok =
            check(
              'public video decodes at least ten frames per second',
              change.framesPerSecond >= 10,
              change.framesPerSecond,
            ) && ok;
          ok =
            check(
              'public audio packets advance',
              change.audioPacketsPerSecond > 0,
              change.audioPacketsPerSecond,
            ) && ok;
        } else {
          ok =
            check('no loss on the clean path', change.packetsLost === 0, change.packetsLost) && ok;
        }
        ok = check('top layer delivered', baselineWidth >= 480, baselineWidth) && ok;
      }
      if (name === 'lossy') {
        ok = check('loss is observed', change.packetsLost > 0, change.packetsLost) && ok;
        ok =
          check('receiver requests retransmission', change.nackCount > 0, change.nackCount) && ok;
        ok =
          check(
            'decode rate holds above half of baseline',
            baselineFps !== null &&
              baselineFps !== undefined &&
              change.framesPerSecond >= baselineFps / 2,
            { baselineFps, framesPerSecond: change.framesPerSecond },
          ) && ok;
      }
      if (name === 'constrained' || name === 'severe') {
        const expected = name === 'severe' ? 0 : 1;
        ok =
          check(
            `server steps the layer down to at most ${expected}`,
            lowestSpatial !== null && lowestSpatial !== undefined && lowestSpatial <= expected,
            { lowestSpatial, events: layerEvents.length },
          ) && ok;
        ok =
          check(
            'received width drops below baseline',
            baselineWidth !== null &&
              baselineWidth !== undefined &&
              phase.minFrameWidth < baselineWidth,
            { baselineWidth, minFrameWidth: phase.minFrameWidth },
          ) && ok;
        const first = layerEvents.find(
          (event) =>
            event.spatial !== null && event.spatial !== undefined && event.spatial <= expected,
        );
        phase.secondsToDowngrade =
          enteringSpatial !== null && enteringSpatial <= expected
            ? 0
            : first
              ? first.atSeconds - phase.startedAtSeconds
              : null;
      }
      if (profile.join) {
        ok =
          check(
            'a new viewer joins and receives video under bidirectional loss',
            phase.secondsToFirstFrame !== undefined && phase.secondsToFirstFrame <= 30,
            {
              secondsToFirstFrame: phase.secondsToFirstFrame ?? null,
              failure: phase.joinFailure ?? null,
            },
          ) && ok;
        if (latecomer)
          await latecomer
            .context()
            .close()
            .catch(() => {});
      }
      if (profile.silent) {
        ok =
          check(
            'silent microphone with DTX sends under ten packets a second',
            silentAudio && change.audioPacketsPerSecond < 10,
            { silentAudio, audioPacketsPerSecond: change.audioPacketsPerSecond },
          ) && ok;
      }
      if (name === 'recovery') {
        ok =
          check(
            'layer returns to the top',
            lastSpatial === 2 ||
              (layerEvents.length === 0 && phase.maxFrameWidth >= (baselineWidth ?? 480)),
            { lastSpatial, maxFrameWidth: phase.maxFrameWidth },
          ) && ok;
        ok =
          check(
            'received width returns to baseline',
            baselineWidth !== null &&
              baselineWidth !== undefined &&
              phase.maxFrameWidth >= baselineWidth,
            { baselineWidth, maxFrameWidth: phase.maxFrameWidth },
          ) && ok;
        const first = layerEvents.find((event) => event.spatial === 2);
        phase.secondsToUpgrade = first ? first.atSeconds - phase.startedAtSeconds : null;
      }
      phase.passed = ok;
      save();
      console.log(
        `${ok ? 'PASS' : 'FAIL'} ${name}: fps=${change.framesPerSecond.toFixed(1)} audioPps=${change.audioPacketsPerSecond.toFixed(1)} lost=${change.packetsLost} nack=${change.nackCount} pliSent=${change.pliCount} publisherKeyframes=${phase.publisher.keyFramesEncoded} publisherPli=${phase.publisher.pliReceived} width=${phase.minFrameWidth}-${phase.maxFrameWidth} layers=${layerEvents.map((event) => event.spatial).join(',') || '-'}`,
      );
    }
    if (callOutcomes) {
      await publisher.locator('#cam-btn').click();
      await viewer.waitForFunction(
        () =>
          ![...document.querySelectorAll('.video-tile:not(.local) video')].some(
            (video) => video.srcObject,
          ),
      );
      const audioReceiver = await client('audio-receiver');
      await audioReceiver.waitForFunction(() =>
        [...document.querySelectorAll('.video-tile:not(.local) audio')].some(
          (audio) => !audio.paused && audio.readyState >= 2 && !audio.muted && audio.volume > 0,
        ),
      );
      await expectCall(audioReceiver, 'call_join', 'audio_playback_ready');
      await audioReceiver.evaluate(() => window.__communitySignalingReconnect.closeCurrent());
      await audioReceiver.waitForFunction(
        () => window.__communitySignalingReconnect.snapshot().counters.reconnectSuccess === 1,
      );
      await expectCall(audioReceiver, 'call_reconnect', 'audio_playback_ready');
      await audioReceiver.context().close();
    }
    report.completed = true;
    report.passed = report.phases.every((phase) => phase.passed);
  } catch (error) {
    report.failure = { message: error.message, stack: error.stack };
    throw error;
  } finally {
    try {
      impair('clear');
      if (blockUdp) impair('unblock-udp');
    } catch (error) {
      report.failure ??= { message: `impairment clear failed: ${error.message}` };
    }
    report.finishedAt = new Date().toISOString();
    correlation?.finish();
    save();
    for (const context of contexts) {
      if (publicCanary) {
        for (const page of context.pages()) {
          try {
            const leave = page.locator('#leave-btn');
            if (await leave.isVisible()) {
              await leave.click({ timeout: 5000 });
              await leave.waitFor({ state: 'hidden', timeout: 5000 });
            }
          } catch {
            report.passed = false;
            report.failure ??= {
              message: 'owned canary could not leave its reserved room cleanly',
            };
          }
        }
      }
      await context.close().catch(() => {});
    }
    save();
    await browser.close().catch(() => {});
  }
  assert.equal(
    report.passed,
    true,
    'impaired-network phases failed; see impaired-network-results.json',
  );
  console.log(`PASS impaired-network (${artifacts})`);
}

main().catch((error) => {
  correlation?.finish();
  console.error(error.message);
  process.exitCode = 1;
});
