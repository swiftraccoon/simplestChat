/** Fixed local workloads and bounded, payload-free browser measurements. */
const assert = require('node:assert/strict');

function configuration(env = process.env) {
  assert.equal(env.UI_STRESS_E2E, '1', 'Set UI_STRESS_E2E=1');
  assert.equal(env.DISPOSABLE_TEST_DATABASE, '1', 'Use an owned disposable database');
  assert.match(env.TEST_METRICS_TOKEN || '', /^[a-f0-9]{64}$/, 'Use the owned test-server helper');
  assert.match(env.TEST_SERVER_PID || '', /^[1-9][0-9]*$/, 'Use the owned test-server helper');
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
    'UI stress requires a credential-free loopback origin',
  );
  assert.notEqual(env.NODE_TLS_REJECT_UNAUTHORIZED, '0', 'TLS verification must remain enabled');
  const profile = env.UI_STRESS_PROFILE ?? 'full';
  assert.ok(['full', 'smoke'].includes(profile), 'UI_STRESS_PROFILE must be full or smoke');
  return {
    base: base.origin,
    serverPid,
    profile,
    observers: 2,
    guests: profile === 'full' ? 38 : 4,
    joinIntervalMs: 6300,
    messages: profile === 'full' ? 960 : 48,
    messageIntervalMs: profile === 'full' ? 125 : 250,
    reconnectAfterMessages: profile === 'full' ? 560 : 28,
    heldMessages: profile === 'full' ? 24 : 8,
    retention: 300,
    deadlineMs: profile === 'full' ? 600000 : 180000,
  };
}

/** Timings are informational distributions, never a synthetic INP or leak score. */
function distribution(values) {
  assert.ok(Array.isArray(values) && values.length <= 4096, 'Timing sample limit exceeded');
  assert.ok(
    values.every((value) => Number.isFinite(value) && value >= 0),
    'Invalid timing sample',
  );
  const ordered = [...values].sort((left, right) => left - right);
  const percentile = (fraction) =>
    ordered.length ? ordered[Math.ceil(ordered.length * fraction) - 1] : null;
  return {
    count: ordered.length,
    totalMs: ordered.reduce((total, value) => total + value, 0),
    p50Ms: percentile(0.5),
    p95Ms: percentile(0.95),
    maxMs: ordered.at(-1) ?? null,
  };
}

function cpuDelta(previous, current) {
  const fields = ['TaskDuration', 'ScriptDuration', 'LayoutDuration', 'RecalcStyleDuration'];
  return Object.fromEntries(
    fields.map((field) => {
      assert.ok(
        Number.isFinite(previous[field]) &&
          Number.isFinite(current[field]) &&
          current[field] >= previous[field],
        'Missing or reset Chromium performance counter',
      );
      return [`${field}Seconds`, current[field] - previous[field]];
    }),
  );
}

function retainedMessages(actual, expected, retention = 300) {
  assert.ok(Array.isArray(actual) && Array.isArray(expected) && expected.length <= 2048);
  assert.ok(Number.isSafeInteger(retention) && retention > 0 && retention <= 300);
  const last = expected.slice(-retention);
  assert.equal(actual.length, last.length, 'Incorrect retained message count');
  assert.equal(
    new Set(actual.map((entry) => entry.id)).size,
    actual.length,
    'Duplicate retained message',
  );
  for (let index = 0; index < last.length; index++) {
    assert.ok(
      actual[index].id === last[index].id,
      'Incorrect retained message identity or ordering',
    );
    assert.ok(actual[index].content === last[index].content, 'Incorrect retained message content');
  }
}

const SERVER_COUNTERS = Object.freeze({
  errors: 'simplestchat_errors_total',
  sendFailures: 'simplestchat_message_send_failed_total',
  queueFull: 'simplestchat_outbound_queue_full_total',
  queueClosed: 'simplestchat_outbound_queue_closed_total',
  producersCreated: 'simplestchat_producers_created_total',
  consumersCreated: 'simplestchat_consumers_created_total',
});

function serverCounters(text) {
  assert.ok(typeof text === 'string' && text.length <= 131072, 'Invalid server counter snapshot');
  return Object.fromEntries(
    Object.entries(SERVER_COUNTERS).map(([key, name]) => {
      const lines = text
        .split('\n')
        .filter((line) => new RegExp(`^${name}(?:[ \\t{]|$)`).test(line));
      assert.equal(lines.length, 1, 'Missing or duplicate server counter');
      const match = new RegExp(`^${name} ([0-9]+)$`).exec(lines[0]);
      assert.ok(match && Number.isSafeInteger(Number(match[1])), 'Invalid server counter');
      return [key, Number(match[1])];
    }),
  );
}

