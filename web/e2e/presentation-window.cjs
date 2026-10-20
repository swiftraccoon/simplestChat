/** Require one uninterrupted independent observation window inside a fixed
 * deadline. A transient high-resolution sample cannot satisfy recovery. Any
 * sample of the uninterrupted run at least `windowMs` old may open the window,
 * oldest first: the oldest lets a slow layer's progress accumulate, a later one
 * rescues a run whose first samples carried no statistics yet (Firefox answers
 * empty reports for seconds after a change). The retained samples cover only
 * the accepted window, keeping evidence bounded. */
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
  let run = [];
  while (now() < deadline) {
    const point = await sample();
    const at = now();
    observe(point);
    if (at > deadline) break;
    if (!accepts(point)) run = [];
    else {
      run.push({ at, point });
      const start = run.find(
        (entry) => at - entry.at >= windowMs && progresses(entry.point, point),
      );
      if (start)
        return {
          before: start.point,
          after: point,
          observations: run.slice(run.indexOf(start)).map((entry) => entry.point),
          durationMs: at - start.at,
        };
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
