import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import tracing from '../../web/e2e/peer-events.cjs';

const { installPeerEventTracing } = tracing;
const secret = 'PRIVATE_ICE_TRACE_SENTINEL';
const announcedIp = '192.0.2.73';
const plain = value => JSON.parse(JSON.stringify(value));

function fixture(t, options = { announcedIp }, { withConstructor = true } = {}) {
  let now = 1000;
  const forbiddenCalls = [], constructions = [];
  const forbidden = name => () => { forbiddenCalls.push(name); throw new Error(`Unexpected ${name}`); };
  class NativePeer {
    static fixtureMarker = 'native-static-property';
    connectionState = 'new';
    iceConnectionState = 'new';
    iceGatheringState = 'new';
    listeners = new Map();
    constructor(...args) { constructions.push({ peer: this, args, newTarget: new.target }); }
    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? [];
      listeners.push(listener); this.listeners.set(type, listeners);
    }
    emit(type, event = {}) {
      for (const listener of this.listeners.get(type) ?? []) listener.call(this, event);
      this[`on${type}`]?.call(this, event);
    }
  }
  for (const name of ['createOffer', 'createAnswer', 'setLocalDescription', 'setRemoteDescription',
    'addIceCandidate', 'addTrack', 'addTransceiver', 'removeTrack', 'restartIce', 'close', 'getStats']) {
    NativePeer.prototype[name] = forbidden(name);
  }
  const window = { RTCPeerConnection: withConstructor ? NativePeer : undefined };
  const install = runInNewContext(`(${installPeerEventTracing.toString()})`, {
    window, URL, performance: { now: () => now },
    setTimeout: forbidden('setTimeout'), setInterval: forbidden('setInterval'),
    clearTimeout: forbidden('clearTimeout'), clearInterval: forbidden('clearInterval'),
    fetch: forbidden('fetch'), XMLHttpRequest: forbidden('XMLHttpRequest'),
    navigator: { mediaDevices: { getUserMedia: forbidden('getUserMedia'), getDisplayMedia: forbidden('getDisplayMedia') } },
  }, { timeout: 1000 });
  install(options);
  t.after(() => assert.deepEqual(forbiddenCalls, [], 'tracing must remain passive and timer-free'));
  const record = peer => plain(window.__communityPeerEvents.get(peer));
  return {
    window, NativePeer, constructions, record,
    create: (...args) => new window.RTCPeerConnection(...args),
    advance(ms = 1) { now += ms; },
    last(peer) { return record(peer).events.at(-1); },
  };
}

function candidate(address = announcedIp, overrides = {}) {
  return {
    type: 'host', protocol: 'udp', port: 41010, priority: 2122260223, tcpType: null, address,
    foundation: secret, usernameFragment: secret,
    candidate: `candidate:${secret} 1 UDP 2122260223 ${address} 41010 typ host ufrag ${secret}`,
    ...overrides,
  };
}

function privateSummary(value, addresses = []) {
  const json = JSON.stringify(value);
  for (const forbidden of [secret, 'candidate:', 'turns:', 'stun:', ...addresses].filter(value => typeof value === 'string' && value.length > 0)) {
    assert.equal(json.includes(forbidden), false, `trace exposed ${forbidden}`);
  }
}

test('ICE tracing serializes independently and preserves native construction, arguments and prototypes', t => {
  const f = fixture(t);
  const configuration = Object.freeze({ iceServers: [{ urls: `turns:${secret}`, username: secret, credential: secret }] });
  const legacy = Object.freeze({ optional: [] });
  const peer = f.create(configuration, legacy);
  assert.ok(peer instanceof f.NativePeer);
  assert.ok(peer instanceof f.window.RTCPeerConnection);
  assert.equal(Object.getPrototypeOf(peer), f.NativePeer.prototype);
  assert.equal(f.window.RTCPeerConnection.prototype, f.NativePeer.prototype);
  assert.equal(f.window.RTCPeerConnection.fixtureMarker, 'native-static-property');
  assert.equal(f.constructions[0].args[0], configuration);
  assert.equal(f.constructions[0].args[1], legacy);
  class Derived extends f.window.RTCPeerConnection {}
  const derived = new Derived(configuration);
  assert.equal(Object.getPrototypeOf(derived), Derived.prototype);
  assert.equal(f.constructions[1].newTarget, Derived);
  assert.equal(f.window.__communityPeers[0], peer);
  assert.equal(f.window.__communityPeers[1], derived);
  assert.equal(f.record(peer).events[0].event, 'created');
  assert.equal(f.record(peer).events[0].elapsedMs, 0);
  privateSummary(f.record(peer));
});

test('ICE tracing tolerates an unavailable native peer constructor without side effects', t => {
  const f = fixture(t, {}, { withConstructor: false });
  assert.equal(f.window.RTCPeerConnection, undefined);
  assert.equal(f.window.__communityPeers.length, 0);
  assert.equal(Object.prototype.toString.call(f.window.__communityPeerEvents), '[object WeakMap]');
  assert.deepEqual(f.constructions, []);
});