function serverCounterDelta(before, after) {
  return Object.fromEntries(
    Object.keys(SERVER_COUNTERS).map((key) => {
      assert.ok(
        Number.isSafeInteger(before[key]) &&
          before[key] >= 0 &&
          Number.isSafeInteger(after[key]) &&
          after[key] >= before[key],
        'Missing or reset server counter',
      );
      return [key, after[key] - before[key]];
    }),
  );
}

function cleanServerDelta(delta, closedQueueAllowance = 0) {
  assert.ok(
    Number.isSafeInteger(closedQueueAllowance) &&
      closedQueueAllowance >= 0 &&
      closedQueueAllowance <= 2048,
  );
  for (const key of Object.keys(SERVER_COUNTERS)) {
    assert.ok(
      Number.isSafeInteger(delta[key]) &&
        delta[key] >= 0 &&
        delta[key] <= (key === 'queueClosed' ? closedQueueAllowance : 0),
      'Unexpected server error, queue failure, or media creation',
    );
  }
}

/** Validate the fixed payload and expected sender without retaining message text. */
function workloadMessage(entry, workload) {
  if (typeof entry?.clientMessageId !== 'string') return false;
  const prefix = `${workload.runId}-`;
  if (!entry.clientMessageId.startsWith(prefix)) return false;
  const sequence = entry.clientMessageId.slice(prefix.length);
  if (!/^[1-9][0-9]{0,3}$/.test(sequence)) return false;
  const index = Number(sequence);
  return (
    index <= workload.messages &&
    entry.content === `UI stress message ${sequence.padStart(4, '0')}` &&
    entry.participantId === workload.senders[(index - 1) % workload.senders.length]
  );
}

/**
 * Installed before application scripts, on this runner's pages only. Native
 * signaling/media transports remain real. Capture calls are counted and denied
 * before reaching a device; any request fails the workload. No frames, content,
 * credentials, or native resources are retained in measurement records.
 */
