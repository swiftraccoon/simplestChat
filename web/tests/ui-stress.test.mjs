import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const require = createRequire(import.meta.url);
const { cleanupWithin, Guest } = require('../e2e/ui-stress.cjs');
const {
  configuration,
  distribution,
  cpuDelta,
  retainedMessages,
  serverCounters,
  serverCounterDelta,
  cleanServerDelta,
  workloadMessage,
  installUiStressObservation,
} = require('../e2e/ui-stress-metrics.cjs');
const owned = {
  UI_STRESS_E2E: '1',
  DISPOSABLE_TEST_DATABASE: '1',
  TEST_METRICS_TOKEN: 'a'.repeat(64),
  TEST_SERVER_PID: '12345',
};

test('UI stress requires owned local services and exact bounded profiles', () => {
  const full = configuration(owned);
  assert.equal(full.guests + full.observers, 40);
  assert.equal(full.messages, 960);
  assert.equal(full.retention, 300);
  assert.equal(full.messageIntervalMs, 125);
  assert.equal(full.joinIntervalMs, 6300);
  assert.ok(full.messages * full.messageIntervalMs >= 120000);
  const smoke = configuration({ ...owned, UI_STRESS_PROFILE: 'smoke' });
  assert.equal(smoke.guests + smoke.observers, 6);
  assert.equal(smoke.messages, 48);
  assert.equal(smoke.messageIntervalMs, 250);
  for (const key of Object.keys(owned)) {
    const env = { ...owned };
    delete env[key];
    assert.throws(() => configuration(env));
    assert.throws(() => configuration({ ...owned, [key]: '' }));
  }
  for (const profile of ['', 'FULL', 'full ', '10000', 'production']) {
    assert.throws(() => configuration({ ...owned, UI_STRESS_PROFILE: profile }));
  }
  for (const pid of ['0', '1', '-2', '01', '1e3', '2147483648', '9'.repeat(100)]) {
    assert.throws(() => configuration({ ...owned, TEST_SERVER_PID: pid }));
  }
  assert.throws(() => configuration({ ...owned, TEST_METRICS_TOKEN: 'secret' }));
  assert.throws(() => configuration({ ...owned, NODE_TLS_REJECT_UNAUTHORIZED: '0' }));
});

test('UI stress rejects public, credentialed, path-qualified or ambiguous origins', () => {
  for (const origin of ['http://127.0.0.1:3119', 'https://localhost:3119', 'http://[::1]:3119']) {
    assert.equal(configuration({ ...owned, BASE_URL: origin }).base, origin);
  }
  for (const origin of [
    'https://research.clinic',
    'http://192.168.1.1',
    'http://0.0.0.0',
    'http://localhost.example.test',
    'http://user:secret@localhost',
    'http://localhost/path',
    'http://localhost?token=secret',
    'http://localhost/#fragment',
    'ws://localhost:3119',
    'file://localhost/',
    'invalid-url',
  ])
    assert.throws(() => configuration({ ...owned, BASE_URL: origin }), undefined, origin);
});

test('fixed join and send schedules remain below unchanged per-IP/room/sender limits', () => {
  for (const profile of ['full', 'smoke']) {
    const config = configuration({ ...owned, UI_STRESS_PROFILE: profile });
    const joins = Array.from(
      { length: config.guests + 2 },
      (_, index) => index * config.joinIntervalMs,
    );
    for (const at of joins)
      assert.ok(joins.filter((time) => time >= at && time < at + 60000).length <= 10);
    const sends = Array.from({ length: config.messages }, (_, index) => ({
      at: index * config.messageIntervalMs,
      sender: index % Math.min(8, config.guests),
    }));
    for (const { at, sender } of sends) {
      const window = sends.filter((item) => item.at >= at && item.at < at + 1000);
      assert.ok(window.length <= 8);
      assert.ok(window.filter((item) => item.sender === sender).length <= 2);
    }
    // Full reconnect happens >60s after the last join; smoke totals only seven admissions.
    assert.ok(
      config.reconnectAfterMessages * config.messageIntervalMs > 60000 || config.guests + 3 <= 10,
    );
    assert.ok(config.reconnectAfterMessages + config.heldMessages + 4 < config.messages);
  }
});

