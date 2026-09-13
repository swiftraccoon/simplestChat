/**
 * Bounded, payload-free lifecycle instrumentation for page.addInitScript().
 * This observer must run alone: older diagnostic registries retain closed peers
 * and therefore cannot be used for meaningful repeated-session memory samples.
 */
function installLifecycleObservation() {
  if ('__lifecycle' in window) throw new Error('Lifecycle observer already installed');

  const limits = Object.freeze({
    peers: 32,
    tracks: 64,
    sockets: 8,
    captures: 16,
    mediaElements: 256,
    statsPerPeer: 512,
    inboundPerPeer: 64,
    statsIdLength: 256,
    messageLength: 65536,
    counter: 10000,
  });
  const counters = {
    peersCreated: 0,
    peersClosed: 0,
    tracksObserved: 0,
    tracksEnded: 0,
    capturesRequested: 0,
    capturesResolved: 0,
    capturesRejected: 0,
    socketsCreated: 0,
    socketsClosed: 0,
    sentReconnect: 0,
    sentJoinRoom: 0,
    sentCreateSendTransport: 0,
    sentCreateRecvTransport: 0,
    sentProduce: 0,
    reconnectSuccess: 0,
    reconnectFailure: 0,
  };
  const peers = new Map();
  const tracks = new Map();
  const sockets = new Map();
  const peerRecords = new WeakMap();
  const trackRecords = new WeakMap();
  const socketRecords = new WeakMap();
  let pendingCaptures = 0;
  let nextStream = 0;
  let failure = null;
  let sampling = false;
  const integrity = [];

  const fail = (reason) => {
    failure ??= reason;
  };
  const increment = (key) => {
    if (counters[key] >= limits.counter) fail('counter limit exceeded');
    else counters[key]++;
    return counters[key];
  };
  const safely = (action) => {
    try {
      action();
    } catch {
      fail('native lifecycle observation unavailable');
    }
  };
  const replace = (object, key, value) => {
    const descriptor = Object.getOwnPropertyDescriptor(object, key);
    Object.defineProperty(object, key, { ...descriptor, value });
    integrity.push(() => object[key] === value);
  };
  const NativePeer = window.RTCPeerConnection;
  const NativeSocket = window.WebSocket;
  const NativeTrack = window.MediaStreamTrack;
  const NativeStream = window.MediaStream;
  const mediaDevices = navigator.mediaDevices;
  const nativeCapture = mediaDevices?.getUserMedia;
  const nativePeerClose = NativePeer?.prototype.close;
  const nativeStats = NativePeer?.prototype.getStats;
  const nativeTrackStop = NativeTrack?.prototype.stop;
  const nativeTrackClone = NativeTrack?.prototype.clone;
  const nativeStreamClone = NativeStream?.prototype.clone;
  const nativeGetTracks = NativeStream?.prototype.getTracks;
  const nativeSocketClose = NativeSocket?.prototype.close;
  const nativeSocketSend = NativeSocket?.prototype.send;
  const nativeThen = Promise.prototype.then;
  const nativeAddListener = window.EventTarget?.prototype.addEventListener;
  const nativeRemoveListener = window.EventTarget?.prototype.removeEventListener;
  const nativeQuery = document.querySelectorAll;
  const origin = window.location.origin;

  function messageKind(data, sent) {
    if (typeof data !== 'string') return;
    if (data.length > limits.messageLength) {
      fail('signaling message limit exceeded');
      return;
    }
    let message;
    try {
      message = JSON.parse(data);
    } catch {
      return;
    }
    if (sent) {
      const kinds = {
        reconnect: 'sentReconnect',
        joinRoom: 'sentJoinRoom',
        createSendTransport: 'sentCreateSendTransport',
        createRecvTransport: 'sentCreateRecvTransport',
        produce: 'sentProduce',
      };
      if (typeof message?.type === 'string' && Object.hasOwn(kinds, message.type))
        increment(kinds[message.type]);
    } else if (message?.type === 'reconnectResult' && typeof message.success === 'boolean') {
      increment(message.success ? 'reconnectSuccess' : 'reconnectFailure');
    }
  }

  function terminal(kind, object) {
    if (kind === 'peers') {
      if (
        !['new', 'connecting', 'connected', 'disconnected', 'failed', 'closed'].includes(
          object.connectionState,
        ) ||
        ![
          'stable',
          'have-local-offer',
          'have-remote-offer',
          'have-local-pranswer',
          'have-remote-pranswer',
          'closed',
        ].includes(object.signalingState)
      )
        throw new Error('Unavailable peer state');
      return object.connectionState === 'closed' || object.signalingState === 'closed';
    }
    if (kind === 'tracks') {
      if (!['live', 'ended'].includes(object.readyState))
        throw new Error('Unavailable track state');
      return object.readyState === 'ended';
    }
    if (![0, 1, 2, 3].includes(object.readyState)) throw new Error('Unavailable socket state');
    return object.readyState === 3;
  }

  function release(kind, record, object) {
    const collection = { peers, tracks, sockets }[kind];
    if (!collection.delete(record.ordinal)) return;
    increment({ peers: 'peersClosed', tracks: 'tracksEnded', sockets: 'socketsClosed' }[kind]);
    for (const event of record.events)
      Reflect.apply(nativeRemoveListener, object, [event, record.listener]);
    record.streams?.clear();
  }

  function sweep(kind) {
    const collection = { peers, tracks, sockets }[kind];
    for (const record of collection.values()) {
      const object = record.reference.deref();
      if (!object) {
        collection.delete(record.ordinal);
        // Collection alone does not establish native resource shutdown.
        fail('resource disappeared before terminal observation');
      } else if (terminal(kind, object)) release(kind, record, object);
      else if (
        (kind === 'peers' &&
          (object.close !== NativePeer.prototype.close || object.getStats !== nativeStats)) ||
        (kind === 'tracks' &&
          (object.stop !== NativeTrack.prototype.stop ||
            object.clone !== NativeTrack.prototype.clone)) ||
        (kind === 'sockets' &&
          (object.send !== NativeSocket.prototype.send || object.close !== nativeSocketClose))
      )
        fail('native resource method was replaced');
    }
  }

  function observe(kind, object) {
    const collection = { peers, tracks, sockets }[kind];
    const records = { peers: peerRecords, tracks: trackRecords, sockets: socketRecords }[kind];
    if (records.has(object)) return;
    sweep(kind);
    if (collection.size >= limits[kind]) {
      fail('active resource limit exceeded');
      return;
    }
    const ordinal = increment(
      { peers: 'peersCreated', tracks: 'tracksObserved', sockets: 'socketsCreated' }[kind],
    );
    const events = {
      peers: ['connectionstatechange', 'signalingstatechange'],
      tracks: ['ended'],
      sockets: ['close', 'message'],
    }[kind];
    const record = {
      ordinal,
      reference: new WeakRef(object),
      events,
      listener(event) {
        safely(() => {
          if (kind === 'sockets' && event.type === 'message') messageKind(event.data, false);
          if (terminal(kind, this)) release(kind, record, this);
        });
      },
      ...(kind === 'peers' ? { streams: new Map() } : {}),
    };
    records.set(object, record);
    collection.set(ordinal, record);
    for (const event of events) Reflect.apply(nativeAddListener, object, [event, record.listener]);
    if (terminal(kind, object)) release(kind, record, object);
  }

  function observeStream(stream) {
    const captured = Reflect.apply(nativeGetTracks, stream, []);
    if (captured.length > limits.tracks) {
      fail('capture track limit exceeded');
      return;
    }
    for (const track of captured) observe('tracks', track);
  }

  const supported =
    typeof WeakRef === 'function' &&
    typeof document.querySelectorAll === 'function' &&
    [
      NativePeer,
      NativeSocket,
      NativeTrack,
      NativeStream,
      nativeCapture,
      nativePeerClose,
      nativeStats,
      nativeTrackStop,
      nativeTrackClone,
      nativeStreamClone,
      nativeGetTracks,
      nativeSocketClose,
      nativeSocketSend,
      nativeAddListener,
      nativeRemoveListener,
    ].every((value) => typeof value === 'function');

  if (!supported) fail('unsupported browser APIs');
  else {
    safely(() => {
      integrity.push(
        () => navigator.mediaDevices === mediaDevices,
        () => window.MediaStreamTrack === NativeTrack,
        () => window.MediaStream === NativeStream,
        () => document.querySelectorAll === nativeQuery,
        () => NativePeer.prototype.getStats === nativeStats,
        () => NativeStream.prototype.getTracks === nativeGetTracks,
        () => NativeSocket.prototype.close === nativeSocketClose,
        () => window.EventTarget.prototype.addEventListener === nativeAddListener,
        () => window.EventTarget.prototype.removeEventListener === nativeRemoveListener,
      );
      replace(
        window,
        'RTCPeerConnection',
        new Proxy(NativePeer, {
          construct(target, args, newTarget) {
            const peer = Reflect.construct(target, args, newTarget);
            safely(() => observe('peers', peer));
            return peer;
          },
        }),
      );
      replace(NativePeer.prototype, 'close', function (...args) {
        const result = Reflect.apply(nativePeerClose, this, args);
        safely(() => {
          const record = peerRecords.get(this);
          if (record && terminal('peers', this)) release('peers', record, this);
        });
        return result;
      });
      replace(NativeTrack.prototype, 'stop', function (...args) {
        const result = Reflect.apply(nativeTrackStop, this, args);
        safely(() => {
          const record = trackRecords.get(this);
          if (record && terminal('tracks', this)) release('tracks', record, this);
        });
        return result;
      });
      replace(NativeTrack.prototype, 'clone', function (...args) {
        const clone = Reflect.apply(nativeTrackClone, this, args);
        safely(() => {
          if (trackRecords.has(this)) observe('tracks', clone);
        });
        return clone;
      });
      replace(NativeStream.prototype, 'clone', function (...args) {
        const clone = Reflect.apply(nativeStreamClone, this, args);
        safely(() => {
          const source = Reflect.apply(nativeGetTracks, this, []);
          if (source.length > limits.tracks) fail('clone track limit exceeded');
          else if (source.some((track) => trackRecords.has(track))) {
            if (source.every((track) => trackRecords.has(track))) observeStream(clone);
            else fail('mixed local and remote stream clone unavailable');
          }
        });
        return clone;
      });
      replace(mediaDevices, 'getUserMedia', function (...args) {
        increment('capturesRequested');
        pendingCaptures++;
        if (pendingCaptures > limits.captures) fail('pending capture limit exceeded');
        let result;
        try {
          result = Reflect.apply(nativeCapture, this, args);
        } catch (error) {
          pendingCaptures--;
          increment('capturesRejected');
          throw error;
        }
        // Keep the native promise and its rejection intact for application code.
        // Observation handlers never throw or retain the resolved stream.
        safely(() => {
          Reflect.apply(nativeThen, result, [
            (stream) => {
              pendingCaptures--;
              increment('capturesResolved');
              safely(() => observeStream(stream));
            },
            () => {
              pendingCaptures--;
              increment('capturesRejected');
            },
          ]);
        });
        return result;
      });
      replace(NativeSocket.prototype, 'send', function (...args) {
        const result = Reflect.apply(nativeSocketSend, this, args);
        safely(() => {
          const record = socketRecords.get(this);
          if (record && sockets.has(record.ordinal) && this.readyState === 1)
            messageKind(args[0], true);
        });
        return result;
      });
      replace(
        window,
        'WebSocket',
        new Proxy(NativeSocket, {
          construct(target, args, newTarget) {
            const socket = Reflect.construct(target, args, newTarget);
            safely(() => {
              const url = new URL(socket.url);
              const socketOrigin = `${url.protocol === 'wss:' ? 'https:' : 'http:'}//${url.host}`;
              if (
                ['ws:', 'wss:'].includes(url.protocol) &&
                socketOrigin === origin &&
                url.pathname === '/ws'
              )
                observe('sockets', socket);
            });
            return socket;
          },
        }),
      );
    });
  }

  function healthy() {
    safely(() => {
      if (window.__lifecycle !== api || integrity.some((check) => !check()))
        fail('observer was replaced');
      for (const kind of ['peers', 'tracks', 'sockets']) sweep(kind);
    });
    if (failure) throw new Error(`Lifecycle observation unavailable: ${failure}`);
  }

  function snapshot() {
    healthy();
    let attachedMediaElements = 0;
    safely(() => {
      const elements = Reflect.apply(nativeQuery, document, ['audio, video']);
      if (elements.length > limits.mediaElements) fail('media element limit exceeded');
      else for (const element of elements) if (element.srcObject !== null) attachedMediaElements++;
    });
    healthy();
    return {
      ...counters,
      openPeers: peers.size,
      liveLocalTracks: tracks.size,
      pendingCaptures,
      attachedMediaElements,
      openSockets: sockets.size,
    };
  }

  // Calling getStats in a synchronous helper avoids retaining the peer itself in
  // the sampling continuation if native stats settle after close().
  function requestStats(record) {
    const peer = record.reference.deref();
    if (!peer) throw new Error('Peer unavailable');
    return Reflect.apply(nativeStats, peer, []);
  }

  async function mediaSample() {
    healthy();
    if (sampling) throw new Error('Lifecycle media sample already pending');
    sampling = true;
    try {
      const result = [];
      for (const record of [...peers.values()]) {
        if (!peers.has(record.ordinal)) continue;
        let report;
        try {
          report = await requestStats(record);
        } catch {
          healthy();
          if (!peers.has(record.ordinal)) continue;
          fail('native peer stats unavailable');
          healthy();
        }
        healthy();
        if (!peers.has(record.ordinal)) continue;
        const inbound = [];
        const currentStreams = new Map();
        let visited = 0;
        for (const stat of report.values()) {
          if (++visited > limits.statsPerPeer) throw new Error('Stats limit exceeded');
          if (stat.type !== 'inbound-rtp' || !['audio', 'video'].includes(stat.kind)) continue;
          if (
            inbound.length >= limits.inboundPerPeer ||
            typeof stat.id !== 'string' ||
            stat.id.length === 0 ||
            stat.id.length > limits.statsIdLength ||
            currentStreams.has(stat.id)
          )
            throw new Error('Inbound stats unavailable');
          let stream = record.streams.get(stat.id);
          if (stream === undefined) {
            if (nextStream >= limits.counter) throw new Error('Stream limit exceeded');
            stream = ++nextStream;
          }
          currentStreams.set(stat.id, stream);
          const row = { stream, kind: stat.kind };
          for (const key of [
            'packetsReceived',
            'packetsLost',
            'bytesReceived',
            'framesDecoded',
            'framesDropped',
            'totalSamplesReceived',
            'concealedSamples',
            'silentConcealedSamples',
            'jitterBufferEmittedCount',
            'totalDecodeTime',
            'totalAudioEnergy',
            'totalSamplesDuration',
            'jitterBufferDelay',
          ]) {
            const value = stat[key];
            const floating = [
              'totalDecodeTime',
              'totalAudioEnergy',
              'totalSamplesDuration',
              'jitterBufferDelay',
            ].includes(key);
            if (
              value !== undefined &&
              (!Number.isFinite(value) ||
                (key !== 'packetsLost' && value < 0) ||
                (!floating && !Number.isSafeInteger(value)))
            )
              throw new Error('Numeric stats unavailable');
            row[key] = value ?? null;
          }
          inbound.push(row);
        }
        // Retain identities only for the current native report, never historical
        // stream objects or IDs. Ordinals cannot disclose native identifiers.
        record.streams = currentStreams;
        result.push({ peer: record.ordinal, inbound });
      }
      healthy();
      return { peers: result };
    } catch {
      fail('bounded media sample unavailable');
      healthy();
    } finally {
      sampling = false;
    }
  }

  function closeCurrentSocket() {
    healthy();
    const open = [...sockets.values()].filter(
      (record) => record.reference.deref()?.readyState === 1,
    );
    if (open.length !== 1) throw new Error('Expected exactly one open application socket');
    const socket = open[0].reference.deref();
    Reflect.apply(nativeSocketClose, socket, [4000, 'Synthetic reliability check']);
    return open[0].ordinal;
  }

  const api = Object.freeze({ snapshot, mediaSample, closeCurrentSocket });
  Object.defineProperty(window, '__lifecycle', {
    value: api,
    enumerable: false,
    writable: false,
    configurable: false,
  });
}

module.exports = { installLifecycleObservation };
