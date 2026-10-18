/** Require one uninterrupted independent observation window inside a fixed
 * deadline. A transient high-resolution sample cannot satisfy recovery. The
 * retained samples cover only the current window, keeping evidence bounded. */
async function presentationWindow({
  sample,
  accepts,
  wait,
  now = Date.now,
  timeoutMs,
  windowMs,
  observe = () => {},
  progresses = () => true,
}) {
  const deadline = now() + timeoutMs;
  let started = 0;
  let observations = [];
  while (now() < deadline) {
    const point = await sample();
    const at = now();
    observe(point);
    if (at > deadline) break;
    if (!accepts(point)) observations = [];
    else {
      if (observations.length === 0) started = at;
      observations.push(point);
      if (at - started >= windowMs && progresses(observations[0], point))
        return { before: observations[0], after: point, observations, durationMs: at - started };
    }
    await wait(Math.min(650, Math.max(0, deadline - now())));
  }
  throw new Error('No sustained readable presentation window before the deadline');
}

/** The same progress requirements apply throughout all resolution phases. */
function presentationProgress(before, after) {
  if (!(
    after.screenFrames >= before.screenFrames + 2 &&
    after.cameraFrames >= before.cameraFrames + 2 &&
    after.audioPackets >= before.audioPackets + 10 &&
    before.audioSamples !== null &&
    after.audioSamples !== null &&
    after.audioSamples >= before.audioSamples + 4800
  ))
    return false;
  const samples = after.audioSamples - before.audioSamples;
  const concealed =
    before.concealedSamples !== null && after.concealedSamples !== null
      ? after.concealedSamples - before.concealedSamples
      : null;
  return concealed === null || concealed / samples < 0.25;
}

module.exports = { presentationWindow, presentationProgress };
