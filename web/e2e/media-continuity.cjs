/** Owned loopback only. Native receive/capture recovery plus controlled device UI.
 * Device IDs and native identities stay inside isolated browser pages. */
const { openRoomMenu } = require('./room-menu.cjs');
const { sheetScreen, closeSheet } = require('./settings-sheet.cjs');
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
    'One owned receiver negotiation is held indefinitely to test its deadline; sender negotiation and all subsequent receiver negotiation remain native.',
    'Hidden/visible/pageshow/devicechange hints are dispatched explicitly; the network outage uses browser request emulation, not Wi-Fi, cellular, Bluetooth, or OS lock/unlock.',
  ],
};
let stage = 'launch';
const contexts = [];
const clients = [];
const roomName = `continuity-${Date.now().toString(36)}`;

function installDeviceFixtures() {
  const evidence = (window.__mediaProduct = {
    captures: 0,
    captureRequests: [],
    captureStreams: [],
    rejectCameraQuality: false,
    qualityRejections: 0,
    revealInputs: false,
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
  navigator.mediaDevices.getUserMedia = async (constraints) => {
    evidence.captures++;
    const requested = { video: Boolean(constraints.video), audio: Boolean(constraints.audio) };
    evidence.captureRequests.push(requested);
    if (
      evidence.rejectCameraQuality &&
      constraints.video &&
      typeof constraints.video === 'object' &&
      ('width' in constraints.video ||
        'height' in constraints.video ||
        'frameRate' in constraints.video)
    ) {
      evidence.qualityRejections++;
      throw new DOMException('Starting videoinput failed', 'NotReadableError');
    }
    const stream = await capture(constraints);
    evidence.captureStreams.push(stream);
    if (evidence.revealInputs) {
      for (const [kind, enabled, label] of [
        ['videoinput', requested.video, 'Fixture camera'],
        ['audioinput', requested.audio, 'Fixture microphone'],
      ]) {
        if (enabled && !evidence.devices.some((device) => device.kind === kind))
          evidence.devices.push({ kind, deviceId: `permission-${kind}`, label, groupId: '' });
      }
      // Permission alone need not dispatch devicechange. The product must
      // refresh its concealed device list immediately after explicit preview.
    }
    return stream;
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

/** One owned negotiation failure; all native identities stay inside the page. */
function installReceiveRecoveryObservation() {
  const nativeRemote = RTCPeerConnection.prototype.setRemoteDescription;
  const nativeSend = WebSocket.prototype.send;
  const counters = { joins: 0, leaves: 0, held: 0 };
  let armed = false;
  let heldPeer = null;
  let heldAt = null;
  let baseline = null;
  const send = function (...args) {
    const result = Reflect.apply(nativeSend, this, args);
    if (typeof args[0] === 'string' && args[0].length <= 65536) {
      try {
        const type = JSON.parse(args[0])?.type;
        if (type === 'joinRoom') counters.joins++;
        if (type === 'leaveRoom') counters.leaves++;
      } catch {
        /* Observe fixed message kinds only. */
      }
    }
    return result;
  };
  const remote = function (...args) {
    if (
      armed &&
      args[0]?.type === 'offer' &&
      window.__communityPeers.includes(this) &&
      this.getSenders().every((sender) => !sender.track)
    ) {
      armed = false;
      counters.held++;
      heldPeer = this;
      heldAt = performance.now();
      return new Promise(() => {});
    }
    return Reflect.apply(nativeRemote, this, args);
  };
  RTCPeerConnection.prototype.setRemoteDescription = remote;
  WebSocket.prototype.send = send;
  const guard = () => {
    if (
      RTCPeerConnection.prototype.setRemoteDescription !== remote ||
      WebSocket.prototype.send !== send ||
      counters.joins > 100 ||
      counters.leaves > 100 ||
      counters.held > 1
    )
      throw new Error('Owned receiver observation was replaced or exceeded its bounds');
  };
  const liveTracks = (peer) =>
    peer
      .getSenders()
      .map((sender) => sender.track)
      .filter((track) => track && track.readyState === 'live');
  window.__receiveRecoveryObservation = {
    baseline() {
      guard();
      const peers = window.__communityPeers.filter((peer) => peer.connectionState === 'connected');
      const sender = peers.find((peer) => liveTracks(peer).length >= 3);
      const receiver = peers.find(
        (peer) =>
          !liveTracks(peer).length &&
          peer.getReceivers().some((item) => item.track?.readyState === 'live'),
      );
      if (!sender || !receiver)
        throw new Error('Expected active sender and receiver for owned recovery');
      baseline = {
        sender,
        receiver,
        tracks: liveTracks(sender),
        captures: window.__mediaProduct.captures,
        screenCaptures: window.__mediaProduct.screenCaptures,
        joins: counters.joins,
        leaves: counters.leaves,
      };
      return { sendingTracks: baseline.tracks.length };
    },
    arm() {
      guard();
      if (armed || counters.held || !baseline)
        throw new Error('Owned receiver failure may be armed once');
      armed = true;
    },
    snapshot() {
      guard();
      if (!baseline) throw new Error('Owned recovery baseline missing');
      const currentTracks = liveTracks(baseline.sender);
      const receivers = window.__communityPeers.filter(
        (peer) =>
          peer.connectionState === 'connected' &&
          !liveTracks(peer).length &&
          peer.getReceivers().some((item) => item.track?.readyState === 'live'),
      );
      return {
        senderPreserved:
          baseline.sender.connectionState === 'connected' &&
          currentTracks.length === baseline.tracks.length &&
          baseline.tracks.every(
            (track) =>
              currentTracks.includes(track) &&
              track.readyState === 'live' &&
              track.enabled &&
              !track.muted,
          ),
        receiverReplaced:
          baseline.receiver.connectionState === 'closed' &&
          receivers.some((peer) => peer !== baseline.receiver),
        capturesUnchanged:
          baseline.captures === window.__mediaProduct.captures &&
          baseline.screenCaptures === window.__mediaProduct.screenCaptures,
        membershipUnchanged:
          baseline.joins === counters.joins && baseline.leaves === counters.leaves,
        heldCount: counters.held,
        heldReceiverClosed: heldPeer ? heldPeer.connectionState === 'closed' : null,
        heldAgeMs: heldAt === null ? null : Math.max(0, performance.now() - heldAt),
      };
    },
  };
}

/** Native decode/playback counters only; no IDs, SDP or device data leave the page. */
async function recoveryProgress() {
  const camera = document
    .querySelector('.video-tile:not(.screen-share):not(.local) video')
    ?.srcObject?.getVideoTracks()[0];
  const screen = document
    .querySelector('.video-tile.screen-share:not(.local) video')
    ?.srcObject?.getVideoTracks()[0];
  const result = { cameraFrames: 0, screenFrames: 0, audioPackets: 0, audioPlaying: false };
  for (const peer of window.__communityPeers) {
    if (peer.connectionState !== 'connected') continue;
    for (const receiver of peer.getReceivers()) {
      if (receiver.track?.readyState !== 'live') continue;
      const stats = await receiver.getStats();
      for (const row of stats.values()) {
        if (row.type !== 'inbound-rtp') continue;
        if (receiver.track === camera && Number.isFinite(row.framesDecoded))
          result.cameraFrames += row.framesDecoded;
        if (receiver.track === screen && Number.isFinite(row.framesDecoded))
          result.screenFrames += row.framesDecoded;
        if (receiver.track.kind === 'audio' && Number.isFinite(row.packetsReceived))
          result.audioPackets += row.packetsReceived;
      }
    }
  }
  result.audioPlaying = [...document.querySelectorAll('.video-tile:not(.local) audio')].some(
    (audio) =>
      !audio.paused &&
      !audio.muted &&
      audio.volume > 0 &&
      audio.readyState >= 2 &&
      audio.currentTime > 0,
  );
  return result;
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
  await page.addInitScript(installReceiveRecoveryObservation);
  await page.goto(origin.toString());
  await page.locator('#name-input').fill(label);
  await page.locator('#room-input').fill(roomName);
  await page.locator('#join-btn').click();
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  await page.waitForFunction(
    () => document.querySelector('#connection-status').textContent === 'Connected',
  );
  assert.equal(
    await page.evaluate(() => window.__mediaProduct.captures),
    0,
    'opening the site and joining never requests camera or microphone capture',
  );
  return page;
}

/** Opens the sheet on one media screen; Camera & mic by default. */
async function settings(page, screen = 'Camera & mic') {
  await page.locator('#settings-btn').click();
  return sheetScreen(page, screen);
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
  dialog = await sheetScreen(page, 'Speaker');
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  assert.equal(await dialog.locator('[data-output-device]').inputValue(), 'fixture-speaker');
  await dialog.getByRole('button', { name: 'Test speaker', exact: true }).click();
  await page.waitForFunction(() => window.__mediaProduct.plays === 1);
  // Leaving the Speaker screen stops its test tone.
  await dialog.getByRole('button', { name: 'Back', exact: true }).click();
  assert.equal(
    await page.evaluate(() => window.__mediaProduct.tones.every((tone) => tone.paused)),
    true,
  );
  dialog = await sheetScreen(page, 'Speaker');
  await page.evaluate(() => {
    window.__mediaProduct.devices = [];
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  });
  await page.waitForFunction(() =>
    document
      .querySelector('[data-output-status]')
      .textContent.startsWith('Selected speaker is not currently listed'),
  );
  await page.evaluate(() => {
    window.__mediaProduct.chooserMode = 'cancel';
  });
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.includes('cancelled or blocked'),
  );
  // The select applies as soon as it changes.
  await dialog.locator('[data-output-device]').selectOption('');
  await page.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  await page.evaluate(() => {
    window.__mediaProduct.chooserMode = 'defer';
  });
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await closeSheet(page);
  await page.evaluate(() => window.__mediaProduct.completeChooser());
  dialog = await settings(page, 'Speaker');
  assert.equal(
    await dialog.locator('[data-output-device]').inputValue(),
    '',
    'dismissed speaker choice must not change a reopened screen',
  );
  dialog = await sheetScreen(page, 'Camera & mic');
  assert.equal(
    await dialog.locator('[name=cameraDeviceId]').inputValue(),
    'fixture-camera',
    'the chosen camera stays selected while it is unplugged',
  );
  // Choices apply at once, so hand the next stage the default camera again.
  await dialog.locator('[name=cameraDeviceId]').selectOption('');
  await dialog.locator('.sheet-row-saved', { hasText: 'Saved' }).first().waitFor();
  await closeSheet(page);
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
  // One test tone per visit to the Speaker screen, disposed on leaving it.
  assert.equal(result.revoked, 3);
  report.checks.push({
    name: 'live-devices-output-gesture-cancel-and-cleanup',
    passed: true,
    captures: 0,
  });
}

async function privatePermissionPreview(page) {
  await page.evaluate(() => {
    window.__mediaProduct.devices = [];
    window.__mediaProduct.revealInputs = true;
    window.__mediaProduct.rejectCameraQuality = true;
  });
  const dialog = await settings(page);
  assert.equal(await dialog.locator('select[name=cameraDeviceId] option').count(), 1);
  assert.equal(await dialog.locator('select[name=microphoneDeviceId] option').count(), 1);
  assert.equal(await page.evaluate(() => window.__mediaProduct.captures), 0);

  const observation = () =>
    page.evaluate(() => ({
      requests: window.__mediaProduct.captureRequests,
      liveKinds: window.__mediaProduct.captureStreams
        .flatMap((stream) => stream.getTracks())
        .filter((track) => track.readyState === 'live')
        .map((track) => track.kind)
        .sort(),
      sending: window.__communityPeers.some((peer) =>
        peer.getSenders().some((sender) => sender.track?.readyState === 'live'),
      ),
      cameraEnabled: document.querySelector('#cam-btn').classList.contains('active'),
      microphoneEnabled: !document.querySelector('#mic-btn').classList.contains('muted'),
    }));
  await dialog.getByRole('button', { name: 'Test camera', exact: true }).click();
  await dialog
    .locator('select[name=cameraDeviceId] option[value=permission-videoinput]')
    .waitFor({ state: 'attached' });
  assert.equal(await dialog.locator('select[name=microphoneDeviceId] option').count(), 1);
  assert.deepEqual(await observation(), {
    requests: [
      { video: true, audio: false },
      { video: true, audio: false },
    ],
    liveKinds: ['video'],
    sending: false,
    cameraEnabled: false,
    microphoneEnabled: false,
  });
  await dialog.getByRole('button', { name: 'Stop preview', exact: true }).click();
  await page.waitForFunction(() =>
    window.__mediaProduct.captureStreams.every((stream) =>
      stream.getTracks().every((track) => track.readyState === 'ended'),
    ),
  );
  assert.equal(
    await dialog.locator('.media-preview-video').evaluate((video) => video.srcObject === null),
    true,
  );
  assert.equal(await page.evaluate(() => window.__mediaProduct.qualityRejections), 1);
  await page.evaluate(() => {
    window.__mediaProduct.rejectCameraQuality = false;
  });

  await dialog.getByRole('button', { name: 'Test microphone', exact: true }).click();
  await dialog
    .locator('select[name=microphoneDeviceId] option[value=permission-audioinput]')
    .waitFor({ state: 'attached' });
  assert.equal(await dialog.locator('select[name=cameraDeviceId] option').count(), 2);
  assert.deepEqual(await observation(), {
    requests: [
      { video: true, audio: false },
      { video: true, audio: false },
      { video: false, audio: true },
    ],
    liveKinds: ['audio'],
    sending: false,
    cameraEnabled: false,
    microphoneEnabled: false,
  });
  await closeSheet(page);
  await page.waitForFunction(() =>
    window.__mediaProduct.captureStreams.every((stream) =>
      stream.getTracks().every((track) => track.readyState === 'ended'),
    ),
  );
  await settings(page);
  assert.equal(await page.evaluate(() => window.__mediaProduct.captures), 3);
  // Reopening applies nothing: the devices revealed by the preview were kept at once.
  await closeSheet(page);
  assert.deepEqual(await observation(), {
    requests: [
      { video: true, audio: false },
      { video: true, audio: false },
      { video: false, audio: true },
    ],
    liveKinds: [],
    sending: false,
    cameraEnabled: false,
    microphoneEnabled: false,
  });
  report.checks.push({
    name: 'private-preview-quality-fallback-reveals-devices-without-save',
    passed: true,
    captures: 3,
    cameraOnly: true,
    microphoneOnly: true,
    stoppedAndClosed: true,
    broadcast: false,
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

async function lifecycleRecovery(viewer) {
  stage = 'lifecycle-controls';
  const captured = await viewer.evaluate(() => window.__mediaProduct.captures);
  const tile = viewer.locator('.video-tile:not(.local)').first();
  await tile.locator('.personal-media-controls summary').click();
  await tile.locator('[data-control="mute"]').click();
  await tile.locator('[data-control="volume"]').evaluate((input) => {
    input.value = '32';
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await tile.locator('.personal-media-dismiss').click();
  await viewer.evaluate(() => {
    document.querySelector('.video-tile:not(.local) video').pause();
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
    window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
    for (let index = 0; index < 30; index++) {
      window.dispatchEvent(new Event('online'));
      navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
    }
  });
  await viewer.waitForTimeout(350);
  assert.equal(await tile.locator('video').count(), 0, 'hidden pages release incoming video');
  stage = 'lifecycle-visible-playback';
  await viewer.evaluate(() => {
    delete document.visibilityState;
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    document.dispatchEvent(new Event('visibilitychange'));
    for (let index = 0; index < 30; index++) window.dispatchEvent(new Event('online'));
  });
  await viewer.waitForFunction(
    () => document.querySelector('.video-tile:not(.local) video')?.paused === false,
  );
  assert.deepEqual(
    await tile.locator('video').evaluate((video) => ({ muted: video.muted, volume: video.volume })),
    { muted: true, volume: 0.32 },
  );
  assert.equal(await viewer.evaluate(() => window.__mediaProduct.captures), captured);
  await tile.locator('.personal-media-controls summary').click();
  await tile.locator('[data-control="mute"]').click();
  await tile.locator('.personal-media-dismiss').click();
  report.checks.push({
    name: 'lifecycle-hints-resume-playback-preserve-personal-choices',
    passed: true,
  });

  stage = 'lifecycle-speaker-change';
  await viewer.evaluate(() => {
    window.__mediaProduct.chooserMode = 'allow';
    window.__mediaProduct.devices = [
      { kind: 'audiooutput', deviceId: 'fixture-speaker', label: 'Fixture speaker', groupId: '' },
    ];
  });
  let dialog = await settings(viewer, 'Speaker');
  await dialog.getByRole('button', { name: 'Choose another speaker', exact: true }).click();
  await viewer.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  await closeSheet(viewer);
  await viewer.evaluate(() => {
    window.__mediaProduct.devices = [];
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  });
  await viewer
    .getByText(
      'Your selected speaker is not currently listed. Reconnect it or choose a speaker in Settings.',
      { exact: true },
    )
    .waitFor();
  dialog = await settings(viewer, 'Speaker');
  assert.equal(await dialog.locator('[data-output-device]').inputValue(), 'fixture-speaker');
  await dialog.locator('[data-output-device]').selectOption('');
  await viewer.waitForFunction(() =>
    document.querySelector('[data-output-status]').textContent.startsWith('Speaker selected'),
  );
  await closeSheet(viewer);
  assert.equal(await viewer.evaluate(() => window.__mediaProduct.captures), captured);
  report.checks.push({
    name: 'detached-speaker-notice-keeps-selected-output-until-user-changes-it',
    passed: true,
  });
}

async function receiveBudget(viewer, publisher) {
  stage = 'receive-video-modes';
  const tile = viewer.locator('.video-tile:not(.local)').first();
  const before = await tile.boundingBox();
  const captures = await viewer.evaluate(() => window.__mediaProduct.captures);
  const publisherCaptures = await publisher.evaluate(() => window.__mediaProduct.captures);
  await openRoomMenu(viewer);
  const mode = viewer.getByLabel('Incoming video', { exact: true });
  await mode.selectOption('audio-only');
  await viewer.waitForFunction(
    () => document.querySelectorAll('.video-tile:not(.local) video').length === 0,
  );
  assert.equal(await tile.locator('.video-deferred-notice').textContent(), 'Audio only');
  const after = await tile.boundingBox();
  assert.ok(
    Math.abs(before.width - after.width) <= 1 && Math.abs(before.height - after.height) <= 1,
    'deferred video keeps its existing tile dimensions',
  );
  await mode.selectOption('data-saver');
  await progressing(viewer, 'data-saver-native-decoding');
  await mode.selectOption('balanced');
  await viewer.locator('#room-more-btn').click();
  stage = 'offscreen-video-reclaim';
  await tile.evaluate((element) => {
    element.style.transform = 'translateY(100000px)';
  });
  await viewer.waitForFunction(
    () => document.querySelectorAll('.video-tile:not(.local) video').length === 0,
  );
  assert.equal(
    await tile.locator('.video-deferred-notice').textContent(),
    'Video resumes when visible',
  );
  await tile.evaluate((element) => {
    element.style.transform = '';
  });
  await progressing(viewer, 'visible-tile-resubscribes-and-decodes');
  assert.equal(await viewer.evaluate(() => window.__mediaProduct.captures), captures);
  assert.equal(await publisher.evaluate(() => window.__mediaProduct.captures), publisherCaptures);
  assert.equal(
    await publisher.locator('#cam-btn').evaluate((button) => button.classList.contains('active')),
    true,
  );
  report.checks.push({
    name: 'receive-modes-reclaim-offscreen-video-preserve-layout-and-publishing',
    passed: true,
  });
}

async function openCallHealth(page) {
  await openRoomMenu(page);
  await page.locator('#call-health-btn').click();
  return page.getByRole('dialog', { name: 'Call health', exact: true });
}

async function receiverReplacement(viewer, publisher) {
  stage = 'bidirectional-receiver-refresh-setup';
  await publisher.locator('#mic-btn').click();
  await viewer.locator('#mic-btn').click();
  await viewer.locator('#cam-btn').click();
  await viewer.evaluate(() => {
    window.__mediaProduct.screenMode = 'video';
  });
  await viewer.locator('#screen-btn').click();
  await viewer.waitForFunction(() =>
    document.querySelector('#screen-btn').classList.contains('active'),
  );
  await progressing(publisher, 'bidirectional-sending-native-decoding');
  await viewer.waitForFunction(() =>
    window.__communityPeers.some(
      (peer) =>
        peer.connectionState === 'connected' &&
        peer
          .getSenders()
          .filter(
            (sender) =>
              sender.track?.readyState === 'live' && sender.track.enabled && !sender.track.muted,
          ).length >= 3,
    ),
  );
  await viewer.evaluate(() => window.__receiveRecoveryObservation.baseline());
  const advancing = (before, after, screen) =>
    after.cameraFrames >= before.cameraFrames + 3 &&
    (!screen || after.screenFrames >= before.screenFrames + 3) &&
    after.audioPackets > before.audioPackets &&
    after.audioPlaying;
  const verify = async (name, requireHeld) => {
    const deadline = Date.now() + 20000;
    const outgoingBefore = await publisher.evaluate(recoveryProgress);
    let incomingBefore = await viewer.evaluate(recoveryProgress);
    while (Date.now() < deadline) {
      await viewer.waitForTimeout(600);
      const identity = await viewer.evaluate(() => window.__receiveRecoveryObservation.snapshot());
      assert.equal(
        identity.senderPreserved,
        true,
        'receiver recovery preserves the sending PC and exact live local tracks',
      );
      assert.equal(identity.capturesUnchanged, true);
      assert.equal(identity.membershipUnchanged, true);
      const incoming = await viewer.evaluate(recoveryProgress);
      const outgoing = await publisher.evaluate(recoveryProgress);
      report.lastReceiverRecovery = { name, ...identity, incoming, outgoing };
      if (
        identity.receiverReplaced &&
        (!requireHeld || (identity.heldCount === 1 && identity.heldReceiverClosed)) &&
        advancing(incomingBefore, incoming, false) &&
        advancing(outgoingBefore, outgoing, true)
      ) {
        report.checks.push({
          name,
          passed: true,
          senderAndTracksPreserved: true,
          noCaptureOrMembershipChange: true,
          outgoingCameraFrames: outgoing.cameraFrames - outgoingBefore.cameraFrames,
          outgoingScreenFrames: outgoing.screenFrames - outgoingBefore.screenFrames,
          outgoingAudioPackets: outgoing.audioPackets - outgoingBefore.audioPackets,
          incomingCameraFrames: incoming.cameraFrames - incomingBefore.cameraFrames,
          incomingAudioPackets: incoming.audioPackets - incomingBefore.audioPackets,
          heldCount: identity.heldCount,
          heldAgeMs: identity.heldAgeMs,
        });
        return;
      }
      incomingBefore = incoming;
    }
    throw new Error(
      'Owned receive-only recovery did not restore bidirectional native progress within its bound',
    );
  };
  stage = 'call-health-open';
  assert.equal(
    await viewer.evaluate(() => localStorage.getItem('reliabilityTelemetry') === 'true'),
    false,
  );
  let telemetryUploads = 0;
  const observeTelemetry = (request) => {
    if (new URL(request.url()).pathname === '/api/telemetry') telemetryUploads++;
  };
  viewer.on('request', observeTelemetry);
  const health = await openCallHealth(viewer);
  stage = 'call-health-current-evidence';
  assert.equal(await health.locator('[data-call-health]').count(), 3);
  await viewer.waitForFunction(
    () =>
      ['sending', 'receiving'].every(
        (direction) =>
          document
            .querySelector(`[data-call-health="${direction}"]`)
            ?.getAttribute('data-health-level') === 'good',
      ),
    null,
    { timeout: 10000 },
  );
  stage = 'explicit-receiver-replacement-preserves-sending';
  await health.getByRole('button', { name: 'Refresh incoming media', exact: true }).click();
  await verify('explicit-receiver-replacement-preserves-outgoing-mic-camera-screen', false);
  await health
    .locator('[data-call-health="receiving"][data-health-level="good"]')
    .waitFor({ state: 'visible' });
  assert.equal(telemetryUploads, 0, 'local call health does not upload with sharing disabled');
  viewer.off('request', observeTelemetry);
  report.checks.push({
    name: 'local-call-health-with-sharing-off-has-separate-live-evidence-and-explicit-repair',
    passed: true,
    independentSections: 3,
    sharingEnabled: false,
    telemetryUploads,
  });
  await health.getByRole('button', { name: 'Close', exact: true }).click();
  await health.waitFor({ state: 'hidden' });
  await viewer.evaluate(() => {
    window.__receiveRecoveryObservation.baseline();
    window.__receiveRecoveryObservation.arm();
  });
  stage = 'hung-receiver-negotiation-deadline';
  // Only the remote publisher changes capture. The viewer's next receive-only
  // offer is held indefinitely, leaving its sender entirely native and active.
  await publisher.locator('#cam-btn').click();
  await publisher.waitForFunction(
    () => !document.querySelector('#cam-btn').classList.contains('active'),
  );
  await publisher.locator('#cam-btn').click();
  await viewer.waitForFunction(
    () => window.__receiveRecoveryObservation.snapshot().heldCount === 1,
  );
  await verify('hung-native-receive-negotiation-has-bounded-receive-only-recovery', true);
  await openRoomMenu(viewer);
  assert.equal(await viewer.locator('#refresh-incoming-media').isEnabled(), true);
  await viewer.keyboard.press('Escape');
}

async function main() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  try {
    stage = 'join';
    const publisher = await join(browser, 'Publisher');
    const viewer = await join(browser, 'Viewer');
    stage = 'device-ui';
    await deviceUi(viewer);
    stage = 'private-permission-preview';
    await privatePermissionPreview(viewer);
    const viewerCaptureCounts = await viewer.evaluate(() => window.__mediaProduct.captures);
    stage = 'camera';
    await publisher.locator('#cam-btn').click();
    await progressing(viewer, 'native-camera-decoding');
    assert.equal(
      await publisher.locator('#settings-sheet').isVisible(),
      false,
      'explicit camera activation does not require opening and saving Settings',
    );
    assert.equal(
      await publisher.locator('#mic-btn').evaluate((button) => button.classList.contains('muted')),
      true,
    );
    stage = 'camera-off-on';
    await publisher.locator('#cam-btn').click();
    await publisher.waitForFunction(
      () => !document.querySelector('#cam-btn').classList.contains('active'),
    );
    await publisher.evaluate(() => {
      window.__mediaProduct.rejectCameraQuality = true;
    });
    await publisher.locator('#cam-btn').click();
    await progressing(viewer, 'camera-off-on-quality-fallback-decoding');
    assert.equal(
      await publisher.locator('#mic-btn').evaluate((button) => button.classList.contains('muted')),
      true,
    );
    const captureCounts = await publisher.evaluate(() => window.__mediaProduct.captures);
    assert.equal(captureCounts, 3, 'two explicit camera activations plus one quality fallback');
    assert.equal(await publisher.evaluate(() => window.__mediaProduct.qualityRejections), 1);
    await publisher.evaluate(() => {
      window.__mediaProduct.rejectCameraQuality = false;
    });
    await receiveBudget(viewer, publisher);
    stage = 'lifecycle-hints';
    await lifecycleRecovery(viewer);
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
    await openRoomMenu(viewer);
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
    stage = 'browser-request-outage';
    const recoveries = await viewer.evaluate(
      () => window.__communitySignalingReconnect.snapshot().counters.reconnectSuccess,
    );
    await viewer.context().setOffline(true);
    try {
      await viewer.evaluate(() => window.__communitySignalingReconnect.closeCurrent());
      await viewer.waitForTimeout(400);
    } finally {
      await viewer.context().setOffline(false);
    }
    await viewer.evaluate(() => {
      for (let index = 0; index < 30; index++) window.dispatchEvent(new Event('online'));
    });
    await viewer.waitForFunction(
      (before) =>
        window.__communitySignalingReconnect.snapshot().counters.reconnectSuccess > before,
      recoveries,
    );
    await progressing(viewer, 'browser-request-outage-recovery-decoding');
    assert.equal(
      await viewer.evaluate(() => window.__mediaProduct.captures),
      viewerCaptureCounts,
      'receiver recovery never enables capture',
    );
    assert.equal(
      await publisher.evaluate(() => window.__mediaProduct.captures),
      captureCounts,
      'receive recovery never restarts publisher capture',
    );
    await receiverReplacement(viewer, publisher);
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
      const before = await page.evaluate(() => window.__mediaProduct.captures);
      await page.evaluate(() => {
        window.dispatchEvent(new Event('online'));
        window.dispatchEvent(new Event('pageshow'));
        navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
      });
      await page.waitForTimeout(350);
      assert.equal(await page.evaluate(() => window.__mediaProduct.captures), before);
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