test('retention checks exact latest identities, order and content, including rollover', () => {
  const expected = Array.from({ length: 960 }, (_, index) => ({
    id: `message-${index}`,
    content: `text-${index}`,
  }));
  const retained = expected.slice(-300);
  retainedMessages(retained, expected);
  retainedMessages(expected.slice(0, 48), expected.slice(0, 48));
  assert.throws(() => retainedMessages(retained.slice(1), expected));
  assert.throws(() => retainedMessages([...retained.slice(1), retained[1]], expected));
  assert.throws(() => retainedMessages([...retained].reverse(), expected));
  assert.throws(() =>
    retainedMessages([{ ...retained[0], content: 'wrong' }, ...retained.slice(1)], expected),
  );
  assert.throws(() => retainedMessages(retained, expected, 500));
});

test('timing summaries preserve empty coverage and reject invalid or unbounded samples', () => {
  assert.deepEqual(distribution([]), {
    count: 0,
    totalMs: 0,
    p50Ms: null,
    p95Ms: null,
    maxMs: null,
  });
  assert.deepEqual(distribution([4, 1, 2, 3]), {
    count: 4,
    totalMs: 10,
    p50Ms: 2,
    p95Ms: 4,
    maxMs: 4,
  });
  for (const values of [[-1], [NaN], [Infinity], ['1'], new Array(4097).fill(0)])
    assert.throws(() => distribution(values));
  const before = {
    TaskDuration: 2,
    ScriptDuration: 1,
    LayoutDuration: 0.5,
    RecalcStyleDuration: 0.25,
  };
  const after = Object.fromEntries(
    Object.entries(before).map(([name, value]) => [name, value + 1]),
  );
  assert.deepEqual(Object.values(cpuDelta(before, after)), [1, 1, 1, 1]);
  assert.throws(() => cpuDelta(before, { ...after, TaskDuration: 0 }));
  assert.throws(() => cpuDelta(before, { TaskDuration: 3 }));
});

const counterNames = [
  'errors',
  'message_send_failed',
  'outbound_queue_full',
  'outbound_queue_closed',
  'producers_created',
  'consumers_created',
];
const metricText = (values = [0, 0, 0, 0, 0, 0]) =>
  counterNames.map((name, index) => `simplestchat_${name}_total ${values[index]}`).join('\n');

test('server counter evidence is complete, bounded, unlabelled and monotonic', () => {
  const baseline = serverCounters(metricText());
  assert.deepEqual(baseline, {
    errors: 0,
    sendFailures: 0,
    queueFull: 0,
    queueClosed: 0,
    producersCreated: 0,
    consumersCreated: 0,
  });
  for (const invalid of [
    '',
    metricText().replace('errors_total 0', 'errors_total NaN'),
    metricText().replace('errors_total 0', 'errors_total{client="secret"} 0'),
    `${metricText()}\nsimplestchat_errors_total 0`,
    'x'.repeat(131073),
  ]) {
    assert.throws(() => serverCounters(invalid));
  }
  const after = serverCounters(metricText([0, 0, 0, 25, 0, 0]));
  const delta = serverCounterDelta(baseline, after);
  assert.equal(delta.queueClosed, 25);
  assert.throws(() => serverCounterDelta(after, baseline));
  assert.throws(() => serverCounterDelta({}, baseline));
  cleanServerDelta(baseline);
  assert.throws(() => cleanServerDelta(delta));
  assert.throws(() => cleanServerDelta(delta, 24));
  cleanServerDelta(delta, 25);
  for (const key of [
    'errors',
    'sendFailures',
    'queueFull',
    'producersCreated',
    'consumersCreated',
  ]) {
    assert.throws(() => cleanServerDelta({ ...delta, [key]: 1 }, 25), undefined, key);
  }
  assert.throws(() => cleanServerDelta(delta, Infinity));
});

const fixedWorkload = {
  runId: 'ui-stress-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
  messages: 960,
  senders: ['sender-0', 'sender-1', 'sender-2', 'sender-3'],
};
const workloadEntry = (index) => ({
  messageId: `id-${index}`,
  clientMessageId: `${fixedWorkload.runId}-${index}`,
  content: `UI stress message ${String(index).padStart(4, '0')}`,
  participantId: fixedWorkload.senders[(index - 1) % fixedWorkload.senders.length],
});

