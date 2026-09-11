import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import diagnostics from '../../web/e2e/peer-diagnostics.cjs';

const { collectPeerDiagnostics } = diagnostics;
const secret = 'PRIVATE_DIAGNOSTIC_SENTINEL';
const address = '198.51.100.71';
const sdp = [
  'v=0', `o=${secret} 42 1 IN IP4 ${address}`, 's=-', `c=IN IP4 ${address}`, 't=0 0',
  'a=group:BUNDLE 0 1', 'a=recvonly', 'a=end-of-candidates', 'a=ice-lite', 'a=setup:actpass',
  `a=ice-ufrag:${secret}`, `a=ice-pwd:${secret}`, `a=fingerprint:sha-256 ${secret}`,
  'm=audio 9 UDP/TLS/RTP/SAVPF 111', 'a=mid:0', 'a=rtpmap:111 opus/48000/2',
  `a=candidate:${secret} 1 UDP 2122260223 ${address} 41010 typ host`, 'a=end-of-candidates',
  'm=video 0 UDP/TLS/RTP/SAVPF 96', 'a=mid:1', 'a=inactive', 'a=rtpmap:96 VP8/90000',
  'a=bundle-only', 'a=rtcp-mux', 'a=setup:passive',
  `a=candidate:${secret} 1 TCP 1234 ${address} 9 typ srflx raddr ${address} rport 41010 tcptype passive`,
  `a=msid:${secret} ${secret}`, `a=ssrc:1234 cname:${secret}`, `a=ssrc:1234 msid:${secret} ${secret}`,
  'a=end-of-candidates', '',
].join('\r\n');

function peer(overrides = {}) {
  return {
    connectionState: 'connected', iceConnectionState: 'connected', signalingState: 'stable', iceGatheringState: 'complete',
    localDescription: null, remoteDescription: null, currentLocalDescription: null,
    currentRemoteDescription: null, pendingLocalDescription: null, pendingRemoteDescription: null,
    getTransceivers: () => [], getStats: async () => new Map(),
    ...overrides,
  };
}

async function collect(peers) {
  const mutations = [], delays = [], activeTimers = new Set();
  const forbidden = name => () => { mutations.push(name); throw new Error(`Unexpected mutation: ${name}`); };
  for (const value of peers) {
    for (const name of ['createOffer', 'createAnswer', 'setLocalDescription', 'setRemoteDescription',
      'addIceCandidate', 'addTrack', 'removeTrack', 'addTransceiver', 'restartIce', 'close']) {
      value[name] = forbidden(name);
    }
    Object.freeze(value);
  }
  const execute = runInNewContext(`(${collectPeerDiagnostics.toString()})`, {
    window: { __communityPeers: Object.freeze(peers) },
    navigator: { mediaDevices: { getUserMedia: forbidden('getUserMedia'), getDisplayMedia: forbidden('getDisplayMedia') } },
    setTimeout(callback, delay) {
      delays.push(delay);
      const timer = setTimeout(() => { activeTimers.delete(timer); callback(); }, Math.min(delay, 5));
      activeTimers.add(timer);
      return timer;
    },
    clearTimeout(timer) { clearTimeout(timer); activeTimers.delete(timer); },
  }, { timeout: 1000 });
  try {
    const result = JSON.parse(JSON.stringify(await execute()));
    assert.deepEqual(mutations, [], 'diagnostics must not negotiate, capture, close or otherwise mutate peers');
    assert.equal(activeTimers.size, 0, 'stats deadlines must always be cleared');
    assert.ok(delays.every(delay => delay > 0 && delay <= 2000));
    assert.doesNotMatch(JSON.stringify(result), new RegExp(`${secret}|${address.replaceAll('.', '\\.')}`));
    return result;
  } finally {
    for (const timer of activeTimers) clearTimeout(timer);
  }
}

