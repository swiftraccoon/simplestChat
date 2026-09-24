/** Owned loopback only. Native receive/capture recovery plus controlled device UI.
 * Device IDs and native identities stay inside isolated browser pages. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { browserOptions } = require('./browser-options.cjs');
const { installPeerEventTracing } = require('./peer-events.cjs');
const { installSignalingReconnectObservation } = require('./signaling-reconnect.cjs');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:3119');
if (
  process.env.MEDIA_CONTINUITY_E2E !== '1' ||
  process.env.DISPOSABLE_TEST_DATABASE !== '1' ||
  origin.protocol !== 'http:' ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error(
    'Media continuity requires explicit opt-in and owned disposable loopback services.',
  );
const options = browserOptions(process.env.E2E_BROWSER);
const artifacts =
  process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-continuity.'));
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
const report = {
  browser: options.name,
  passed: false,
  pageErrors: 0,
  checks: [],
  failureStage: null,
  limitations: [
    'Synthetic camera and microphone; no physical device unplug, macOS sleep, OS permissions or audible-sound proof.',
    'Output chooser, sink routing and device lists use controlled native-API fixtures; actual browser/device support remains a manual check.',
    'Display capture uses an owned fake camera stream; native screen picker UI and system/tab audio availability are not simulated as proof.',
    'Signaling loss closes one owned native WebSocket; this does not simulate an OS network change or UDP outage.',
  ],
};
let stage = 'launch';
const contexts = [];
const clients = [];
const roomName = `continuity-${Date.now().toString(36)}`;

function installDeviceFixtures() {
  const evidence = (window.__mediaProduct = {
    captures: 0,
    screenCaptures: 0,
    chooserGestures: [],
    plays: 0,
    revoked: 0,
    sinkCalls: [],
    devices: [],
    chooserMode: 'allow',
    screenMode: 'cancel',
    tones: [],
  });
  const capture = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
  navigator.mediaDevices.getUserMedia = (...args) => {
    evidence.captures++;
    return capture(...args);
  };
  navigator.mediaDevices.enumerateDevices = async () => [...evidence.devices];
  const sinks = new WeakMap();
  Object.defineProperty(HTMLMediaElement.prototype, 'sinkId', {
    configurable: true,
    get() {
      return sinks.get(this) || '';
    },
  });
  HTMLMediaElement.prototype.setSinkId = async function (id) {
    evidence.sinkCalls.push({ kind: this.tagName.toLowerCase(), selected: id !== '' });
    sinks.set(this, id);
  };
  navigator.mediaDevices.selectAudioOutput = () => {
    evidence.chooserGestures.push(navigator.userActivation.isActive);
    if (evidence.chooserMode === 'cancel')
      return Promise.reject(new DOMException('Fixture', 'NotAllowedError'));
    const device = {
      kind: 'audiooutput',
      deviceId: 'fixture-speaker',
      label: 'Fixture speaker',
      groupId: '',
    };
    if (evidence.chooserMode === 'defer')
      return new Promise((resolve) => {
        evidence.completeChooser = () => resolve(device);
      });
    return Promise.resolve(device);
  };
  const NativeAudio = window.Audio;
  window.Audio = new Proxy(NativeAudio, {
    construct(target, args) {
      const audio = Reflect.construct(target, args);
      if (String(args[0]).startsWith('blob:')) {
        evidence.tones.push(audio);
        const play = audio.play.bind(audio);
        audio.play = () => {
          evidence.plays++;
          return play();
        };
      }
      return audio;
    },
  });
  const revoke = URL.revokeObjectURL.bind(URL);
  URL.revokeObjectURL = (url) => {
    evidence.revoked++;
    revoke(url);
  };
  navigator.mediaDevices.getDisplayMedia = async () => {
    evidence.screenCaptures++;
    if (evidence.screenMode === 'cancel') throw new DOMException('Fixture', 'NotAllowedError');
    return capture({ video: true, audio: false });
  };
}

async function join(browser, label) {
  const context = await browser.newContext({
    ...options.contextOptions,
    viewport: { width: 1280, height: 900 },
  });
  contexts.push(context);
  const page = await context.newPage();
  clients.push({ label, page });
  page.on('pageerror', () => {
    report.pageErrors = Math.min(100, report.pageErrors + 1);
  });
  page.setDefaultTimeout(15000);
  await page.addInitScript(installPeerEventTracing, {
    announcedIp: process.env.TEST_ANNOUNCE_IP || null,
  });
  await page.addInitScript(installSignalingReconnectObservation);
  await page.addInitScript(installDeviceFixtures);
  await page.goto(origin.toString());
  await page.locator('#name-input').fill(label);
  await page.locator('#room-input').fill(roomName);
  await page.locator('#join-btn').click();
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  await page.waitForFunction(
    () => document.querySelector('#connection-status').textContent === 'Connected',
  );
  return page;
}

async function settings(page) {
  await page.locator('#settings-btn').click();
  const dialog = page.getByRole('dialog', { name: 'Your settings', exact: true });
  await dialog.waitFor({ state: 'visible' });
  return dialog;
}

async function deviceUi(page) {
  let dialog = await settings(page);
  await page.evaluate(() => {
    window.__mediaProduct.devices = [
      { kind: 'videoinput', deviceId: 'fixture-camera', label: 'Fixture camera', groupId: '' },
      { kind: 'audioinput', deviceId: 'fixture-mic', label: 'Fixture mic', groupId: '' },
      { kind: 'audiooutput', deviceId: 'fixture-speaker', label: 'Fixture speaker', groupId: '' },
    ];
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  });
  await dialog
    .locator('[name=cameraDeviceId] option[value=fixture-camera]')
    .waitFor({ state: 'attached' });
  await dialog.locator('[name=cameraDeviceId]').selectOption('fixture-camera');
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  assert.equal(await dialog.locator('[data-output-device]').inputValue(), 'fixture-speaker');
  await dialog.getByRole('button', { name: 'Test speaker', exact: true }).click();
  await page.waitForFunction(() => window.__mediaProduct.plays === 1);
  await dialog.getByRole('tab', { name: 'Appearance', exact: true }).click();
  assert.equal(
    await page.evaluate(() => window.__mediaProduct.tones.every((tone) => tone.paused)),
    true,
  );
  await dialog.getByRole('tab', { name: 'Audio & video', exact: true }).click();
  await page.evaluate(() => {
    window.__mediaProduct.devices = [];
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  });
  await page.waitForFunction(() =>
    document
      .querySelector('[data-output-status]')
      .textContent.startsWith('Selected speaker is not currently listed'),
  );
  assert.equal(await dialog.locator('[name=cameraDeviceId]').inputValue(), 'fixture-camera');
  await page.evaluate(() => {
    window.__mediaProduct.chooserMode = 'cancel';
  });
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.includes('cancelled or blocked'),
  );
  await dialog.locator('[data-output-device]').selectOption('');
  await dialog.getByRole('button', { name: 'Use speaker', exact: true }).click();
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  await page.evaluate(() => {
    window.__mediaProduct.chooserMode = 'defer';
  });
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await dialog.getByRole('button', { name: 'Close your settings', exact: true }).click();
  await page.evaluate(() => window.__mediaProduct.completeChooser());
  dialog = await settings(page);
  assert.equal(
    await dialog.locator('[data-output-device]').inputValue(),
    '',
    'dismissed speaker choice must not change a reopened dialog',
  );
  await dialog.getByRole('button', { name: 'Close your settings', exact: true }).click();
  const result = await page.evaluate(() => ({
    captures: window.__mediaProduct.captures,
    screenCaptures: window.__mediaProduct.screenCaptures,
    gestures: window.__mediaProduct.chooserGestures,
    disposed: window.__mediaProduct.tones.every((tone) => tone.paused && !tone.getAttribute('src')),
    revoked: window.__mediaProduct.revoked,
  }));
  assert.equal(result.captures, 0);
  assert.equal(result.screenCaptures, 0);
  assert.deepEqual(result.gestures, [true, true, true]);
  assert.equal(result.disposed, true);
  assert.equal(result.revoked, 2);
  report.checks.push({
    name: 'live-devices-output-gesture-cancel-and-cleanup',
    passed: true,
    captures: 0,
  });
}

async function decoded(page) {
  return page.evaluate(async () => {
    let frames = 0;
    let receivers = 0;
    for (const peer of window.__communityPeers) {
      if (peer.connectionState !== 'connected') continue;
      for (const receiver of peer.getReceivers()) {
        if (receiver.track.kind !== 'video' || receiver.track.readyState !== 'live') continue;
        receivers++;
        const stats = await receiver.getStats();
        for (const row of stats.values())
          if (
            row.type === 'inbound-rtp' &&
            row.kind === 'video' &&
            typeof row.framesDecoded === 'number'
          )
            frames += row.framesDecoded;
      }
    }
    const presented = [...document.querySelectorAll('.video-tile:not(.local) video')].some(
      (video) => !video.paused && video.videoWidth > 0 && video.readyState >= 2,
    );
    const states = ['new', 'connecting', 'connected', 'disconnected', 'failed', 'closed'];
    return {
      frames,
      receivers,
      presented,
      transportStates: window.__communityPeers
        .slice(-8)
        .map((peer) => (states.includes(peer.connectionState) ? peer.connectionState : 'unknown')),
    };
  });
}

async function progressing(page, name) {
  const deadline = Date.now() + 20000;
  let before = await decoded(page);
  report.lastMediaObservation = { check: name, ...before };
  while (Date.now() < deadline) {
    await page.waitForTimeout(600);
    const after = await decoded(page);
    report.lastMediaObservation = { check: name, ...after };
    if (after.presented && after.receivers > 0 && after.frames >= before.frames + 3) {
      report.checks.push({
        name,
        passed: true,
        decodedFramesInWindow: after.frames - before.frames,
      });
      return;
    }
    before = after;
  }
  throw new Error('Owned viewer did not resume decoding and playback readiness');
}

async function main() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  try {
    stage = 'join';
    const publisher = await join(browser, 'Publisher');
    const viewer = await join(browser, 'Viewer');
    stage = 'device-ui';
    await deviceUi(viewer);
    stage = 'camera';
    await publisher.locator('#cam-btn').click();
    const dialog = publisher.getByRole('dialog', { name: 'Your settings', exact: true });
    await dialog.getByRole('button', { name: 'Save settings', exact: true }).click();
    await progressing(viewer, 'native-camera-decoding');
    assert.equal(
      await publisher.locator('#mic-btn').evaluate((button) => button.classList.contains('muted')),
      true,
    );
    stage = 'camera-off-on';
    await publisher.locator('#cam-btn').click();
    await publisher.waitForFunction(
      () => !document.querySelector('#cam-btn').classList.contains('active'),
    );
    await publisher.locator('#cam-btn').click();
    await progressing(viewer, 'camera-off-on-decoding');
    assert.equal(
      await publisher.locator('#mic-btn').evaluate((button) => button.classList.contains('muted')),
      true,
    );
    const captureCounts = await publisher.evaluate(() => window.__mediaProduct.captures);
    assert.equal(captureCounts, 2, 'only the two explicit camera activations capture');
    if (options.name === 'chromium') {
      stage = 'native-freeze-resume';
      const session = await viewer.context().newCDPSession(viewer);
      try {
        await session.send('Page.setWebLifecycleState', { state: 'frozen' });
        await publisher.waitForTimeout(1500);
      } finally {
        await session.send('Page.setWebLifecycleState', { state: 'active' });
        await session.detach();
      }
      await progressing(viewer, 'native-tab-freeze-resume-decoding');
    } else
      report.limitations.push(
        'This engine exposes no CDP page lifecycle freeze; background/freeze coverage is Chromium only.',
      );
    stage = 'connected-receiver-recovery';
    const stopped = await viewer.evaluate(() => {
      const peer = window.__communityPeers.find(
        (candidate) =>
          candidate.connectionState === 'connected' &&
          candidate
            .getReceivers()
            .some(
              (receiver) => receiver.track.kind === 'video' && receiver.track.readyState === 'live',
            ),
      );
      if (!peer) return false;
      window.__stalledReceiver = peer
        .getReceivers()
        .find(
          (receiver) => receiver.track.kind === 'video' && receiver.track.readyState === 'live',
        ).track;
      window.__stalledReceiver.stop();
      return (
        peer.connectionState === 'connected' && window.__stalledReceiver.readyState === 'ended'
      );
    });
    assert.equal(stopped, true);
    await viewer.locator('#refresh-incoming-media').click();
    await progressing(viewer, 'explicit-connected-receiver-recovery');
    assert.equal(
      await viewer.evaluate(() =>
        window.__communityPeers.some((peer) =>
          peer
            .getReceivers()
            .some(
              (receiver) =>
                receiver.track.kind === 'video' &&
                receiver.track.readyState === 'live' &&
                receiver.track !== window.__stalledReceiver,
            ),
        ),
      ),
      true,
    );
    stage = 'signaling-reconnect';
    await viewer.evaluate(() => window.__communitySignalingReconnect.closeCurrent());
    await viewer.waitForFunction(
      () => window.__communitySignalingReconnect.snapshot().counters.reconnectSuccess > 0,
    );
    await progressing(viewer, 'native-signaling-reconnect-decoding');
    assert.equal(
      await viewer.evaluate(() => window.__mediaProduct.captures),
      0,
      'receiver recovery never enables capture',
    );
    assert.equal(
      await publisher.evaluate(() => window.__mediaProduct.captures),
      captureCounts,
      'receive recovery never restarts publisher capture',
    );
    stage = 'screen-outcomes';
    await publisher.locator('#screen-btn').click();
    await publisher.waitForFunction(() => window.__mediaProduct.screenCaptures === 1);
    assert.equal(
      await publisher
        .locator('#screen-btn')
        .evaluate((button) => button.classList.contains('active')),
      false,
    );
    await publisher.evaluate(() => {
      window.__mediaProduct.screenMode = 'video';
    });
    await publisher.locator('#screen-btn').click();
    await publisher.waitForFunction(
      () =>
        document.querySelector('#screen-share-status').textContent.includes('no screen audio') &&
        !document.querySelector('#screen-share-status').hidden,
    );
    await publisher.locator('#screen-btn').click();
    await publisher.waitForFunction(() => document.querySelector('#screen-share-status').hidden);
    report.checks.push({ name: 'screen-cancel-and-video-only-status', passed: true });
    stage = 'leave';
    assert.equal(report.pageErrors, 0);
    for (const page of [viewer, publisher]) {
      await page.locator('#leave-btn').click();
      await page.locator('#join-screen').waitFor({ state: 'visible' });
      assert.equal(
        await page.evaluate(() =>
          window.__communityPeers.every((peer) => peer.connectionState === 'closed'),
        ),
        true,
      );
    }
    report.passed = true;
  } finally {
    report.failureStage = report.passed ? null : stage;
    if (!report.passed) {
      report.failureClients = await Promise.all(
        clients.map(async ({ label, page }) => {
          let timer;
          try {
            return {
              label,
              ...(await Promise.race([
                page.evaluate(() => {
                  const states = [
                    'new',
                    'connecting',
                    'connected',
                    'disconnected',
                    'failed',
                    'closed',
                  ];
                  return {
                    cameraEnabled:
                      document.querySelector('#cam-btn')?.classList.contains('active') === true,
                    microphoneEnabled:
                      document.querySelector('#mic-btn')?.classList.contains('active') === true,
                    transportStates: window.__communityPeers
                      .slice(-8)
                      .map((peer) =>
                        states.includes(peer.connectionState) ? peer.connectionState : 'unknown',
                      ),
                  };
                }),
                new Promise((_, reject) => {
                  timer = setTimeout(() => reject(new Error('Snapshot deadline')), 2000);
                }),
              ])),
            };
          } catch {
            return { label, unavailable: true };
          } finally {
            clearTimeout(timer);
          }
        }),
      );
    }
    await Promise.allSettled(contexts.map((context) => context.close()));
    await browser.close();
    fs.writeFileSync(
      path.join(artifacts, 'media-continuity.json'),
      `${JSON.stringify(report, null, 2)}\n`,
      { mode: 0o600 },
    );
  }
}
main()
  .then(() => console.log('Owned media continuity checks passed.'))
  .catch(() => {
    console.error(`Owned media continuity check failed at ${stage}; see bounded report.`);
    process.exitCode = 1;
  });
