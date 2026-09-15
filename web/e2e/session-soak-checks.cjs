/** Bounded, opt-in continuity workload; no clocks, tokens, or server limits are changed. */
const assert = require('node:assert/strict');

function configuration(env = process.env) {
  assert.equal(env.SESSION_SOAK_E2E, '1', 'Set SESSION_SOAK_E2E=1');
  assert.equal(env.DISPOSABLE_TEST_DATABASE, '1', 'Use an owned disposable database');
  assert.match(env.TEST_METRICS_TOKEN || '', /^[a-f0-9]{64}$/, 'Use the owned server helper');
  assert.match(env.TEST_SERVER_PID || '', /^[1-9][0-9]*$/, 'Use the owned server helper');
  const serverPid = Number(env.TEST_SERVER_PID);
  assert.ok(Number.isSafeInteger(serverPid) && serverPid > 1 && serverPid <= 2147483647);
  const base = new URL(env.BASE_URL || 'http://127.0.0.1:3119');
  assert.ok(
    ['http:', 'https:'].includes(base.protocol) &&
      ['127.0.0.1', 'localhost', '[::1]'].includes(base.hostname) &&
      !base.username &&
      !base.password &&
      !base.search &&
      !base.hash &&
      base.pathname === '/',
    'Session soak requires a credential-free loopback origin',
  );
  assert.notEqual(env.NODE_TLS_REJECT_UNAUTHORIZED, '0', 'TLS verification must remain enabled');
  const profile = env.SESSION_SOAK_PROFILE ?? 'full';
  assert.ok(['full', 'smoke'].includes(profile), 'SESSION_SOAK_PROFILE must be full or smoke');
  return {
    base: base.origin,
    serverPid,
    profile,
    clients: 2,
    durationMs: profile === 'full' ? 1200000 : 30000,
    messageIntervalMs: 3000,
    recoveryMs: 60000,
    deadlineMs: profile === 'full' ? 1380000 : 180000,
  };
}

/** Decode only our own successful auth response; never return/persist its credential. */
function sessionExpiry(response, nowMs) {
  assert.ok(typeof response?.token === 'string' && response.token.length <= 16384);
  const parts = response.token.split('.');
  assert.equal(parts.length, 3);
  assert.ok(parts.every((part) => /^[A-Za-z0-9_-]+$/.test(part)));
  const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
  assert.ok(Number.isSafeInteger(payload.exp) && payload.exp > 0);
  const expiryMs = payload.exp * 1000;
  assert.ok(Number.isSafeInteger(expiryMs) && Number.isFinite(nowMs));
  // An unexpected lifetime is not permission to shorten or advance the soak.
  assert.ok(expiryMs > nowMs && expiryMs <= nowMs + 16 * 60000);
  return expiryMs;
}

/** Full rejoin may recover delivery but is never a seamless continuity pass. */
function continuityFailure(value) {
  if (value.failure) return value.failure;
  if (value.captureRequests) return 'unexpected_capture_request';
  if (value.socketCloses) return 'authenticated_socket_closed';
  if (value.resumeRejected) return 'resume_rejected';
  if (value.freshJoins) return 'fresh_room_join';
  if (value.protocolErrors) return 'signaling_request_rejected';
  if (value.authenticationRenewalFailed) return 'authentication_renewal_failed';
  if (value.disabledTransitions) return 'chat_input_disabled';
  if (
    !Number.isSafeInteger(value.initialNavigationCount) ||
    value.initialNavigationCount < 1 ||
    value.navigationCount !== value.initialNavigationCount
  )
    return 'page_navigated';
  return null;
}