test('peer diagnostics serialize without outer bindings and summarize rejected media and candidate types', async () => {
  const description = { type: 'offer', sdp };
  const [result] = await collect([peer({
    localDescription: description, remoteDescription: { type: 'answer', sdp },
    currentLocalDescription: description, currentRemoteDescription: { type: 'answer', sdp },
    pendingLocalDescription: description, pendingRemoteDescription: { type: 'answer', sdp },
  })]);
  assert.equal(result.iceGatheringState, 'complete');
  assert.deepEqual(Object.keys(result.descriptions), ['local', 'remote', 'currentLocal', 'currentRemote', 'pendingLocal', 'pendingRemote']);
  for (const [key, summary] of Object.entries(result.descriptions)) {
    assert.equal(summary.type, key.toLowerCase().includes('remote') ? 'answer' : 'offer');
    assert.deepEqual(summary.bundleMids, ['0', '1']);
    assert.equal(summary.endOfCandidates, true);
    assert.equal(summary.iceLite, true);
    assert.equal(summary.media.length, 2);
    const [audio, video] = summary.media;
    assert.equal(audio.kind, 'audio'); assert.equal(audio.port, 9); assert.equal(audio.direction, 'recvonly');
    assert.equal(audio.protocol, 'UDP/TLS/RTP/SAVPF'); assert.equal(audio.mid, '0');
    assert.deepEqual(audio.codecs, [{ payloadType: 111, name: 'opus' }]);
    assert.equal(audio.candidates.count, 1); assert.equal(audio.candidates.byType.host, 1);
    assert.equal(audio.candidates.byProtocol.udp, 1); assert.equal(audio.endOfCandidates, true);
    assert.equal(audio.setup, 'actpass');
    assert.equal(video.kind, 'video'); assert.equal(video.port, 0); assert.equal(video.direction, 'inactive');
    assert.deepEqual(video.codecs, [{ payloadType: 96, name: 'VP8' }]);
    assert.equal(video.candidates.byType.srflx, 1); assert.equal(video.candidates.byProtocol.tcp, 1);
    assert.equal(video.setup, 'passive'); assert.equal(video.bundleOnly, true); assert.equal(video.rtcpMux, true);
  }
});

test('peer diagnostics preserve transceiver direction and null current direction without track identity', async () => {
  const track = Object.freeze({ kind: 'video', enabled: true, muted: false, readyState: 'live', id: secret, label: secret,
    stop() { assert.fail('diagnostics stopped capture'); }, applyConstraints() { assert.fail('diagnostics changed capture'); } });
  const [result] = await collect([peer({ getTransceivers: () => [{
    mid: '1', direction: 'recvonly', currentDirection: null, stopped: false,
    sender: { track: null, replaceTrack() { assert.fail('diagnostics changed a sender'); } }, receiver: { track },
  }, { mid: '2', direction: 'inactive', currentDirection: 'inactive', stopped: true, sender: { track }, receiver: { track: null } }] })]);
  assert.deepEqual(result.transceivers, [{
    mid: '1', direction: 'recvonly', currentDirection: null, stopped: false,
    senderTrack: null, receiverTrack: { kind: 'video', enabled: true, muted: false, readyState: 'live' },
  }, {
    mid: '2', direction: 'inactive', currentDirection: 'inactive', stopped: true,
    senderTrack: { kind: 'video', enabled: true, muted: false, readyState: 'live' }, receiverTrack: null,
  }]);
});

test('peer diagnostics retain RTP counters and transport references while filtering private stats', async () => {
  const entries = [
    { type: 'transport', id: 'transport-safe', selectedCandidatePairId: 'pair-safe', bytesSent: 2340000, bytesReceived: 456, dtlsState: 'connected',
      localCertificateId: secret, remoteCertificateId: secret },
    { type: 'candidate-pair', id: 'pair-safe', localCandidateId: 'local-safe', remoteCandidateId: 'remote-safe', state: 'succeeded', nominated: true, bytesSent: 2340000, bytesReceived: 456 },
    { type: 'local-candidate', id: 'local-safe', candidateType: 'host', protocol: 'udp', address, ip: address, relatedAddress: address,
      usernameFragment: secret, foundation: secret, url: `turn:${secret}@${address}` },
    { type: 'outbound-rtp', id: 'outbound-safe', kind: 'video', codecId: 'codec-safe', transportId: 'transport-safe',
      ssrc: 1234, packetsSent: 1189, bytesSent: 2340000, framesEncoded: 1189, trackIdentifier: secret },
    { type: 'inbound-rtp', id: 'inbound-safe', kind: 'video', codecId: 'codec-safe', transportId: 'transport-safe',
      ssrc: 1234, packetsReceived: 1189, bytesReceived: 2340000, framesDecoded: 1189, trackIdentifier: secret },
    { type: 'codec', id: 'codec-safe', mimeType: 'video/VP8', payloadType: 96, clockRate: 90000, sdpFmtpLine: secret },
    { type: 'certificate', id: secret, fingerprint: secret, base64Certificate: secret },
    { type: 'media-source', id: secret, trackIdentifier: secret },
  ];
  const [result] = await collect([peer({ getStats: async () => new Map(entries.map((entry, index) => [index, entry])) })]);
  const byType = Object.fromEntries(result.stats.map(entry => [entry.type, entry]));
  assert.equal(byType.transport.selectedCandidatePairId, 'pair-safe');
  assert.equal(byType['candidate-pair'].localCandidateId, 'local-safe');
  assert.equal(byType['candidate-pair'].remoteCandidateId, 'remote-safe');
  assert.equal(byType['outbound-rtp'].framesEncoded, 1189);
  assert.equal(byType['outbound-rtp'].bytesSent, 2340000);
  assert.equal(byType['inbound-rtp'].framesDecoded, 1189);
  assert.equal(byType['inbound-rtp'].bytesReceived, 2340000);
  assert.equal(byType['inbound-rtp'].codecId, 'codec-safe');
  assert.equal(byType['inbound-rtp'].transportId, 'transport-safe');
  assert.equal(byType.codec.mimeType, 'video/VP8');
  assert.equal(byType.certificate, undefined); assert.equal(byType['media-source'], undefined);
});

