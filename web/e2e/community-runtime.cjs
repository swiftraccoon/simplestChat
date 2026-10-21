const assert = require('node:assert/strict');

function communityStartupReady() {
  return (
    document.querySelector('.conversation-toolbar') !== null &&
    document.querySelector('#sign-in-btn')?.hidden === false &&
    document.querySelector('#connection-status')?.classList.contains('connected') === true
  );
}

async function navigateCommunityPage(page, url, clients = []) {
  // The app may fetch directory/account updates while already interactive.
  // Capabilities unhide Sign in; Connected is emitted only after auth restore
  // and a real signaling connection. Global network idleness is not readiness.
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  const item = clients.find((candidate) => candidate.page === page);
  await item?.testNetwork?.ready();
  await page.waitForFunction(communityStartupReady, null, { timeout: 10000 });
}

async function reconnectMediaIdentity(page, { allowIncomingVideoReplacement = false } = {}) {
  return page.evaluateHandle((allowIncomingVideoReplacement) => {
    const registry = window.__communityPeers;
    if (!Array.isArray(registry) || registry.length === 0)
      throw new Error('Native peer observation unavailable');
    const captures = window.__communityCaptureRequests;
    if (!Number.isSafeInteger(captures) || captures < 0)
      throw new Error('Capture request observation unavailable');
    const receivers = (peer) =>
      peer
        .getReceivers()
        .map((receiver) => receiver.track)
        .filter((track) => !allowIncomingVideoReplacement || track?.kind !== 'video');
    const peers = registry.map((peer) => ({
      peer,
      closed: peer.connectionState === 'closed',
      senders: peer.getSenders().map((sender) => sender.track),
      receivers: receivers(peer),
    }));
    const liveTracks = peers
      .flatMap(({ senders, receivers }) => [...senders, ...receivers])
      .filter((track) => track?.readyState === 'live');
    const localTracks = document.querySelector('#local-tile video')?.srcObject?.getTracks() || [];
    const same = (before, after) =>
      before.length === after.length && before.every((value, index) => value === after[index]);
    return {
      verify() {
        if (
          window.__communityPeers !== registry ||
          !same(
            peers.map(({ peer }) => peer),
            registry,
          )
        )
          throw new Error('Signaling recovery replaced or added native peers');
        if (window.__communityCaptureRequests !== captures)
          throw new Error('Signaling recovery requested new capture');
        for (const entry of peers) {
          // Unused directions legitimately remain New; already closed historical
          // peers may also exist after earlier smoke steps.
          if (!entry.closed && ['closed', 'failed'].includes(entry.peer.connectionState))
            throw new Error('Signaling recovery closed or failed a retained native peer');
          if (
            !same(
              entry.senders,
              entry.peer.getSenders().map((sender) => sender.track),
            ) ||
            !same(entry.receivers, receivers(entry.peer))
          )
            throw new Error('Signaling recovery replaced native media tracks');
        }
        if (
          liveTracks.some((track) => track.readyState !== 'live') ||
          !same(
            localTracks,
            document.querySelector('#local-tile video')?.srcObject?.getTracks() || [],
          )
        )
          throw new Error('Signaling recovery ended or replaced capture/receive tracks');
        return { peers: peers.length, liveTracks: liveTracks.length, captureRequests: captures };
      },
      async sample() {
        const audio = [...document.querySelectorAll('.video-tile:not(.local) audio')].filter(
          (element) =>
            !element.paused &&
            !element.muted &&
            element.volume > 0 &&
            element.readyState >= 2 &&
            element.srcObject?.getAudioTracks().some((track) => track.readyState === 'live'),
        );
        if (audio.length !== 1) throw new Error('Expected one audible owned remote audio element');
        const tracks = audio[0].srcObject.getAudioTracks();
        if (tracks.length !== 1) throw new Error('Expected one owned remote audio track');
        let videoReports = 0;
        let audioReports = 0;
        let framesDecoded = 0;
        let audioStats;
        let audioPlayout = null;
        for (const { peer, closed } of peers) {
          if (closed) continue;
          const stats = await peer.getStats();
          for (const stat of stats.values()) {
            if (stat.type !== 'inbound-rtp') continue;
            if (stat.kind === 'video' && Number.isFinite(stat.framesDecoded)) {
              videoReports++;
              framesDecoded += stat.framesDecoded;
            }
            if (stat.kind === 'audio' && stat.trackIdentifier === tracks[0].id) {
              audioReports++;
              audioStats = stat;
              if (stat.playoutId !== undefined) {
                audioPlayout = stats.get(stat.playoutId);
                if (audioPlayout?.type !== 'media-playout' || audioPlayout.kind !== 'audio')
                  throw new Error('Linked native audio playout statistics unavailable');
              }
            }
          }
        }
        if (!videoReports || audioReports !== 1)
          throw new Error(
            `Expected decoded-video counters and one matching native audio report; received ${videoReports} video and ${audioReports} audio reports`,
          );
        const audioCounters = [
          audioStats.packetsReceived,
          audioStats.totalSamplesDuration,
          audioStats.totalAudioEnergy,
          audio[0].currentTime,
        ];
        if (audioPlayout) audioCounters.push(audioPlayout.totalSamplesDuration);
        if (audioCounters.some((value) => !Number.isFinite(value) || value < 0))
          throw new Error('Native audio progress counters unavailable or invalid');
        return {
          framesDecoded,
          audioReportId: audioStats.id,
          audioPackets: audioStats.packetsReceived,
          audioSamplesTime: audioStats.totalSamplesDuration,
          audioEnergy: audioStats.totalAudioEnergy,
          audioTime: audio[0].currentTime,
          audioPlayoutId: audioPlayout?.id ?? null,
          audioPlayoutTime: audioPlayout?.totalSamplesDuration ?? null,
          sampledAtMs: performance.now(),
        };
      },
    };
  }, allowIncomingVideoReplacement);
}
function mediaProgressDelta(before, after) {
  if (
    before.audioReportId !== after.audioReportId ||
    before.audioPlayoutId !== after.audioPlayoutId
  )
    throw new Error('Native audio statistics changed identity during media observation');
  return {
    framesDecoded: after.framesDecoded - before.framesDecoded,
    audioPackets: after.audioPackets - before.audioPackets,
    audioSamplesSeconds: after.audioSamplesTime - before.audioSamplesTime,
    audioEnergy: after.audioEnergy - before.audioEnergy,
    audioSeconds: after.audioTime - before.audioTime,
    audioPlayoutSeconds:
      before.audioPlayoutTime === null || after.audioPlayoutTime === null
        ? null
        : after.audioPlayoutTime - before.audioPlayoutTime,
    elapsedMs: after.sampledAtMs - before.sampledAtMs,
  };
}
async function waitForMediaStart(page, identity, clock = performance) {
  const until = clock.now() + 10000;
  let lastError;
  do {
    try {
      const sample = await identity.evaluate((state) => state.sample());
      if (sample.audioPackets > 0 && sample.audioSamplesTime > 0.5 && sample.audioEnergy > 0)
        return;
    } catch (error) {
      lastError = error;
    }
    await page.waitForTimeout(200);
  } while (clock.now() < until);
  throw new Error('New remote media did not expose decoded audio statistics', {
    cause: lastError,
  });
}
async function advancingReconnectMedia(
  page,
  identity,
  milliseconds,
  ensureGap,
  clock = performance,
) {
  if (ensureGap) await ensureGap();
  const before = await identity.evaluate((state) => state.sample());
  const samples = [before];
  if (ensureGap) await ensureGap();
  const until = clock.now() + milliseconds;
  do {
    await page.waitForTimeout(200);
    if (ensureGap) await ensureGap();
    const after = await identity.evaluate((state) => state.sample());
    // Retain a bounded timeline on failure; one final stats snapshot cannot
    // distinguish a stalled stream from a reset counter or playback timeline.
    if (samples.length < 32) samples.push(after);
    if (ensureGap) await ensureGap();
    const delta = mediaProgressDelta(before, after);
    // Chromium's HTML clock can freeze after capture replacement while decoded
    // audio and device playout continue. Prefer linked native playout counters;
    // retain the HTML clock for engines without them. The owned fake microphone
    // emits a signal, so received packets or synthetic silence alone cannot pass.
    if (
      delta.framesDecoded > 0 &&
      delta.audioPackets > 0 &&
      delta.audioSamplesSeconds > 0.05 &&
      delta.audioEnergy > 0 &&
      (delta.audioPlayoutSeconds ?? delta.audioSeconds) > 0.05
    )
      return delta;
  } while (clock.now() < until);
  throw new Error(
    ensureGap
      ? 'No measured video/audio progress inside the observed signaling outage'
      : 'No measured decoded video and audible audio progress',
    { cause: { mediaProgress: samples } },
  );
}