function expiryCoverage(authentication, endedAtMs) {
  return (
    authentication.length === 2 &&
    authentication.every(
      (client) =>
        Number.isFinite(client.originalExpiresAtMs) &&
        endedAtMs >= client.originalExpiresAtMs + 60000 &&
        client.refreshFailures === 0 &&
        client.refreshes.some(
          (refresh) =>
            refresh.atMs < client.originalExpiresAtMs &&
            refresh.expiresAtMs > client.originalExpiresAtMs &&
            refresh.atMs >= client.registeredAtMs + 600000,
        ),
    )
  );
}

/** Native socket observation only: no frames are delayed, forged, or replayed. */
function installSessionObservation() {
  if ('__sessionSoak' in window) throw new Error('Session observer already installed');
  const NativeSocket = window.WebSocket;
  const sockets = [];
  const messages = new Map();
  const events = [];
  const counters = {
    captureRequests: 0,
    socketCloses: 0,
    freshJoins: 0,
    resumeAccepted: 0,
    resumeRejected: 0,
    protocolErrors: 0,
    disabledTransitions: 0,
    disabledMs: 0,
    acknowledgements: 0,
    peerDeliveries: 0,
    authenticationRenewed: 0,
    authenticationRenewalFailed: 0,
  };
  let armed = false;
  let failure = null;
  let participantId = null;
  let joined = 0;
  let disabledSince = null;
  const fail = (code) => {
    failure ??= code;
  };
  const event = (type, detail = {}) => {
    if (events.length >= 128) {
      fail('observation_event_limit');
      return;
    }
    events.push({ type, atMs: Date.now(), ...detail });
  };
  const identity = (value) => typeof value === 'string' && /^[a-zA-Z0-9_-]{1,128}$/.test(value);
  const disabled = () => {
    if (!armed) return;
    const unavailable = document.querySelector('#chat-input')?.disabled !== false;
    if (unavailable && disabledSince === null) {
      disabledSince = performance.now();
      counters.disabledTransitions++;
      event('input_disabled');
    } else if (!unavailable && disabledSince !== null) {
      counters.disabledMs += performance.now() - disabledSince;
      disabledSince = null;
      event('input_enabled');
    }
  };
  const observe = () =>
    new MutationObserver(disabled).observe(document.documentElement, {
      subtree: true,
      attributes: true,
      attributeFilter: ['disabled'],
      childList: true,
    });
  if (document.documentElement) observe();
  else document.addEventListener('DOMContentLoaded', observe, { once: true });
  window.WebSocket = new Proxy(NativeSocket, {
    construct(target, args, newTarget) {
      const socket = Reflect.construct(target, args, newTarget);
      const url = new URL(socket.url);
      if (url.origin !== location.origin.replace(/^http/, 'ws') || url.pathname !== '/ws') {
        fail('unexpected_socket_origin');
        return socket;
      }
      if (sockets.length >= 8) {
        fail('socket_limit');
        return socket;
      }
      sockets.push(new WeakRef(socket));
      const send = socket.send;
      socket.send = function (data) {
        if (armed && typeof data === 'string' && data.length <= 262144) {
          let frame;
          try {
            frame = JSON.parse(data);
          } catch {
            fail('invalid_outgoing_frame');
          }
          if (frame?.type === 'chatMessage') {
            const expected = messages.get(frame.content);
            if (!expected?.own || !identity(frame.clientMessageId) || expected.clientMessageId)
              fail('unexpected_chat_send');
            else expected.clientMessageId = frame.clientMessageId;
          }
        }
        return Reflect.apply(send, this, [data]);
      };
      socket.addEventListener('close', (close) => {
        if (armed) {
          counters.socketCloses++;
          event('socket_close', { code: close.code, clean: close.wasClean });
        }
      });
      socket.addEventListener('message', (messageEvent) => {
        if (typeof messageEvent.data !== 'string' || messageEvent.data.length > 262144) {
          fail('signaling_frame_limit');
          return;
        }
        let frame;
        try {
          frame = JSON.parse(messageEvent.data);
        } catch {
          fail('invalid_signaling_json');
          return;
        }
        if (frame?.type === 'roomJoined') {
          if (!identity(frame.participantId)) {
            fail('invalid_participant_identity');
            return;
          }
          participantId = frame.participantId;
          joined++;
          if (armed) {
            counters.freshJoins++;
            event('fresh_room_join');
          }
        }
        if (!armed) return;
        if (frame?.type === 'authenticationRenewed') {
          counters.authenticationRenewed++;
          event('authentication_renewed');
        } else if (frame?.type === 'authenticationRenewalFailed') {
          counters.authenticationRenewalFailed++;
          event('authentication_renewal_failed');
        } else if (frame?.type === 'reconnectResult') {
          if (frame.success === true) {
            counters.resumeAccepted++;
            event('resume_accepted');
          } else {
            counters.resumeRejected++;
            event('resume_rejected');
          }
        } else if (
          ['error', 'socialError', 'roomClosed', 'roomPasswordRequired', 'lobbyWaiting'].includes(
            frame?.type,
          )
        ) {
          counters.protocolErrors++;
          event('protocol_error');
        } else if (['messageAck', 'chatReceived'].includes(frame?.type)) {
          const entry = frame.type === 'messageAck' ? frame.message : frame;
          const expected = messages.get(entry?.content);
          if (
            !expected ||
            entry.participantId !== expected.senderId ||
            !identity(entry.messageId)
          ) {
            fail('message_identity_or_payload_mismatch');
            return;
          }
          if (expected.id && expected.id !== entry.messageId) fail('message_identity_changed');
          expected.id = entry.messageId;
          if (frame.type === 'messageAck') {
            if (
              !expected.own ||
              expected.ack ||
              !expected.clientMessageId ||
              frame.clientMessageId !== expected.clientMessageId ||
              entry.clientMessageId !== expected.clientMessageId
            )
              fail('incorrect_message_ack');
            else {
              expected.ack = true;
              counters.acknowledgements++;
            }
          } else {
            if (expected.received) fail('duplicate_live_delivery');
            else {
              expected.received = true;
              if (!expected.own) counters.peerDeliveries++;
            }
          }
        }
      });
      return socket;
    },
  });
  const devices = navigator.mediaDevices;
  const denyCapture = () => {
    counters.captureRequests++;
    return Promise.reject(
      new DOMException('Capture is outside this text-only test', 'NotAllowedError'),
    );
  };
  if (devices)
    for (const method of ['getUserMedia', 'getDisplayMedia'])
      if (typeof devices[method] === 'function') devices[method] = denyCapture;
  window.__sessionSoak = {
    arm() {
      if (armed || joined !== 1 || !participantId) throw new Error('Invalid initial room state');
      armed = true;
      disabled();
    },
    cleanup() {
      armed = false;
    },
    expect(value) {
      if (
        !armed ||
        messages.size >= 500 ||
        !/^Session soak [0-9]{4}$/.test(value?.content) ||
        !identity(value.senderId) ||
        typeof value.own !== 'boolean' ||
        messages.has(value.content)
      )
        throw new Error('Invalid expected workload message');
      messages.set(value.content, {
        ...value,
        ack: false,
        received: false,
        id: null,
        clientMessageId: null,
      });
    },
    delivery(content) {
      const value = messages.get(content);
      return value ? { id: value.id, ack: value.ack, received: value.received } : null;
    },
    status() {
      disabled();
      return {
        ...counters,
        participantId,
        joined,
        failure,
        disabledMs:
          counters.disabledMs + (disabledSince === null ? 0 : performance.now() - disabledSince),
        inputDisabled: disabledSince !== null,
        openSockets: sockets.filter((ref) => ref.deref()?.readyState === NativeSocket.OPEN).length,
        events: events.map((entry) => ({ ...entry })),
      };
    },
  };
}

module.exports = {
  configuration,
  sessionExpiry,
  continuityFailure,
  expiryCoverage,
  installSessionObservation,
};
