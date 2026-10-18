import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import stats from '../e2e/presentation-stats.cjs';

const report = (...rows) => new Map(rows.map((row, index) => [String(index), row]));
const evaluate = async (fn, scope) =>
  JSON.parse(JSON.stringify(await vm.runInNewContext(`(${fn.toString()})()`, scope)));

test('signal diagnostics retain only the bounded public numeric receive estimate', () => {
  let socket;
  class Socket {
    constructor() {
      socket = this;
      this.listeners = [];
    }
    addEventListener(name, callback) {
      if (name === 'message') this.listeners.push(callback);
    }
    receive(data) {
      for (const callback of this.listeners) callback({ data });
    }
  }
  const window = { WebSocket: Socket };
  vm.runInNewContext(`(${stats.installPresentationSignalStats.toString()})()`, {
    window,
    performance: { now: () => 42 },
  });
  new window.WebSocket('ws://owned.invalid');
  socket.receive(JSON.stringify({ type: 'privateMessage', text: 'private text' }));
  assert.equal(window.__presentationReceiveEstimate, null);
  socket.receive('invalid JSON');
  socket.receive(JSON.stringify({ type: 'connectionStats', availableBitrate: -1 }));
  assert.equal(window.__presentationReceiveEstimate, null);
  socket.receive(
    JSON.stringify({ type: 'connectionStats', availableBitrate: 950000, accountId: 'private-id' }),
  );
  assert.deepEqual(JSON.parse(JSON.stringify(window.__presentationReceiveEstimate)), {
    availableBitrate: 950000,
    observedAt: 42,
  });
  socket.receive(JSON.stringify({ type: 'connectionStats', availableBitrate: 'private text' }));
  assert.equal(window.__presentationReceiveEstimate.availableBitrate, 950000);
});

test('receiver diagnostics retain native layer/counters and only the selected incoming estimate', async () => {
  const screen = { kind: 'video' },
    camera = { kind: 'video' },
    audio = { kind: 'audio' };
  const receiver = (track, row) => ({ track, getStats: async () => report(row) });
  const peer = {
    connectionState: 'connected',
    getReceivers: () => [
      receiver(screen, {
        type: 'inbound-rtp',
        id: 'private-native-id',
        frameWidth: 480,
        frameHeight: 270,
        framesDecoded: 71,
        bytesReceived: 1200,
        packetsReceived: 12,
      }),
      receiver(camera, { type: 'inbound-rtp', framesDecoded: 88 }),
      receiver(audio, {
        type: 'inbound-rtp',
        packetsReceived: 200,
        totalSamplesReceived: 48000,
        concealedSamples: 240,
      }),
    ],
    getStats: async () =>
      report(
        { type: 'transport', selectedCandidatePairId: 'chosen' },
        {
          type: 'candidate-pair',
          id: 'chosen',
          state: 'succeeded',
          availableIncomingBitrate: 720000,
        },
        {
          type: 'candidate-pair',
          state: 'succeeded',
          availableIncomingBitrate: 99999999,
        },
      ),
  };
  const result = await evaluate(stats.presentationReceiverStats, {
    window: { __communityPeers: [peer] },
    document: {
      querySelector: (selector) => ({
        srcObject: {
          getVideoTracks: () => [selector.includes(':not(.screen-share)') ? camera : screen],
        },
      }),
    },
  });
  assert.equal(result.screenFrames, 71);
  assert.equal(result.cameraFrames, 88);
  assert.equal(result.audioPackets, 200);
  assert.equal(result.audioSamples, 48000);
  assert.equal(result.concealedSamples, 240);
  assert.equal(result.availableIncomingBitrate, 720000);
  assert.deepEqual(result.screenInbound, [
    {
      width: 480,
      height: 270,
      spatialLayer: 0,
      framesDecoded: 71,
      framesPerSecond: null,
      bytesReceived: 1200,
      packetsReceived: 12,
    },
  ]);
  assert.ok(!JSON.stringify(result).includes('private-native-id'));
});

test('missing incoming estimates stay unknown without breaking required native audio evidence', async () => {
  const result = await evaluate(stats.presentationReceiverStats, {
    window: {
      __communityPeers: [
        {
          connectionState: 'connected',
          getStats: async () => {
            throw new Error('not supported');
          },
          getReceivers: () => [
            {
              track: { kind: 'audio' },
              getStats: async () =>
                report({ type: 'inbound-rtp', packetsReceived: 5, jitterBufferEmittedCount: 480 }),
            },
          ],
        },
      ],
    },
    document: { querySelector: () => null },
  });
  assert.equal(result.availableIncomingBitrate, null);
  assert.equal(result.audioSamples, 480);
});

test('sender diagnostics expose actual per-encoding progress without changing publication', async () => {
  const screen = { kind: 'video' },
    camera = { kind: 'video' };
  const encodings = ['q', 'h', 'f'].map((rid, index) => ({
    rid,
    maxBitrate: (index + 1) * 100000,
  }));
  const parameters = {
    codecs: [{ mimeType: 'video/VP8' }, { mimeType: 'video/rtx' }],
    encodings,
  };
  const sender = {
    track: screen,
    getParameters: () => parameters,
    getStats: async () =>
      report(
        ...encodings.map((encoding, index) => ({
          id: `private-native-id-${index}`,
          type: 'outbound-rtp',
          rid: encoding.rid,
          frameWidth: [480, 960, 1920][index],
          frameHeight: [270, 540, 1080][index],
          framesEncoded: [300, 250, 0][index],
          bytesSent: [10000, 20000, 0][index],
          targetBitrate: 400000,
          qualityLimitationReason: 'bandwidth',
        })),
      ),
  };
  const result = await evaluate(stats.presentationSenderStats, {
    window: {
      __presentation: { tracks: [screen], captures: 2, displays: 2 },
      __communityPeers: [
        {
          getSenders: () => [
            sender,
            {
              track: camera,
              getParameters: () => ({
                encodings: [{}, {}, { active: false }],
              }),
            },
          ],
        },
      ],
    },
  });
  assert.equal(result.screenCodec, 'video/VP8');
  assert.equal(
    result.screenSpatialStreams,
    2,
    'configured layers do not imply frames were encoded',
  );
  assert.equal(result.screenOutbound[2].framesEncoded, 0);
  assert.equal(result.screenOutbound[2].encodingIndex, 2);
  assert.equal(result.screenOutbound[0].qualityLimitationReason, 'bandwidth');
  assert.equal(result.camera[2].active, false);
  assert.equal(result.captures, 2);
  assert.equal(result.displays, 2);
  assert.ok(!JSON.stringify(result).includes('private-native-id'));
  assert.ok(encodings.every((encoding) => encoding.active === undefined));
});