test('ICE tracing leaves application listeners and property handlers on the original event', t => {
  const f = fixture(t), peer = f.create(), received = [];
  const handler = event => received.push(event);
  peer.onicecandidate = handler;
  peer.addEventListener('icecandidate', event => received.push(event));
  const event = Object.freeze({ candidate: Object.freeze(candidate()) });
  peer.emit('icecandidate', event);
  assert.equal(peer.onicecandidate, handler);
  assert.deepEqual(received, [event, event]);
  assert.equal(received[0], event);
  assert.equal(event.candidate.address, announcedIp);
  privateSummary(f.record(peer), [announcedIp]);
});

test('ICE state transitions retain names and peer-relative elapsed times', t => {
  const f = fixture(t), peer = f.create();
  for (const [event, property, state] of [
    ['icegatheringstatechange', 'iceGatheringState', 'gathering'],
    ['iceconnectionstatechange', 'iceConnectionState', 'checking'],
    ['connectionstatechange', 'connectionState', 'connected'],
  ]) {
    f.advance(10); peer[property] = state; peer.emit(event);
    assert.equal(f.last(peer).event, event); assert.equal(f.last(peer).state, state);
  }
  assert.deepEqual(f.record(peer).events.map(event => event.elapsedMs), [0, 10, 20, 30]);
});

test('IPv4 candidate summaries classify loopback and announcement matches without addresses', t => {
  const f = fixture(t), peer = f.create();
  for (const [address, isLoopback, matchesAnnouncedIp] of [
    [announcedIp, false, true], ['203.0.113.8', false, false], ['127.0.0.1', true, false], ['127.2.3.4', true, false],
  ]) {
    peer.emit('icecandidate', { candidate: candidate(address) });
    const event = f.last(peer);
    assert.equal(event.phase, 'candidate');
    assert.deepEqual(event.candidate, { type: 'host', protocol: 'udp', port: 41010, priority: 2122260223,
      tcpType: null, addressKind: 'ipv4', isLoopback, matchesAnnouncedIp });
    privateSummary(event, [address]);
  }
});

test('IPv6, mDNS, hostname and missing addresses are classified without resolving names', t => {
  const f = fixture(t), peer = f.create();
  for (const [address, addressKind, isLoopback, matchesAnnouncedIp] of [
    ['::1', 'ipv6', true, false], ['2001:db8::7', 'ipv6', false, false],
    [`${secret}.local`, 'mdns', null, null], [`${secret}.example.test`, 'hostname', null, null],
    [undefined, 'unknown', null, null], ['', 'unknown', null, null], ['999.2.3.4', 'unknown', null, null],
  ]) {
    peer.emit('icecandidate', { candidate: candidate(address, { address, candidate: undefined }) });
    const summary = f.last(peer).candidate;
    assert.equal(summary.addressKind, addressKind); assert.equal(summary.isLoopback, isLoopback);
    assert.equal(summary.matchesAnnouncedIp, matchesAnnouncedIp);
    privateSummary(summary, [address]);
  }
});

test('null and empty ICE candidates distinguish overall completion from end of generation', t => {
  const f = fixture(t), peer = f.create();
  peer.emit('icecandidate', { candidate: null });
  assert.equal(f.last(peer).phase, 'complete'); assert.equal(f.last(peer).candidate, null);
  peer.emit('icecandidate', { candidate: { candidate: '' } });
  assert.equal(f.last(peer).phase, 'end-of-generation'); assert.equal(f.last(peer).candidate, null);
  peer.emit('icecandidate', {});
  assert.equal(f.last(peer).phase, 'unavailable'); assert.equal(f.last(peer).candidate, null);
});

test('ICE candidate errors retain code and server scheme but never URLs or error text', t => {
  const f = fixture(t), peer = f.create();
  for (const serverScheme of ['stun', 'stuns', 'turn', 'turns']) {
    peer.emit('icecandidateerror', {
      errorCode: 701, address: announcedIp, port: 41010,
      url: `${serverScheme}:${secret}@${secret}.example.test:5349?transport=tcp`, errorText: secret,
    });
    const event = f.last(peer);
    assert.equal(event.event, 'icecandidateerror'); assert.equal(event.errorCode, 701);
    assert.equal(event.addressKind, 'ipv4'); assert.equal(event.isLoopback, false);
    assert.equal(event.matchesAnnouncedIp, true); assert.equal(event.port, 41010);
    assert.equal(event.serverScheme, serverScheme);
    privateSummary(event, [announcedIp]);
  }
  peer.emit('icecandidateerror', { errorCode: 701, url: `https://${secret}`, errorText: secret });
  assert.equal(f.last(peer).serverScheme, null);
  privateSummary(f.last(peer));
  peer.emit('icecandidateerror', { errorCode: secret, address: secret, port: secret, url: `javascript:${secret}`, errorText: secret });
  assert.equal(f.last(peer).errorCode, null); assert.equal(f.last(peer).port, null);
  assert.equal(f.last(peer).serverScheme, null); privateSummary(f.last(peer));
  peer.emit('icecandidateerror', { errorCode: 701 });
  assert.equal(f.last(peer).addressKind, 'unknown'); assert.equal(f.last(peer).isLoopback, null);
  assert.equal(f.last(peer).matchesAnnouncedIp, null);
});