function installUiStressObservation() {
  if ('__uiStress' in window) throw new Error('UI stress observer already installed');
  const NativeSocket = window.WebSocket;
  const NativePeer = window.RTCPeerConnection;
  const sockets = [];
  const peers = [];
  const seen = new Set();
  const live = new Set();
  const rendered = new Set();
  const framed = new Set();
  const pending = new Map();
  const replays = new Set();
  const timings = {
    arrivalToDomMs: [],
    arrivalToFrameMs: [],
    snapshotToPostDispatchDomMs: [],
    snapshotToFrameMs: [],
    trustedInputToFrameMs: [],
    longTaskMs: [],
  };
  const counters = {
    captureRequests: 0,
    joined: 0,
    reconnectSuccess: 0,
    reconnectFailure: 0,
    snapshotCount: 0,
    replayRecoveredMessages: 0,
    liveDuplicates: 0,
    protocolErrors: 0,
    unexpectedSocketCloses: 0,
    payloadValidatedMessages: 0,
  };
  let failure = null;
  let pendingFrames = 0;
  let guardConnections = false;
  let workload = null;
  const fail = (code) => {
    failure ??= code;
  };
  const push = (name, value) => {
    if (timings[name].length >= 4096) fail('timing_buffer_full');
    else timings[name].push(Math.max(0, value));
  };
  const identity = (value) => typeof value === 'string' && value.length > 0 && value.length <= 128;
  const remember = (entry, at, replay) => {
    let sequenceNumber = 0;
    if (!identity(entry?.messageId)) {
      fail('invalid_message_identity');
      return;
    }
    if (workload) {
      const prefix = `${workload.runId}-`;
      const sequence =
        typeof entry.clientMessageId === 'string' && entry.clientMessageId.startsWith(prefix)
          ? entry.clientMessageId.slice(prefix.length)
          : '';
      const index = /^[1-9][0-9]{0,3}$/.test(sequence) ? Number(sequence) : 0;
      if (
        !index ||
        index > workload.messages ||
        entry.content !== `UI stress message ${sequence.padStart(4, '0')}` ||
        entry.participantId !== workload.senders[(index - 1) % workload.senders.length]
      ) {
        fail('incorrect_message_payload_or_sender');
        return;
      }
      sequenceNumber = index;
    }
    const id = entry.messageId;
    if (!replay) {
      if (live.has(id)) counters.liveDuplicates++;
      if (live.size >= 2048) {
        fail('message_buffer_full');
        return;
      }
      live.add(id);
    }
    if (seen.has(id)) return;
    if (seen.size >= 2048) {
      fail('message_buffer_full');
      return;
    }
    seen.add(id);
    if (workload) counters.payloadValidatedMessages++;
    if (replay) counters.replayRecoveredMessages++;
    pending.set(id, { at, replay, sequenceNumber });
  };
  const rows = () => [
    ...document.querySelectorAll('#chat-messages .chat-msg:not(.system)[data-message-id]'),
  ];
  const reconcile = () => {
    if (!pending.size) return;
    const now = performance.now();
    const ready = [];
    for (const row of rows()) {
      const id = row.dataset.messageId;
      const item = pending.get(id);
      if (!item) continue;
      if (
        workload &&
        row.querySelector('.msg-text')?.textContent !==
          `UI stress message ${String(item.sequenceNumber).padStart(4, '0')}`
      ) {
        fail('incorrect_rendered_message_content');
        pending.delete(id);
        continue;
      }
      pending.delete(id);
      rendered.add(id);
      ready.push([id, item]);
      if (!item.replay) push('arrivalToDomMs', now - item.at);
    }
    if (!ready.length) return;
    pendingFrames++;
    requestAnimationFrame(() => {
      pendingFrames--;
      const at = performance.now();
      for (const [id, item] of ready) {
        framed.add(id);
        if (!item.replay) push('arrivalToFrameMs', at - item.at);
      }
    });
  };
  new MutationObserver(reconcile).observe(document, { childList: true, subtree: true });

  function snapshot(message, at) {
    const entries = message.data?.messages;
    if (!Array.isArray(entries) || entries.length > 300 || replays.size >= 8) {
      fail('invalid_or_excess_snapshot');
      return;
    }
    counters.snapshotCount++;
    if (counters.snapshotCount > 8) {
      fail('snapshot_limit');
      return;
    }
    const ids = entries
      .filter((entry) => !seen.has(entry?.messageId))
      .map((entry) => entry.messageId);
    for (const entry of entries) remember(entry, at, true);
    const replay = { ids, at };
    replays.add(replay);
    // A timer runs after the whole native message dispatch, unlike a microtask
    // from our first listener. This includes application replay plus queue delay.
    const finish = () => {
      if (performance.now() - at > 30000) {
        replays.delete(replay);
        fail('snapshot_render_deadline');
        return;
      }
      reconcile();
      if (!ids.every((id) => rendered.has(id))) {
        setTimeout(finish, 16);
        return;
      }
      replays.delete(replay);
      push('snapshotToPostDispatchDomMs', performance.now() - at);
      pendingFrames++;
      requestAnimationFrame(() => {
        pendingFrames--;
        push('snapshotToFrameMs', performance.now() - at);
      });
    };
    setTimeout(finish, 0);
  }

  const WrappedSocket = new Proxy(NativeSocket, {
    construct(target, args, newTarget) {
      const socket = Reflect.construct(target, args, newTarget);
      const url = new URL(socket.url);
      if (
        url.origin !== window.location.origin.replace(/^http/, 'ws') ||
        url.pathname !== '/ws' ||
        url.search ||
        url.hash ||
        url.username ||
        url.password
      )
        return socket;
      if (sockets.length >= 8) {
        fail('socket_limit');
        return socket;
      }
      const record = { reference: new WeakRef(socket), closed: false, plannedClose: false };
      sockets.push(record);
      socket.addEventListener('close', () => {
        record.closed = true;
        if (guardConnections && !record.plannedClose) counters.unexpectedSocketCloses++;
      });
      socket.addEventListener('message', (event) => {
        const at = performance.now();
        if (typeof event.data !== 'string' || event.data.length > 262144) {
          fail('signaling_frame_limit');
          return;
        }
        let message;
        try {
          message = JSON.parse(event.data);
        } catch {
          fail('invalid_signaling_json');
          return;
        }
        if (message.type === 'roomJoined') counters.joined++;
        else if (message.type === 'reconnectResult') {
          if (message.success === true) counters.reconnectSuccess++;
          else counters.reconnectFailure++;
        } else if (
          ['error', 'socialError', 'roomClosed', 'roomPasswordRequired', 'lobbyWaiting'].includes(
            message.type,
          )
        ) {
          counters.protocolErrors++;
        } else if (message.type === 'chatReceived') remember(message, at, false);
        else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot')
          snapshot(message, at);
      });
      return socket;
    },
  });
  const WrappedPeer = new Proxy(NativePeer, {
    construct(target, args, newTarget) {
      const peer = Reflect.construct(target, args, newTarget);
      if (peers.length >= 16) {
        fail('peer_limit');
        return peer;
      }
      const record = { reference: new WeakRef(peer), closed: false };
      peers.push(record);
      const closed = () => {
        if (peer.connectionState === 'closed' || peer.signalingState === 'closed')
          record.closed = true;
      };
      peer.addEventListener('connectionstatechange', closed);
      peer.addEventListener('signalingstatechange', closed);
      return peer;
    },
  });
  window.WebSocket = WrappedSocket;
  window.RTCPeerConnection = WrappedPeer;
  const devices = navigator.mediaDevices;
  const deniedCapture = () => {
    counters.captureRequests++;
    return Promise.reject(
      new DOMException('Capture disabled by text-only test', 'NotAllowedError'),
    );
  };
  if (!devices) fail('media_devices_unavailable');
  else {
    devices.getUserMedia = deniedCapture;
    devices.getDisplayMedia = deniedCapture;
  }
  document.addEventListener(
    'input',
    (event) => {
      if (!event.isTrusted || event.target?.id !== 'chat-input') return;
      const at = performance.now();
      pendingFrames++;
      requestAnimationFrame(() => {
        pendingFrames--;
        push('trustedInputToFrameMs', performance.now() - at);
      });
    },
    true,
  );
  new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) push('longTaskMs', entry.duration);
  }).observe({ type: 'longtask', buffered: true });

  const open = (records, kind) =>
    records.flatMap((record) => {
      if (record.closed) return [];
      const object = record.reference.deref();
      if (!object) {
        fail('unobserved_resource_shutdown');
        return [];
      }
      if (
        kind === 'socket'
          ? object.readyState === 3
          : object.connectionState === 'closed' || object.signalingState === 'closed'
      ) {
        record.closed = true;
        return [];
      }
      return [object];
    });
  const status = () => {
    const openSockets = open(sockets, 'socket');
    const openPeers = open(peers, 'peer');
    if (
      window.WebSocket !== WrappedSocket ||
      window.RTCPeerConnection !== WrappedPeer ||
      window.__uiStress !== api ||
      navigator.mediaDevices !== devices ||
      devices?.getUserMedia !== deniedCapture ||
      devices?.getDisplayMedia !== deniedCapture
    )
      fail('observer_replaced');
    return {
      failure,
      ...counters,
      openSockets: openSockets.length,
      openPeers: openPeers.length,
      pendingMessages: pending.size,
      pendingFrames,
      pendingReplays: replays.size,
      seenIds: [...seen],
      renderedIds: [...rendered],
      framedIds: [...framed],
      attachedMediaElements: [...document.querySelectorAll('audio, video')].filter(
        (element) => element.srcObject,
      ).length,
    };
  };
  const api = {
    status,
    expectWorkload(value) {
      if (
        workload ||
        !/^ui-stress-[a-f0-9-]{36}$/.test(value?.runId) ||
        !Number.isSafeInteger(value.messages) ||
        value.messages < 1 ||
        value.messages > 960 ||
        !Array.isArray(value.senders) ||
        value.senders.length < 4 ||
        value.senders.length > 8 ||
        !value.senders.every(identity) ||
        new Set(value.senders).size !== value.senders.length
      ) {
        throw new Error('Invalid or repeated fixed workload');
      }
      workload = { runId: value.runId, messages: value.messages, senders: [...value.senders] };
    },
    guardConnections() {
      guardConnections = true;
    },
    beginCleanup() {
      guardConnections = false;
    },
    sample() {
      const sample = {
        ...status(),
        timings: Object.fromEntries(
          Object.entries(timings).map(([key, values]) => [key, [...values]]),
        ),
      };
      for (const values of Object.values(timings)) values.length = 0;
      return sample;
    },
    closeCurrent() {
      if (status().failure) throw new Error('UI stress observation failed');
      const current = open(sockets, 'socket').filter(
        (socket) => socket.readyState === NativeSocket.OPEN,
      );
      if (current.length !== 1) throw new Error('Expected one owned open signaling socket');
      sockets.find((record) => record.reference.deref() === current[0]).plannedClose = true;
      Reflect.apply(NativeSocket.prototype.close, current[0], [4000, 'Owned UI stress reconnect']);
    },
    leave() {
      for (const socket of open(sockets, 'socket')) {
        if (socket.readyState === NativeSocket.OPEN)
          Reflect.apply(NativeSocket.prototype.send, socket, [
            JSON.stringify({ type: 'leaveRoom' }),
          ]);
      }
    },
  };
  window.__uiStress = api;
}

module.exports = {
  configuration,
  distribution,
  cpuDelta,
  retainedMessages,
  serverCounters,
  serverCounterDelta,
  cleanServerDelta,
  workloadMessage,
  installUiStressObservation,
};
