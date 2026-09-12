/** Test-only observation of owned native signaling; no credentials are retained. */
function installSignalingReconnectObservation() {
  const NativeWebSocket = window.WebSocket;
  if (typeof NativeWebSocket !== 'function') {
    // Passive installation must not replace an unrelated page failure. Fail
    // when the dedicated reconnect probe asks to use unavailable observation.
    const unavailable = () => {
      throw new Error('Native WebSocket unavailable');
    };
    window.__communitySignalingReconnect = { snapshot: unavailable, closeCurrent: unavailable };
    return;
  }
  const started = performance.now();
  const sockets = [];
  const events = [];
  const counters = {
    sentReconnect: 0,
    sentJoinRoom: 0,
    sentCreateSendTransport: 0,
    sentCreateRecvTransport: 0,
    sentProduce: 0,
    sentGetRoomSnapshot: 0,
    receivedReconnectResult: 0,
    reconnectSuccess: 0,
    reconnectFailure: 0,
    receivedRoomSnapshot: 0,
  };
  const sentKinds = {
    reconnect: 'sentReconnect',
    joinRoom: 'sentJoinRoom',
    createSendTransport: 'sentCreateSendTransport',
    createRecvTransport: 'sentCreateRecvTransport',
    produce: 'sentProduce',
    getRoomSnapshot: 'sentGetRoomSnapshot',
  };
  let socketsObserved = 0;
  let droppedSockets = 0;
  let droppedEvents = 0;
  let unavailable = false;
  const elapsed = () => Math.max(0, performance.now() - started);
  const increment = (field) => {
    if (counters[field] >= 10000) unavailable = true;
    else counters[field]++;
  };
  const record = (event, socketOrdinal, fields = {}) => {
    if (events.length === 64) {
      droppedEvents++;
      return;
    }
    events.push({ event, socketOrdinal, elapsedMs: elapsed(), ...fields });
  };
  const parse = (data) => {
    if (typeof data !== 'string' || data.length > 65536) return null;
    try {
      return JSON.parse(data);
    } catch {
      return null;
    }
  };
  const WrappedWebSocket = new Proxy(NativeWebSocket, {
    construct(target, args, newTarget) {
      // Native construction performs all argument conversion exactly once.
      const socket = Reflect.construct(target, args, newTarget);
      let address;
      try {
        address = new URL(socket.url);
      } catch {
        return socket;
      }
      if (
        address.protocol !== (window.location.protocol === 'https:' ? 'wss:' : 'ws:') ||
        address.host !== window.location.host ||
        address.pathname !== '/ws' ||
        address.username ||
        address.password ||
        address.search ||
        address.hash
      )
        return socket;

      socketsObserved++;
      if (sockets.length === 16) {
        droppedSockets++;
        return socket;
      }
      const ordinal = socketsObserved;
      const nativeSend = socket.send;
      const nativeClose = socket.close;
      const send = function (...values) {
        // A counter confirms a successful native send(), not remote receipt.
        const result = Reflect.apply(nativeSend, this, values);
        if (this !== socket) return result;
        const message = parse(values[0]);
        if (typeof message?.type === 'string' && Object.hasOwn(sentKinds, message.type)) {
          increment(sentKinds[message.type]);
          if (message.type === 'reconnect') record('reconnect-sent', ordinal);
        }
        return result;
      };
      try {
        socket.send = send;
        sockets.push({ socket, ordinal, send, nativeClose });
        socket.addEventListener('open', () => record('open', ordinal));
        socket.addEventListener('close', () => record('close', ordinal));
        socket.addEventListener('message', (event) => {
          const message = parse(event.data);
          if (message?.type === 'reconnectResult' && typeof message.success === 'boolean') {
            const success = message.success;
            increment('receivedReconnectResult');
            increment(success ? 'reconnectSuccess' : 'reconnectFailure');
            record('reconnect-result', ordinal, { success });
          } else if (message?.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
            increment('receivedRoomSnapshot');
            record('room-snapshot', ordinal);
          }
        });
      } catch {
        unavailable = true;
      }
      return socket;
    },
  });
  const guard = () => {
    if (
      unavailable ||
      droppedSockets ||
      droppedEvents ||
      window.WebSocket !== WrappedWebSocket ||
      window.__communitySignalingReconnect !== api ||
      sockets.some((entry) => entry.socket.send !== entry.send)
    )
      throw new Error('Signaling reconnect observation incomplete or replaced');
  };
  const api = {
    snapshot() {
      guard();
      return {
        schemaVersion: 1,
        socketsObserved,
        socketsRetained: sockets.length,
        openSocketOrdinals: sockets
          .filter((entry) => entry.socket.readyState === NativeWebSocket.OPEN)
          .map((entry) => entry.ordinal),
        counters: { ...counters },
        events: events.map((event) => ({ ...event })),
        droppedEvents,
        droppedSockets,
      };
    },
    closeCurrent() {
      guard();
      const current = sockets.filter((entry) => entry.socket.readyState === NativeWebSocket.OPEN);
      if (current.length !== 1) throw new Error('Expected exactly one open owned signaling socket');
      const entry = current[0];
      const result = { socketOrdinal: entry.ordinal, requestedAtMs: elapsed() };
      Reflect.apply(entry.nativeClose, entry.socket, [
        4000,
        'Owned signaling reconnect regression',
      ]);
      return result;
    },
  };
  window.WebSocket = WrappedWebSocket;
  window.__communitySignalingReconnect = api;
}

module.exports = { installSignalingReconnectObservation };