function installCommunityCaptureObservation() {
  const devices = navigator.mediaDevices;
  const getUserMedia = devices.getUserMedia.bind(devices);
  let requests = 0;
  const observation = {
    devices,
    getUserMedia: (...args) => {
      requests++;
      return getUserMedia(...args);
    },
  };
  devices.getUserMedia = observation.getUserMedia;
  // Keep the instrumented DOM wrapper alive for this document. WebKit can
  // otherwise discard an unrooted MediaDevices wrapper and its own properties.
  window.__communityCaptureObservation = observation;
  Object.defineProperty(window, '__communityCaptureRequests', {
    get() {
      if (
        window.__communityCaptureObservation !== observation ||
        navigator.mediaDevices !== devices ||
        devices.getUserMedia !== observation.getUserMedia
      ) {
        throw new Error('Capture instrumentation was replaced');
      }
      return requests;
    },
  });
}

function createCommunityStep(context) {
  async function step(name, work) {
    context.setActiveStep(name);
    const result = { name, passed: false };
    context.report.steps.push(result);
    await context.paceHttpRequests();
    const requestsBefore = context.observedHttpRequests;
    context.saveReport();
    await context.testNetworkReady();
    const outcome = await (context.runStep ? context.runStep(name, work) : work());
    result.observedHttpRequests = context.observedHttpRequests - requestsBefore;
    await context.testNetworkReady();
    if (outcome?.skip) {
      result.skipped = true;
      result.reason = outcome.skip;
      context.saveReport();
      (context.log ?? console.log)(`SKIP ${name}: ${outcome.skip}`);
      return;
    }
    result.passed = true;
    context.saveReport();
    (context.log ?? console.log)(`PASS ${name}`);
  }
  return step;
}

