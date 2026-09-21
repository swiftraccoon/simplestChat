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
 * silentAudio profile).
 */
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { browserOptions } = require('./browser-options.cjs');
const { installPeerEventTracing } = require('./peer-events.cjs');

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
// The silent-microphone phase only makes sense with a silent capture, so it
// joins the default list only when that is configured.
const defaultProfiles = Object.keys(PROFILES).filter(
  (name) => silentAudio || name !== 'silentAudio',
);
const profiles = (process.env.IMPAIRED_PROFILES || defaultProfiles.join(','))
  .split(',')
  .map((name) => name.trim())
  .filter(Boolean);
for (const name of profiles) if (!PROFILES[name]) throw new Error(`Unknown profile ${name}`);

const report = {
  schemaVersion: 1,
  runId,
  startedAt: new Date().toISOString(),
  base,
  impairment: impairmentEnabled
    ? { script: impairScript }
    : { script: null, note: 'mechanics only' },
  browser: null,
  passed: false,
  completed: false,
  phases: [],
  layerEvents: [],
  limitations: [
    'Two owned headless Chromium clients over loopback; fake capture, no real camera, network or device.',
    'Impairment covers UDP leaving the server media port; signaling and the publisher uplink are untouched.',
    'The impairment script needs sudo and a platform that filters loopback (Linux netem); on macOS pf dummynet does not affect loopback and lossy/constrained assertions fail.',
    'Freeze counters are reported when the browser exposes them; absence is not a pass.',
  ],
  failure: null,
};
function save() {
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
          stat.state === 'succeeded' &&
          Number.isFinite(stat.currentRoundTripTime)
        ) {
          totals.roundTripTime = stat.currentRoundTripTime;
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
    const layers = {};
    let keyFramesEncoded = 0;
    let pliCount = 0;
    let framesEncoded = 0;
    for (const peer of window.__communityPeers || []) {
      if (peer.connectionState === 'closed') continue;
      const stats = await peer.getStats();
      for (const stat of stats.values()) {
        if (stat.type !== 'outbound-rtp' || stat.kind !== 'video') continue;
        keyFramesEncoded += stat.keyFramesEncoded || 0;
        pliCount += stat.pliCount || 0;
        framesEncoded += stat.framesEncoded || 0;
        layers[stat.rid || stat.ssrc] = {
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
    return { keyFramesEncoded, pliCount, framesEncoded, layers, sampledAtMs: performance.now() };
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
    });
    page.on('console', (message) => {
      const match = /\[room\] consumer (\S+) layers: spatial=(\S+), temporal=(\S+)/.exec(
        message.text(),
      );
      if (match) {
        report.layerEvents.push({
          client: label,
          atSeconds: (performance.now() - started) / 1000,
          consumerId: match[1],
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
  try {
    const publisher = await client('publisher');
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
    await publisher.locator('#mic-btn').click();
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
      const publisherBefore = await samplePublisher(publisher);
      const series = [];
      const layerEventsBefore = report.layerEvents.length;
      const end = performance.now() + profile.seconds * 1000;
      while (performance.now() < end) {
        await viewer.waitForTimeout(1000);
        const current = await sample(viewer);
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
      const lowestSpatial = layerEvents.reduce(
        (lowest, event) =>
          event.spatial !== null &&
          event.spatial !== undefined &&
          (lowest === null || lowest === undefined || event.spatial < lowest)
            ? event.spatial
            : lowest,
        null,
      );
      const lastSpatial = layerEvents.length ? layerEvents.at(-1).spatial : null;
      Object.assign(phase, {
        delta: change,
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
      if (name === 'baseline') {
        baselineWidth = phase.maxFrameWidth;
        baselineFps = change.framesPerSecond;
        ok = check('no loss on the clean path', change.packetsLost === 0, change.packetsLost) && ok;
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
        phase.secondsToDowngrade = first ? first.atSeconds - phase.startedAtSeconds : null;
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
    report.completed = true;
    report.passed = report.phases.every((phase) => phase.passed);
  } catch (error) {
    report.failure = { message: error.message, stack: error.stack };
    throw error;
  } finally {
    try {
      impair('clear');
    } catch (error) {
      report.failure ??= { message: `impairment clear failed: ${error.message}` };
    }
    report.finishedAt = new Date().toISOString();
    save();
    for (const context of contexts) await context.close().catch(() => {});
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
  console.error(error.message);
  process.exitCode = 1;
});
