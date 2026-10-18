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
  assert.deepEqual(result.probator, {
    receiverIdentified: false,
    packetsReceived: null,
    bytesReceived: null,
  });
});

test('receiver negotiation and probation diagnostics expose only fixed names and numeric counters', async () => {
  const secret = 'private-native-description';
  const remote = [
    'v=0',
    `a=ice-pwd:${secret}`,
    `a=fingerprint:sha-256 ${secret}`,
    `a=msid:${secret} ${secret}`,
    'a=rtcp-fb:96 transport-cc',
    'a=rtcp-fb:* goog-remb',
    'a=rtcp-fb:96 nack pli',
    `a=rtcp-fb:96 ${secret}`,
    'a=extmap:4/sendonly http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01',
    'a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time',
    'a=extmap:3 urn:ietf:params:rtp-hdrext:sdes:mid',
    `a=extmap:9 https://${secret}/extension`,
    'm=video 7 UDP/TLS/RTP/SAVPF 127',
    'a=mid:probator',
    `a=ssrc:1234 cname:${secret}`,
  ].join('\r\n');
  const result = await evaluate(stats.presentationReceiverStats, {
    window: {
      __communityPeers: [
        {
          connectionState: 'connected',
          remoteDescription: { type: 'offer', sdp: remote },
          localDescription: {
            type: 'answer',
            sdp: [
              `a=ice-pwd:${secret}`,
              'a=rtcp-fb:96 goog-remb',
              'a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time',
              'a=mid:probator',
            ].join('\r\n'),
          },
          getReceivers: () => [
            { track: { kind: 'video', id: 'probator' }, getStats: async () => report() },
          ],
          getStats: async () =>
            report(
              {
                type: 'inbound-rtp',
                ssrc: 1234,
                id: secret,
                packetsReceived: 17,
                bytesReceived: 501,
              },
              {
                type: 'inbound-rtp',
                ssrc: 5678,
                id: secret,
                packetsReceived: 900,
                bytesReceived: 900,
              },
            ),
        },
      ],
    },
    document: { querySelector: () => null },
  });
  assert.deepEqual(result.remoteDescriptions, [
    {
      available: true,
      type: 'offer',
      feedback: ['transport-cc', 'goog-remb', 'nack pli'],
      headerExtensions: ['transport-wide-cc', 'abs-send-time', 'mid'],
      probator: true,
    },
  ]);
  assert.deepEqual(result.probator, {
    receiverIdentified: true,
    packetsReceived: 17,
    bytesReceived: 501,
  });
  assert.deepEqual(
    result.localDescriptions,
    [
      {
        available: true,
        type: 'answer',
        feedback: ['goog-remb'],
        headerExtensions: ['abs-send-time'],
        probator: true,
      },
    ],
    'the local answer never inherits transport-cc offered only by the remote description',
  );
  assert.doesNotMatch(
    JSON.stringify(result),
    /private-native-description|ice-pwd|fingerprint|ssrc|1234|5678/,
  );
});

test('unsupported probation fields stay unknown and oversized descriptions are never retained or parsed', async () => {
  let calls = 0;
  const peer = {
    connectionState: 'connected',
    remoteDescription: { type: 'private-type', sdp: `a=mid:probator\n${'x'.repeat(262144)}` },
    getReceivers: () => [{ track: { kind: 'video' }, getStats: async () => report() }],
    getStats: async () => {
      calls++;
      return report({
        type: 'inbound-rtp',
        mid: 'probator',
        packetsReceived: -1,
        bytesReceived: Infinity,
      });
    },
  };
  const result = await evaluate(stats.presentationReceiverStats, {
    window: { __communityPeers: Array.from({ length: 5 }, () => peer) },
    document: { querySelector: () => null },
  });
  assert.equal(
    calls,
    5,
    'diagnostics reuse the existing one optional aggregate stats request per peer',
  );
  assert.equal(result.remoteDescriptions.length, 4);
  assert.equal(result.localDescriptions.length, 4);
  assert.deepEqual(result.remoteDescriptions[0], {
    available: false,
    type: null,
    feedback: [],
    headerExtensions: [],
    probator: null,
  });
  assert.deepEqual(result.probator, {
    receiverIdentified: true,
    packetsReceived: null,
    bytesReceived: null,
  });
  assert.deepEqual(result.localDescriptions[0], {
    available: false,
    type: null,
    feedback: [],
    headerExtensions: [],
    probator: null,
  });
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