function createTestNetworkOwnership(clients) {
  async function testNetworkReady() {
    await Promise.all(
      clients.filter((item) => !item.testNetworkRetired).map((item) => item.testNetwork?.ready()),
    );
  }
  async function retireTestNetwork(page) {
    const item = clients.find((candidate) => candidate.page === page);
    assert.ok(item, 'only an owned community client may retire its test network');
    if (!item.testNetwork || item.testNetworkRetired) return;
    await item.testNetwork.ready();
    await item.testNetwork.dispose();
    item.testNetworkRetired = true;
  }
  return { testNetworkReady, retireTestNetwork };
}

const httpPathParts = new Set(
  (
    'api auth rooms register login refresh logout sessions others profile profiles invites ' +
    'preferences password recovery key redeem passkey passkeys start finish authorize enroll ' +
    'ws-ticket attachments inbox messages reactions read unread contacts saved-rooms mine ' +
    'memberships preview identity image administration settings members bans reports history ' +
    'moderation lobby sanctions notification-preferences notifications push subscriptions updates telemetry'
  ).split(' '),
);
function httpRoute(request, base, httpCounts) {
  const url = new URL(request.url());
  if (url.origin !== new URL(base).origin || !url.pathname.startsWith('/api/')) return null;
  const path = url.pathname;
  const lane =
    path.startsWith('/api/auth/profiles/') || /^\/api\/rooms\/[^/]+\/image$/.test(path)
      ? 'profiles'
      : [
            'profile',
            'preferences',
            'attachments',
            'contacts',
            'saved-rooms',
            'notification-preferences',
            'inbox',
            'push',
            'updates',
          ].some(
            (prefix) => path === `/api/auth/${prefix}` || path.startsWith(`/api/auth/${prefix}/`),
          )
        ? 'account'
        : path.startsWith('/api/auth/')
          ? 'auth'
          : path === '/api/rooms' || path.startsWith('/api/rooms/')
            ? 'rooms'
            : path === '/api/telemetry'
              ? 'telemetry'
              : null;
  if (!lane) return null;
  const method = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'].includes(
    request.method(),
  )
    ? request.method()
    : 'OTHER';
  // Only fixed route words survive. Never retain query strings, account/room
  // identifiers, emails, headers, request bodies or invitation credentials.
  const template = path
    .split('/')
    .map((part) => (!part || httpPathParts.has(part) ? part : ':id'))
    .join('/');
  const candidate = `${method} ${template}`;
  const key = httpCounts.has(candidate) || httpCounts.size < 128 ? candidate : 'OTHER /api/:other';
  if (!httpCounts.has(key)) httpCounts.set(key, { lane, requests: 0, statuses: {}, failed: 0 });
  return { lane, count: httpCounts.get(key) };
}

