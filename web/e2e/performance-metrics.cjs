/** Serialized by page.evaluate: keep this reader self-contained in the page. */
async function firstDecodedVideoFrame() {
  const video = [...document.querySelectorAll('.video-tile:not(.local) video')].find(
    (element) => element.videoWidth > 0 && element.videoHeight > 0,
  );
  if (!video) return false;
  for (const peer of window.__perfPeers) {
    if (peer.connectionState === 'closed') continue;
    for (const stat of (await peer.getStats()).values()) {
      if (
        stat.type === 'inbound-rtp' &&
        stat.kind === 'video' &&
        Number.isSafeInteger(stat.framesDecoded) &&
        stat.framesDecoded > 0
      ) {
        return {
          receiverId: stat.id,
          framesDecoded: stat.framesDecoded,
          videoWidth: video.videoWidth,
          videoHeight: video.videoHeight,
        };
      }
    }
  }
  return false;
}

/** Await each asynchronous stats read explicitly; a Promise is not evidence. */
async function waitForDecodedVideoFrame(
  readEvidence,
  wait,
  timeoutMs = 15000,
  now = () => performance.now(),
) {
  const deadline = now() + timeoutMs;
  const timedOut = () => new Error('Timed out waiting for native decoded-video-frame evidence');
  let cancelled = false;
  let timer;
  const poll = async () => {
    while (!cancelled && now() < deadline) {
      const evidence = await readEvidence();
      if (cancelled) return;
      if (
        now() < deadline &&
        typeof evidence?.receiverId === 'string' &&
        evidence.receiverId.length > 0 &&
        Number.isSafeInteger(evidence.framesDecoded) &&
        evidence.framesDecoded > 0 &&
        Number.isSafeInteger(evidence.videoWidth) &&
        evidence.videoWidth > 0 &&
        Number.isSafeInteger(evidence.videoHeight) &&
        evidence.videoHeight > 0
      )
        return evidence;
      const remaining = deadline - now();
      if (remaining > 0) await wait(Math.min(25, remaining));
    }
    throw timedOut();
  };
  try {
    // page.evaluate has no action timeout. Race the entire operation so browser
    // cleanup can run even if a native stats read or polling delay stays pending.
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => {
        cancelled = true;
        reject(timedOut());
      }, timeoutMs);
    });
    return await Promise.race([poll(), timeout]);
  } finally {
    cancelled = true;
    clearTimeout(timer);
  }
}

function decodedVideoFrames(before, after) {
  const previous = new Map(
    before
      .filter((item) => item.kind === 'video')
      .map((item) => [item.id, item.framesDecoded || 0]),
  );
  return after
    .filter((item) => item.kind === 'video')
    .reduce((sum, item) => {
      return sum + Math.max(0, (item.framesDecoded || 0) - (previous.get(item.id) || 0));
    }, 0);
}
module.exports = { decodedVideoFrames, firstDecodedVideoFrame, waitForDecodedVideoFrame };
