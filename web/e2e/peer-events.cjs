/** Passive, bounded ICE event history; self-contained for page.addInitScript(). */
function installPeerEventTracing({ announcedIp = null } = {}) {
  const read = (object, key) => {
    try {
      return object?.[key];
    } catch {
      return undefined;
    }
  };
  const choose = (value, allowed) => (allowed.includes(value) ? value : null);
  const integer = (value, maximum) =>
    Number.isInteger(value) && value >= 0 && value <= maximum ? value : null;
  const states = {
    connectionState: ['new', 'connecting', 'connected', 'disconnected', 'failed', 'closed'],
    iceConnectionState: [
      'new',
      'checking',
      'connected',
      'completed',
      'disconnected',
      'failed',
      'closed',
    ],
    iceGatheringState: ['new', 'gathering', 'complete'],
  };
  function addressInfo(value) {
    if (typeof value !== 'string' || value.length > 253)
      return { kind: 'unknown', literal: null, loopback: null };
    if (
      /^(?:0|[1-9]\d{0,2})(?:\.(?:0|[1-9]\d{0,2})){3}$/.test(value) &&
      value.split('.').every((part) => Number(part) <= 255)
    ) {
      return { kind: 'ipv4', literal: value, loopback: value.startsWith('127.') };
    }
    if (value.includes(':')) {
      try {
        const literal = new URL(`http://[${value}]/`).hostname.slice(1, -1);
        return {
          kind: 'ipv6',
          literal,
          loopback: literal === '::1' || /^::ffff:7f[\da-f]{2}:/.test(literal),
        };
      } catch {
        return { kind: 'unknown', literal: null, loopback: null };
      }
    }
    if (!/^[\d.]+$/.test(value) && /^[a-z\d_](?:[a-z\d_.-]*[a-z\d_])?\.?$/i.test(value)) {
      return {
        kind: /\.local\.?$/i.test(value) ? 'mdns' : 'hostname',
        literal: null,
        loopback: null,
      };
    }
    return { kind: 'unknown', literal: null, loopback: null };
  }
  const announced = addressInfo(announcedIp);
  function classify(value) {
    const address = addressInfo(value);
    return {
      addressKind: address.kind,
      isLoopback: address.loopback,
      matchesAnnouncedIp:
        address.literal && announced.literal
          ? address.kind === announced.kind && address.literal === announced.literal
          : null,
    };
  }
  function summarizeCandidate(candidate, raw) {
    // Older engines may only expose the candidate string. Never retain it.
    const fields =
      typeof raw === 'string' && raw.length <= 4096 && /^candidate:\S+ /.test(raw)
        ? raw.trim().split(/\s+/)
        : [];
    const valid = fields.length >= 8 && fields[6] === 'typ';
    const fallback = (index) => (valid ? fields[index] : undefined);
    const numeric = (index) =>
      /^\d+$/.test(fallback(index) ?? '') ? Number(fallback(index)) : undefined;
    const tcpIndex = valid ? fields.indexOf('tcptype', 8) : -1;
    const tcpType = read(candidate, 'tcpType');
    return {
      type: choose(read(candidate, 'type') ?? fallback(7), ['host', 'srflx', 'prflx', 'relay']),
      protocol: choose((read(candidate, 'protocol') ?? fallback(2))?.toLowerCase?.(), [
        'udp',
        'tcp',
      ]),
      port: integer(read(candidate, 'port') ?? numeric(5), 65535),
      priority: integer(read(candidate, 'priority') ?? numeric(3), 4294967295),
      tcpType: choose(tcpType === undefined && tcpIndex >= 0 ? fields[tcpIndex + 1] : tcpType, [
        'active',
        'passive',
        'so',
      ]),
      ...classify(read(candidate, 'address') ?? fallback(4)),
    };
  }
  window.__communityPeers = [];
  window.__communityPeerEvents = new WeakMap();
  if (!window.RTCPeerConnection) return;
  window.RTCPeerConnection = new Proxy(window.RTCPeerConnection, {
    construct(target, args, newTarget) {
      const peer = Reflect.construct(target, args, newTarget);
      window.__communityPeers.push(peer);
      const started = performance.now();
      const trace = { events: [], dropped: 0, startedAt: started };
      window.__communityPeerEvents.set(peer, trace);
      function record(event, fields) {
        const elapsed = performance.now() - started;
        if (trace.events.length === 128) {
          trace.events.shift();
          trace.dropped++;
        }
        trace.events.push({
          event,
          elapsedMs: Number.isFinite(elapsed) ? Math.max(0, elapsed) : null,
          ...fields,
        });
      }
      record(
        'created',
        Object.fromEntries(
          Object.entries(states).map(([key, allowed]) => [key, choose(read(peer, key), allowed)]),
        ),
      );
      function listen(event, summarize) {
        // A diagnostic failure must not interfere with native event delivery.
        try {
          peer.addEventListener(
            event,
            (value) => {
              try {
                record(event, summarize(value));
              } catch {
                record(event, { error: 'Event summary unavailable' });
              }
            },
            { passive: true },
          );
        } catch {
          record(event, { error: 'Event listener unavailable' });
        }
      }
      for (const [event, property] of [
        ['icegatheringstatechange', 'iceGatheringState'],
        ['iceconnectionstatechange', 'iceConnectionState'],
        ['connectionstatechange', 'connectionState'],
      ]) {
        listen(event, () => ({ state: choose(read(peer, property), states[property]) }));
      }
      listen('icecandidate', (event) => {
        const candidate = read(event, 'candidate');
        if (candidate === null) return { phase: 'complete', candidate: null };
        if (!candidate || typeof candidate !== 'object')
          return { phase: 'unavailable', candidate: null };
        const raw = read(candidate, 'candidate');
        return raw === ''
          ? { phase: 'end-of-generation', candidate: null }
          : { phase: 'candidate', candidate: summarizeCandidate(candidate, raw) };
      });
      listen('icecandidateerror', (event) => {
        const url = read(event, 'url');
        const scheme =
          typeof url === 'string'
            ? /^(stun|stuns|turn|turns):/i.exec(url.slice(0, 6))?.[1].toLowerCase()
            : null;
        return {
          errorCode: integer(read(event, 'errorCode'), 65535),
          port: integer(read(event, 'port'), 65535),
          serverScheme: scheme ?? null,
          ...classify(read(event, 'address')),
        };
      });
      return peer;
    },
  });
}

module.exports = { installPeerEventTracing };
