import assert from 'node:assert/strict';
import test from 'node:test';
import windows from '../e2e/presentation-window.cjs';

function fixture(heights, { windowMs = 1800, timeoutMs = 10000, matches = () => true } = {}) {
  let now = 0,
    index = 0;
  const observed = [];
  return {
    now: () => now,
    run: () =>
      windows.presentationWindow({
        sample: async () => ({
          index,
          height: heights[Math.min(index++, heights.length - 1)],
          text: matches(index - 1) ? 'SLIDES 1234' : '?',
        }),
        accepts: (point) => point.height === 1080 && point.text === 'SLIDES 1234',
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
