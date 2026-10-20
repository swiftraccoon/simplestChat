import assert from 'node:assert/strict';
import test from 'node:test';
import windows from '../e2e/presentation-window.cjs';

function fixture(
  heights,
  {
    windowMs = 1800,
    timeoutMs = 10000,
    matches = () => true,
    counters = () => ({}),
    progresses = () => true,
  } = {},
) {
  let now = 0,
    index = 0;
  const observed = [];
  return {
    now: () => now,
    run: () =>
      windows.presentationWindow({
        sample: async () => {
          const current = index++;
          return {
            index: current,
            height: heights[Math.min(current, heights.length - 1)],
            text: matches(current) ? 'SLIDES 1234' : '?',
            ...counters(current),
          };
        },
        accepts: (point) => point.height === 1080 && point.text === 'SLIDES 1234',
        progresses,
        wait: async (ms) => {
          now += ms;
        },
        now: () => now,
        windowMs,
        timeoutMs,
        observe: (point) => observed.push(point),
      }),
    observed,
  };
}

test('a transient recovery rise resets instead of accepting a later lower-resolution progress window', async () => {
  const f = fixture([270, 1080, 270, 1080, 1080, 1080, 1080]);
  const result = await f.run();
  assert.equal(result.before.index, 3);
  assert.equal(result.after.index, 6);
  assert.equal(result.durationMs, 1950);
  assert.ok(result.observations.every((point) => point.height === 1080));
  assert.equal(result.observations.length, 4, 'retain only the successful independent window');
});

test('unreadable text breaks a full-resolution window and repeated flaps cannot extend its deadline', async () => {
  const f = fixture([1080], {
    windowMs: 1800,
    timeoutMs: 4000,
    matches: (index) => index % 2 === 0,
  });
  await assert.rejects(f.run(), /No sustained readable presentation window/);
  assert.equal(f.now(), 4000);
});

test('an initial qualifying sample does not bypass the independent progress interval or timeout', async () => {
  const f = fixture([1080], { windowMs: 3000, timeoutMs: 1000 });
  await assert.rejects(f.run(), /No sustained readable presentation window/);
  assert.equal(f.now(), 1000);
  assert.equal(f.observed.length, 2);
});

function nativeCounters(index) {
  return {
    screenFrames: Math.floor(index / 3),
    cameraFrames: index * 5,
    audioPackets: index * 20,
    audioSamples: index * 19200,
    concealedSamples: 0,
  };
}

test('a valid slow layer waits for actual progress inside the original deadline', async () => {
  const f = fixture([1080], { counters: nativeCounters, progresses: windows.presentationProgress });
  const result = await f.run();
  assert.equal(result.before.index, 0);
  assert.equal(result.after.index, 6);
  assert.equal(result.durationMs, 3900, 'the same valid window extends beyond its minimum');
  assert.equal(result.after.screenFrames - result.before.screenFrames, 2);
});

test('samples taken before the receiver reports statistics cannot anchor the window for good', async () => {
  // Firefox answered empty statistics reports for seven seconds after a downlink
  // change on a hosted runner (2026-10-20): the readable frames that followed
  // must open the window on their own instead of being compared with the blanks.
  const blank = {
    screenFrames: 0,
    cameraFrames: 0,
    audioPackets: null,
    audioSamples: null,
    concealedSamples: null,
  };
  const f = fixture([1080], {
    counters: (index) => (index < 3 ? blank : { ...nativeCounters(index), screenFrames: index }),
    progresses: windows.presentationProgress,
  });
  const result = await f.run();
  assert.equal(result.before.index, 3, 'the first sample with statistics opens the window');
  assert.equal(result.after.index, 6);
  assert.equal(result.durationMs, 1950);
  assert.equal(result.observations.length, 4, 'retain only the accepted window');
});

test('a readable frozen layer exhausts the unchanged deadline despite camera and voice progress', async () => {
  const f = fixture([1080], {
    timeoutMs: 4000,
    counters: (index) => ({ ...nativeCounters(index), screenFrames: 12 }),
    progresses: windows.presentationProgress,
  });
  await assert.rejects(f.run(), /No sustained readable presentation window/);
  assert.equal(f.now(), 4000);
});

test('resolution loss discards earlier progress before accepting a sustained recovered window', async () => {
  const f = fixture([1080, 1080, 270, 1080], {
    counters: (index) => ({ ...nativeCounters(index), screenFrames: index }),
    progresses: windows.presentationProgress,
  });
  const result = await f.run();
  assert.equal(result.before.index, 3);
  assert.equal(result.after.index, 6);
  assert.ok(result.observations.every((point) => point.height === 1080));
});

test('voice, camera and concealment requirements remain part of window eligibility', () => {
  const before = nativeCounters(0),
    after = nativeCounters(6);
  assert.equal(windows.presentationProgress(before, after), true);
  for (const invalid of [
    { ...after, cameraFrames: 1 },
    { ...after, screenFrames: undefined, concealedSamples: null },
    { ...after, audioPackets: 9 },
    { ...after, audioSamples: null },
    { ...after, audioSamples: 4799 },
    { ...after, concealedSamples: after.audioSamples / 4 },
  ])
    assert.equal(windows.presentationProgress(before, invalid), false);
});
