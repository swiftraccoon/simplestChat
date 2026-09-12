import assert from 'node:assert/strict';
import test from 'node:test';
import metrics from '../e2e/performance-metrics.cjs';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

test('browser performance uses the shared fake-device options without autoplay bypass', () => {
  const source = readFileSync(new URL('../e2e/performance.cjs', import.meta.url), 'utf8');
  assert.match(source, /require\('\.\/browser-options\.cjs'\)/);
  assert.match(source, /chromium\.launch\(browserConfiguration\.launchOptions\)/);
  assert.match(source, /\.\.\.browserConfiguration\.contextOptions/);
  assert.doesNotMatch(source, /autoplay-policy|no-user-gesture-required|ignoreDefaultArgs/);
  assert.match(source, /evaluate\(firstDecodedVideoFrame\)/);
  assert.match(source, /await waitForDecodedVideoFrame\(/);
  assert.doesNotMatch(source, /waitForFunction\(firstDecodedVideoFrame\)/);
});

test('decode polling awaits false asynchronous reads until valid evidence arrives', async () => {
  let clock = 0;
  let reads = 0;
  const expected = { receiverId: 'camera', framesDecoded: 1, videoWidth: 640, videoHeight: 480 };
  const evidence = await metrics.waitForDecodedVideoFrame(
    async () => {
      await Promise.resolve();
      return ++reads === 3 ? expected : false;
    },
    async (milliseconds) => {
      clock += milliseconds;
    },
    100,
    () => clock,
  );
  assert.deepEqual(evidence, expected);
  assert.equal(reads, 3);
  assert.equal(clock, 50);
});

test('decode polling times out on false or malformed evidence', async () => {
  for (const evidence of [
    false,
    null,
    {},
    { receiverId: 'camera', framesDecoded: 0, videoWidth: 640, videoHeight: 480 },
  ]) {
    let clock = 0;
    await assert.rejects(
      metrics.waitForDecodedVideoFrame(
        async () => evidence,
        async (milliseconds) => {
          clock += milliseconds;
        },
        40,
        () => clock,
      ),
      /Timed out waiting for native decoded-video-frame evidence/,
    );
    assert.equal(clock, 40);
  }
});

test('decode polling rejects late evidence and preserves stats-read failures', async () => {
  let clock = 0;
  const evidence = { receiverId: 'camera', framesDecoded: 1, videoWidth: 640, videoHeight: 480 };
  await assert.rejects(
    metrics.waitForDecodedVideoFrame(
      async () => {
        clock = 101;
        return evidence;
      },
      async () => {},
      100,
      () => clock,
    ),
    /Timed out/,
  );
  await assert.rejects(
    metrics.waitForDecodedVideoFrame(
      async () => {
        throw new Error('stats unavailable');
      },
      async () => {},
    ),
    /stats unavailable/,
  );
});

test(
  'decode polling times out a pending read and does not resume work afterward',
  { timeout: 1000 },
  async () => {
    let finishRead;
    let reads = 0;
    let waits = 0;
    const pendingRead = new Promise((resolve) => {
      finishRead = resolve;
    });
    await assert.rejects(
      metrics.waitForDecodedVideoFrame(
        () => {
          reads++;
          return pendingRead;
        },
        async () => {
          waits++;
        },
        10,
      ),
      /Timed out/,
    );
    finishRead({ receiverId: 'camera', framesDecoded: 1, videoWidth: 640, videoHeight: 480 });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(reads, 1);
    assert.equal(waits, 0);
  },
);

test(
  'decode polling times out a permanently pending polling delay',
  { timeout: 1000 },
  async () => {
    await assert.rejects(
      metrics.waitForDecodedVideoFrame(
        async () => false,
        () => new Promise(() => {}),
        10,
      ),
      /Timed out/,
    );
  },
);

async function firstFrame(
  stats,
  videos = [{ videoWidth: 640, videoHeight: 480 }],
  state = 'connected',
) {
  return runInNewContext(`(${metrics.firstDecodedVideoFrame.toString()})()`, {
    document: { querySelectorAll: () => videos },
    window: {
      __perfPeers: [
        {
          connectionState: state,
          getStats: async () => new Map(stats.map((stat, i) => [i, stat])),
        },
      ],
    },
  });
}

test('first-frame timing requires native decoding, not dimensions or received packets', async () => {
  const video = {
    type: 'inbound-rtp',
    kind: 'video',
    id: 'camera',
    framesReceived: 10,
    packetsReceived: 100,
  };
  for (const framesDecoded of [undefined, 0, -1, NaN, Infinity, '1', 0.5]) {
    assert.equal(await firstFrame([{ ...video, framesDecoded }]), false);
  }
  const evidence = await firstFrame([
    { ...video, id: 'probation', framesDecoded: 0 },
    { ...video, framesDecoded: 1 },
  ]);
  assert.equal(evidence.receiverId, 'camera');
  assert.equal(evidence.framesDecoded, 1);
  assert.equal(evidence.videoWidth, 640);
  assert.equal(evidence.videoHeight, 480);
});

test('first-frame timing excludes audio, outbound, closed and dimensionless media', async () => {
  const video = { type: 'inbound-rtp', kind: 'video', id: 'camera', framesDecoded: 10 };
  assert.equal(await firstFrame([{ ...video, kind: 'audio' }]), false);
  assert.equal(await firstFrame([{ ...video, type: 'outbound-rtp' }]), false);
  assert.equal(await firstFrame([video], [], 'connected'), false);
  assert.equal(await firstFrame([video], [{ videoWidth: 640, videoHeight: 0 }]), false);
  assert.equal(await firstFrame([video], undefined, 'closed'), false);
});

test('browser frame deltas ignore an idle probation SSRC before the active receiver', () => {
  const before = [
    { id: 'probation', kind: 'video', framesDecoded: 0 },
    { id: 'camera', kind: 'video', framesDecoded: 1 },
  ];
  const after = [
    { id: 'probation', kind: 'video', framesDecoded: 0 },
    { id: 'camera', kind: 'video', framesDecoded: 82 },
    { id: 'audio', kind: 'audio' },
  ];
  assert.equal(metrics.decodedVideoFrames(before, after), 81);
  assert.equal(metrics.decodedVideoFrames(after, after), 0);
});