async function closeCommunityClients({
  clients,
  browser,
  additionalBrowsers,
  report,
  deadline,
  saveReport,
  closeBrowser,
}) {
  let cleanupFailure;
  for (const item of clients) {
    await deadline(
      item.page
        .locator('dialog[open]')
        .evaluateAll((dialogs) => dialogs.forEach((dialog) => dialog.close())),
    ).catch(() => {});
    if (
      await item.page
        .locator('#leave-btn')
        .isVisible()
        .catch(() => false)
    )
      await item.page
        .locator('#leave-btn')
        .click()
        .catch(() => {});
  }
  const networkClosures = await Promise.allSettled(
    clients.map(async (item) => {
      if (item.testNetworkRetired) return;
      try {
        await item.testNetwork?.ready();
      } finally {
        await item.testNetwork?.dispose();
      }
    }),
  );
  const failedNetworkClose = networkClosures.find((result) => result.status === 'rejected');
  if (failedNetworkClose) {
    report.complete = false;
    report.passed = false;
    report.cleanupError = 'Owned WebKit test-network lifecycle failed.';
    cleanupFailure = failedNetworkClose.reason;
  }
  const closures = await Promise.allSettled(
    [...(browser ? [browser] : []), ...additionalBrowsers].map(async (owned) => {
      if (closeBrowser) await closeBrowser(owned);
      else await owned.close();
    }),
  );
  const failedClose = closures.find((result) => result.status === 'rejected');
  if (failedClose) {
    const error =
      failedClose.reason instanceof Error
        ? failedClose.reason
        : new Error('An owned browser could not be closed');
    report.complete = false;
    report.passed = false;
    report.cleanupError = error.message;
    cleanupFailure = error;
  }
  report.finishedAt = new Date().toISOString();
  saveReport();
  return cleanupFailure;
}

module.exports = {
  communityStartupReady,
  navigateCommunityPage,
  installCommunityCaptureObservation,
  reconnectMediaIdentity,
  mediaProgressDelta,
  waitForMediaStart,
  advancingReconnectMedia,
  createCommunityStep,
  createTestNetworkOwnership,
  closeCommunityClients,
  httpRoute,
};