test('all workload frames validate exact payload, sequence and expected author', () => {
  for (const index of [1, 4, 300, 660, 960])
    assert.ok(workloadMessage(workloadEntry(index), fixedWorkload));
  for (const entry of [
    null,
    {},
    workloadEntry(0),
    workloadEntry(961),
    { ...workloadEntry(1), content: 'corrupted' },
    { ...workloadEntry(1), participantId: 'wrong-sender' },
    { ...workloadEntry(1), clientMessageId: `${fixedWorkload.runId}-01` },
    { ...workloadEntry(1), clientMessageId: 'other-run-1' },
  ]) {
    assert.equal(workloadMessage(entry, fixedWorkload), false);
  }
});

test('protocol guests reject duplicate acknowledgments or broadcasts instead of collapsing them', () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'WebSocket');
  class Socket extends EventTarget {
    static OPEN = 1;
    readyState = 1;
    receive(message) {
      this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(message) }));
    }
  }
  Object.defineProperty(globalThis, 'WebSocket', {
    configurable: true,
    writable: true,
    value: Socket,
  });
  try {
    const entry = workloadEntry(1);
    const frames = {
      chatReceived: { type: 'chatReceived', ...entry },
      messageAck: { type: 'messageAck', clientMessageId: entry.clientMessageId, message: entry },
    };
    for (const types of [
      ['chatReceived', 'chatReceived'],
      ['messageAck', 'messageAck'],
      ['messageAck', 'chatReceived'],
    ]) {
      const guest = new Guest('ws://127.0.0.1:3119/ws');
      guest.workload = fixedWorkload;
      guest.socket.receive(frames[types[0]]);
      assert.equal(guest.failure, null);
      assert.equal(guest.received.size, 1);
      guest.socket.receive(frames[types[1]]);
      assert.equal(guest.failure?.code, 'guest_duplicate_message');
      assert.equal(guest.received.size, 1);
    }
  } finally {
    if (descriptor) Object.defineProperty(globalThis, 'WebSocket', descriptor);
    else delete globalThis.WebSocket;
  }
});

/** Deterministic event fixture: no browser, network, physical media or live timers. */
function observerFixture() {
  let now = 0;
  let mutation;
  let performanceEntries;
  let physicalCaptures = 0;
  let rows = [];
  const frames = [];
  const timers = [];
  const listeners = new Map();
  class Socket extends EventTarget {
    static OPEN = 1;
    constructor(url) {
      super();
      this.url = url;
      this.readyState = 1;
      this.sent = [];
    }
    send(value) {
      this.sent.push(value);
    }
    close() {
      this.readyState = 3;
      this.dispatchEvent(new Event('close'));
    }
    receive(value) {
      this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(value) }));
    }
  }
  class Peer extends EventTarget {
    connectionState = 'new';
    signalingState = 'stable';
    close() {
      this.connectionState = 'closed';
      this.dispatchEvent(new Event('connectionstatechange'));
    }
  }
  const window = {
    WebSocket: Socket,
    RTCPeerConnection: Peer,
    location: { origin: 'http://127.0.0.1:3119' },
  };
  const document = {
    querySelectorAll: (selector) => (selector.startsWith('#chat-messages') ? rows : []),
    addEventListener: (kind, callback) => listeners.set(kind, callback),
  };
  const navigator = {
    mediaDevices: {
      getUserMedia: () => {
        physicalCaptures++;
      },
      getDisplayMedia: () => {
        physicalCaptures++;
      },
    },
  };
  const context = {
    window,
    document,
    navigator,
    URL,
    WeakRef,
    DOMException,
    performance: { now: () => now },
    requestAnimationFrame: (callback) => frames.push(callback),
    setTimeout: (callback) => timers.push(callback),
    MutationObserver: class {
      constructor(callback) {
        mutation = callback;
      }
      observe() {}
    },
    PerformanceObserver: class {
      constructor(callback) {
        performanceEntries = callback;
      }
      observe() {}
    },
  };
  runInNewContext(`(${installUiStressObservation.toString()})()`, context);
  return {
    window,
    navigator,
    api: window.__uiStress,
    captured: () => physicalCaptures,
    socket: () => new window.WebSocket('http://127.0.0.1:3119/ws'.replace(/^http/, 'ws')),
    advance: (milliseconds) => {
      now += milliseconds;
    },
    rows: (ids, content = '') => {
      rows = ids.map((id) => ({
        dataset: { messageId: id },
        querySelector: () => ({ textContent: content }),
      }));
      mutation();
    },
    frames: () => {
      for (const callback of frames.splice(0)) callback();
    },
    timers: () => {
      for (const callback of timers.splice(0)) callback();
    },
    input: (trusted) =>
      listeners.get('input')({ isTrusted: trusted, target: { id: 'chat-input' } }),
    longTasks: (durations) =>
      performanceEntries({ getEntries: () => durations.map((duration) => ({ duration })) }),
  };
}