test('raw candidate fallback is sanitized and native candidate properties take precedence', t => {
  const f = fixture(t), peer = f.create();
  const raw = `candidate:${secret} 1 TCP 1234 127.0.0.1 9 typ srflx raddr ${announcedIp} rport 41010 tcptype passive ufrag ${secret}`;
  peer.emit('icecandidate', { candidate: { candidate: raw } });
  assert.deepEqual(f.last(peer).candidate, { type: 'srflx', protocol: 'tcp', port: 9, priority: 1234,
    tcpType: 'passive', addressKind: 'ipv4', isLoopback: true, matchesAnnouncedIp: false });
  peer.emit('icecandidate', { candidate: candidate(announcedIp, { candidate: raw }) });
  assert.equal(f.last(peer).candidate.type, 'host'); assert.equal(f.last(peer).candidate.protocol, 'udp');
  assert.equal(f.last(peer).candidate.port, 41010); assert.equal(f.last(peer).candidate.matchesAnnouncedIp, true);
  assert.equal(f.last(peer).candidate.tcpType, null, 'explicit native null must not become a conflicting raw TCP type');
  privateSummary(f.record(peer), [announcedIp, '127.0.0.1']);
});

test('malformed and oversize candidates remain bounded and cannot leak raw input', t => {
  const f = fixture(t), peer = f.create();
  for (const raw of [`garbage ${secret}`, `candidate:${secret} ${'x'.repeat(100000)}`]) {
    assert.doesNotThrow(() => peer.emit('icecandidate', { candidate: { candidate: raw } }));
    const event = f.last(peer);
    assert.equal(event.event, 'icecandidate');
    assert.ok(JSON.stringify(event).length < 2048);
    privateSummary(event);
  }
});

test('throwing candidate, error and state getters cannot interrupt application event delivery or leak exceptions', t => {
  const f = fixture(t), peer = f.create();
  let delivered = 0;
  const throwing = keys => Object.defineProperties({}, Object.fromEntries(keys.map(key => [key, { get() { throw new Error(secret); } }])));
  peer.addEventListener('icecandidate', () => delivered++);
  peer.addEventListener('icecandidateerror', () => delivered++);
  peer.addEventListener('connectionstatechange', () => delivered++);
  assert.doesNotThrow(() => peer.emit('icecandidate', { candidate: throwing(['candidate', 'type', 'protocol', 'port', 'priority', 'tcpType', 'address']) }));
  assert.doesNotThrow(() => peer.emit('icecandidate', throwing(['candidate'])));
  assert.doesNotThrow(() => peer.emit('icecandidateerror', throwing(['errorCode', 'address', 'port', 'url', 'errorText'])));
  Object.defineProperty(peer, 'connectionState', { get() { throw new Error(secret); } });
  assert.doesNotThrow(() => peer.emit('connectionstatechange'));
  assert.equal(delivered, 4);
  privateSummary(f.record(peer));
});

test('ICE trace ring retains the newest 128 events with an exact dropped count', t => {
  const f = fixture(t), peer = f.create();
  for (let index = 0; index < 200; index++) { f.advance(); peer.emit('icegatheringstatechange'); }
  const record = f.record(peer);
  assert.equal(record.events.length, 128); assert.equal(record.dropped, 73);
  assert.equal(record.events[0].elapsedMs, 73); assert.equal(record.events.at(-1).elapsedMs, 200);
});

test('peer histories and elapsed clocks stay independent and absent announcement is unknown', t => {
  const f = fixture(t, {}), first = f.create();
  f.advance(20);
  const second = f.create();
  f.advance(5); first.emit('icecandidate', { candidate: candidate() }); second.emit('connectionstatechange');
  assert.equal(f.last(first).elapsedMs, 25); assert.equal(f.last(second).elapsedMs, 5);
  assert.equal(f.last(first).candidate.matchesAnnouncedIp, null);
  assert.notEqual(f.window.__communityPeerEvents.get(first), f.window.__communityPeerEvents.get(second));
  assert.equal(f.record(first).events.length, 2); assert.equal(f.record(second).events.length, 2);
  assert.equal(f.record(first).dropped, 0); assert.equal(f.record(second).dropped, 0);
});