test('closed peers remain reportable without invoking unsafe getters or stats', async () => {
  const value = peer({ connectionState: 'closed', iceConnectionState: 'closed', signalingState: 'closed', getStats() { assert.fail('closed peer stats read'); } });
  Object.defineProperty(value, 'localDescription', { get() { assert.fail('closed peer description read'); } });
  assert.deepEqual(await collect([value]), [{ connectionState: 'closed', iceConnectionState: 'closed', signalingState: 'closed', iceGatheringState: 'complete' }]);
});

test('missing and throwing description/transceiver accessors cannot prevent another peer snapshot', async () => {
  const value = peer({ getTransceivers: undefined });
  Object.defineProperty(value, 'localDescription', { get() { throw new Error(secret); } });
  const results = await collect([value, peer({ getTransceivers() { throw new Error(secret); } }), peer()]);
  assert.equal(results.length, 3);
  assert.ok(results[0].errors.length > 0); assert.ok(results[1].errors.length > 0);
  assert.deepEqual(results[2].stats, []);
  assert.equal(results[2].descriptions.local, null);
});

for (const [name, getStats] of [
  ['unsupported', undefined], ['throwing', () => { throw new Error(secret); }],
  ['rejecting', async () => { throw new Error(secret); }], ['timed out', () => new Promise(() => {})],
]) {
  test(`peer diagnostics preserve SDP/state when stats are ${name}`, { timeout: 1000 }, async () => {
    const [result] = await collect([peer({ localDescription: { type: 'offer', sdp }, getStats })]);
    assert.equal(result.connectionState, 'connected');
    assert.equal(result.descriptions.local.media[1].port, 0);
    assert.ok(result.errors.length > 0);
  });
}

test('peer diagnostics bound peer, transceiver, m-line and codec collections', async () => {
  const largeSdp = Array.from({ length: 40 }, (_, index) => [
    'm=audio 9 UDP/TLS/RTP/SAVPF 111', `a=mid:${index}`,
    ...Array.from({ length: 40 }, (_, codec) => `a=rtpmap:${codec} opus/48000/2`),
  ].join('\r\n')).join('\r\n');
  assert.ok(largeSdp.length < 65536);
  const results = await collect(Array.from({ length: 20 }, (_, peerIndex) => peer({
    localDescription: { type: 'offer', sdp: largeSdp },
    getTransceivers: () => Array.from({ length: 40 }, (_, index) => ({ mid: String(peerIndex * 100 + index), direction: 'recvonly', currentDirection: null, sender: { track: null }, receiver: { track: null } })),
  })));
  assert.equal(results.length, 16);
  assert.equal(results[0].transceivers.length, 32);
  assert.equal(results[0].transceivers[0].mid, '400', 'the most recent 16 peers are retained');
  assert.equal(results[0].omittedPeers, 4);
  assert.equal(results[0].descriptions.local.media.length, 32);
  assert.equal(results[0].descriptions.local.media[0].codecs.length, 32);
  assert.ok(results.some(result => result.truncated?.length > 0));
});

test('oversize SDP is replaced by a size error, not partially retained', async () => {
  const [result] = await collect([peer({ localDescription: { type: 'offer', sdp: sdp + 'x'.repeat(65536) } })]);
  assert.deepEqual(result.descriptions.local, { type: 'offer', error: 'SDP size limit exceeded' });
});

test('peer stats are capped without retaining excess records', async () => {
  let yielded = 0;
  const [result] = await collect([peer({ getStats: async () => ({
    *values() { for (let index = 0; index < 1000; index++) { yielded++; yield { type: 'transport', id: `transport-${index}`, bytesSent: index }; } },
  }) })]);
  assert.equal(result.stats.length, 256);
  assert.ok(yielded <= 257, 'collector must stop reading the iterator at its bound');
  assert.ok(result.truncated.length > 0);
});