test('browser observer records arrival/DOM/frame separately without retaining content', () => {
  const fixture = observerFixture();
  const socket = fixture.socket();
  socket.receive({ type: 'chatReceived', messageId: 'first', content: 'sensitive-sentinel' });
  fixture.advance(3);
  fixture.rows(['first']);
  fixture.advance(7);
  fixture.frames();
  const sample = fixture.api.sample();
  assert.deepEqual([...sample.timings.arrivalToDomMs], [3]);
  assert.deepEqual([...sample.timings.arrivalToFrameMs], [10]);
  assert.deepEqual([...sample.framedIds], ['first']);
  assert.equal(JSON.stringify(sample).includes('sensitive-sentinel'), false);
  assert.equal(fixture.api.sample().timings.arrivalToDomMs.length, 0);
  socket.receive({ type: 'chatReceived', messageId: 'first' });
  assert.equal(fixture.api.status().liveDuplicates, 1);
});

test('300-entry replay above64KiB is observed after dispatch, retaining only identities', () => {
  const fixture = observerFixture();
  const socket = fixture.socket();
  const entries = Array.from({ length: 300 }, (_, index) => ({
    messageId: `id-${index}`,
    content: 'x'.repeat(300),
  }));
  const message = {
    type: 'socialResponse',
    action: 'getRoomSnapshot',
    data: { messages: entries },
  };
  assert.ok(JSON.stringify(message).length > 65536);
  socket.receive(message);
  fixture.advance(25);
  fixture.rows(entries.map((entry) => entry.messageId));
  assert.equal(
    fixture.api.status().pendingReplays,
    1,
    'a mutation alone is not a post-dispatch timing',
  );
  fixture.advance(5);
  fixture.timers();
  fixture.advance(10);
  fixture.frames();
  const sample = fixture.api.sample();
  assert.equal(sample.failure, null);
  assert.equal(sample.replayRecoveredMessages, 300);
  assert.deepEqual([...sample.timings.snapshotToPostDispatchDomMs], [30]);
  assert.deepEqual([...sample.timings.snapshotToFrameMs], [40]);
  assert.equal(sample.timings.arrivalToDomMs.length, 0, 'replay and live timing are separate');
  socket.receive(message);
  fixture.timers();
  fixture.frames();
  assert.equal(
    fixture.api.status().replayRecoveredMessages,
    300,
    'duplicate replay is not new delivery',
  );
});

test('browser capture is denied before device access and only trusted input is measured', async () => {
  const fixture = observerFixture();
  await assert.rejects(fixture.navigator.mediaDevices.getUserMedia({ audio: true }), {
    name: 'NotAllowedError',
  });
  await assert.rejects(fixture.navigator.mediaDevices.getDisplayMedia({ video: true }), {
    name: 'NotAllowedError',
  });
  assert.equal(fixture.captured(), 0);
  assert.equal(fixture.api.status().captureRequests, 2);
  fixture.input(false);
  fixture.input(true);
  fixture.advance(12);
  fixture.frames();
  assert.deepEqual([...fixture.api.sample().timings.trustedInputToFrameMs], [12]);
});

test('browser validates wire and newly rendered content before history can evict it', () => {
  let fixture = observerFixture();
  fixture.api.expectWorkload(fixedWorkload);
  let socket = fixture.socket();
  socket.receive({ type: 'chatReceived', ...workloadEntry(1) });
  fixture.rows(['id-1'], 'UI stress message 0001');
  fixture.frames();
  assert.equal(fixture.api.status().failure, null);
  assert.equal(fixture.api.status().payloadValidatedMessages, 1);
  socket.receive({ type: 'chatReceived', ...workloadEntry(2) });
  fixture.rows(['id-2'], 'wrong rendering');
  assert.equal(fixture.api.status().failure, 'incorrect_rendered_message_content');
  fixture = observerFixture();
  fixture.api.expectWorkload(fixedWorkload);
  socket = fixture.socket();
  socket.receive({ type: 'chatReceived', ...workloadEntry(1), content: 'wrong wire payload' });
  assert.equal(fixture.api.status().failure, 'incorrect_message_payload_or_sender');
  assert.equal(fixture.api.status().seenIds.length, 0);
  assert.throws(() => fixture.api.expectWorkload(fixedWorkload));
});

test('missing snapshot DOM stops its own observation at the bounded deadline', () => {
  const fixture = observerFixture();
  fixture.socket().receive({
    type: 'socialResponse',
    action: 'getRoomSnapshot',
    data: { messages: [{ messageId: 'missing' }] },
  });
  fixture.timers();
  assert.equal(fixture.api.status().pendingReplays, 1);
  fixture.advance(30001);
  fixture.timers();
  assert.equal(fixture.api.status().pendingReplays, 0);
  assert.equal(fixture.api.status().failure, 'snapshot_render_deadline');
});

test('unplanned disconnects cannot pass as recovery; owned closure is narrowly allowed', () => {
  const fixture = observerFixture();
  const initial = fixture.socket();
  initial.close(); // Authentication replacement precedes the explicitly armed gate.
  const ownedSocket = fixture.socket();
  fixture.api.guardConnections();
  fixture.api.closeCurrent();
  assert.equal(ownedSocket.readyState, 3);
  assert.equal(fixture.api.status().unexpectedSocketCloses, 0);
  const unexpected = fixture.socket();
  unexpected.close();
  assert.equal(fixture.api.status().unexpectedSocketCloses, 1);
  const cleanup = fixture.socket();
  fixture.api.beginCleanup();
  cleanup.close();
  assert.equal(fixture.api.status().unexpectedSocketCloses, 1);
});

test('observation bounds and instrumentation replacement fail closed', () => {
  let fixture = observerFixture();
  fixture.longTasks(new Array(4097).fill(60));
  assert.equal(fixture.api.status().failure, 'timing_buffer_full');
  fixture = observerFixture();
  const socket = fixture.socket();
  socket.receive({
    type: 'socialResponse',
    action: 'getRoomSnapshot',
    data: { messages: new Array(301).fill({ messageId: 'x' }) },
  });
  assert.equal(fixture.api.status().failure, 'invalid_or_excess_snapshot');
  fixture = observerFixture();
  fixture.navigator.mediaDevices.getUserMedia = () => {};
  assert.equal(fixture.api.status().failure, 'observer_replaced');
  fixture = observerFixture();
  const sockets = Array.from({ length: 9 }, () => fixture.socket());
  assert.equal(sockets.length, 9);
  assert.equal(fixture.api.status().failure, 'socket_limit');
});

test('runner is opt-in, does not grant capture, and retains original cleanup evidence', () => {
  const source = readFileSync(new URL('../e2e/ui-stress.cjs', import.meta.url), 'utf8');
  assert.ok(source.includes('permissions: []'));
  assert.ok(source.includes("require('./lifecycle-cleanup.cjs')"));
  assert.ok(source.includes('closeOwnedBrowser(browserServer, report, save)'));
  assert.ok(source.includes("require('./performance-report.cjs')"));
  assert.ok(source.includes('const config = configuration(env)'));
  assert.ok(source.includes('unexpected_browser_disconnect'));
  assert.equal(source.includes('ignoreHTTPSErrors'), false);
  assert.equal(source.includes('use-fake'), false);
  assert.equal(source.includes('newContext({ storageState'), false);
  assert.ok(source.includes("code: 'finalization_deadline'"));
  assert.ok(source.includes('}, 65000)'));
  assert.match(source, /cleanupWithin\(\(\) =>\s*client\.page\.evaluate\(/);
});

test('cleanup bounds a stuck page operation without hiding the original failure', async () => {
  assert.equal(await cleanupWithin(() => Promise.resolve('closed'), 100), 'closed');
  const original = new Error('original owned operation failure');
  await assert.rejects(
    cleanupWithin(() => {
      throw original;
    }, 100),
    (error) => error === original,
  );
  await assert.rejects(
    cleanupWithin(() => new Promise(() => {}), 5),
    { code: 'cleanup_action_timeout' },
  );
});
