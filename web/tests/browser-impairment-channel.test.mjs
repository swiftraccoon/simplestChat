import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { Duplex } from 'node:stream';
import test from 'node:test';

const require = createRequire(import.meta.url);
const { createChannel, statusOf } = require('../e2e/browser-impairment-channel.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
const steps = [
  ['clear', {}, 'baseline', null],
  ['apply', { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10' }, 'lossy', 'lossy'],
  ['apply', { IMPAIR_RATE_KBIT: '400', IMPAIR_DELAY_MS: '30' }, 'constrained', 'constrained'],
  ['apply', { IMPAIR_RATE_KBIT: '150', IMPAIR_DELAY_MS: '30' }, 'severe', 'severe'],
  ['clear', {}, 'recovery', null],
  [
    'apply',
    { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10', IMPAIR_DIRECTION: 'both' },
    'lossyJoin',
    'lossyJoin',
  ],
  ['clear', {}, undefined, null],
];
function fixture({ writeError, throwWrite = false, mode } = {}) {
  const frames = [];
  const timers = new Map();
  let time = 0;
  let timerId = 0;
  const stream = new Duplex({
    read() {},
    write(bytes, _encoding, callback) {
      frames.push(bytes.toString());
      callback(writeError);
    },
  });
  if (throwWrite)
    stream.write = () => {
      throw new Error('PRIVATE_WRITE');
    };
  const channel = createChannel({
    runId,
    mode,
    stream,
    now: () => time,
    setTimer(callback, milliseconds) {
      const id = ++timerId;
      timers.set(id, { callback, milliseconds });
      return id;
    },
    clearTimer(id) {
      timers.delete(id);
    },
  });
  return {
    channel,
    stream,
    frames,
    timers,
    time(value) {
      time = value;
    },
    ack(sequence, status = 0) {
      stream.push(
        `${JSON.stringify({ schemaVersion: 1, runId, sequence, passed: status === 0, status })}\n`,
      );
    },
    async step(index) {
      const [action, profile, phase] = steps[index];
      const task = channel.impair(action, profile, phase);
      this.ack(index + 1);
      await task;
    },
    assertClosed() {
      assert.equal(timers.size, 0);
      assert.equal(stream.closed, true);
      for (const name of ['data', 'end', 'error', 'close'])
        assert.equal(stream.listenerCount(name), 0, name);
    },
  };
}
async function refuses(f, status = 1) {
  await assert.rejects(f.channel.finish(), (error) => statusOf(error) === status);
  f.assertClosed();
}

test('seven exact canonical requests bind the UUID, sequence, phase and closed profile', async () => {
  const f = fixture();
  for (let index = 0; index < steps.length; index++) {
    await f.step(index);
    const [action, _values, phase, profile] = steps[index];
    assert.equal(
      f.frames[index],
      `${JSON.stringify({ schemaVersion: 1, runId, sequence: index + 1, phase: phase ?? 'finish', action, profile })}\n`,
    );
    assert.ok(Buffer.byteLength(f.frames[index]) <= 1024);
    assert.equal(f.timers.size, 0);
  }
  const closed = f.channel.finish();
  assert.equal(f.channel.finish(), closed, 'the descriptor retirement is memoized');
  await closed;
  f.assertClosed();
});

test('a final clear is allowed at a failing prefix but cannot claim complete control', async () => {
  for (let prefix = 0; prefix < 6; prefix++) {
    const f = fixture();
    for (let index = 0; index < prefix; index++) await f.step(index);
    const task = f.channel.impair('clear');
    f.ack(prefix + 1);
    await task;
    assert.equal(JSON.parse(f.frames.at(-1)).phase, 'finish');
    await refuses(f);
  }
});

test('phase order, action and numeric/environment overrides refuse without dispatch', async () => {
  for (const request of [
    ['apply', steps[1][1], 'lossy'],
    ['block-udp', {}, 'baseline'],
    ['clear', { IMPAIR_LOSS: '0' }, 'baseline'],
    ['clear', {}, 'finish'],
    ['clear', {}, 'PRIVATE_PHASE'],
    ['clear', null, 'baseline'],
  ]) {
    const f = fixture();
    await assert.rejects(f.channel.impair(...request), (error) => statusOf(error) === 1);
    assert.deepEqual(f.frames, []);
    await refuses(f);
  }
});

test('only one request can be in flight and both callers observe a terminal failure', async () => {
  const f = fixture();
  const first = assert.rejects(f.channel.impair('clear', {}, 'baseline'));
  await assert.rejects(f.channel.impair('clear', {}, 'baseline'));
  await first;
  assert.equal(f.frames.length, 1);
  await refuses(f);
});

test('fragmented matching ack is accepted; extra, duplicate and malformed data is refused', async () => {
  const good = `${JSON.stringify({ schemaVersion: 1, runId, sequence: 1, passed: true, status: 0 })}\n`;
  const f = fixture();
  const task = f.channel.impair('clear', {}, 'baseline');
  f.stream.push(good.slice(0, 30));
  assert.equal(f.timers.size, 1);
  f.stream.push(good.slice(30));
  await task;
  await refuses(f);
  for (const value of [
    good + good,
    good + '{',
    good.replace('"schemaVersion":1,', '"schemaVersion":1,"schemaVersion":1,'),
    good.replace('"schemaVersion"', '"schema\\u0056ersion"'),
    good.replace('"runId"', '"PRIVATE_UNKNOWN"'),
    good.replace(runId, '00000000-0000-4000-8000-000000000002'),
    good.replace('"sequence":1', '"sequence":2'),
    good.replace('"status":0', '"status":-0'),
    good.replace('"status":0', '"status":0.0'),
    good.replace('"passed":true', '"passed":1'),
    good.replace('"passed":true', '"passed":false'),
    good.replace('"status":0', '"status":256'),
    good.replace('"status":0', '"status":-1'),
    `${good.trim()} \n`,
    'null\n',
    '{invalid}\n',
    Buffer.from([0xff, 10]),
    ' '.repeat(1024),
  ]) {
    const g = fixture();
    const failed = assert.rejects(
      g.channel.impair('clear', {}, 'baseline'),
      (error) => statusOf(error) === 1,
    );
    g.stream.push(value);
    await failed;
    await refuses(g);
  }
});

test('EOF during partial input or after an ack and unsolicited trailing bytes fail', async () => {
  for (const mode of ['partial', 'after-ack', 'extra-after-ack']) {
    const f = fixture();
    const task = f.channel.impair('clear', {}, 'baseline');
    if (mode === 'partial') {
      const failed = assert.rejects(task);
      f.stream.push('{"schemaVersion":');
      f.stream.push(null);
      await failed;
    } else {
      f.ack(1);
      await task;
      if (mode === 'after-ack') f.stream.push(null);
      else f.stream.push('PRIVATE_TRAILING_DATA');
    }
    await refuses(f);
  }
});

test('observed nonzero ack status survives later socket or final-close failure', async () => {
  for (const code of [1, 42, 124, 130, 143, 255]) {
    const f = fixture();
    const failed = assert.rejects(
      f.channel.impair('clear', {}, 'baseline'),
      (error) => statusOf(error) === code,
    );
    f.ack(1, code);
    await failed;
    await refuses(f, code);
  }
});

test('the absolute deadline never resets and a late positive ack is not accepted', async () => {
  const f = fixture();
  await f.step(0);
  f.time(599900);
  const failed = assert.rejects(
    f.channel.impair(...steps[1].slice(0, 3)),
    (error) => statusOf(error) === 124,
  );
  assert.equal([...f.timers.values()][0].milliseconds, 100);
  f.time(600001);
  f.ack(2);
  await failed;
  await refuses(f, 124);
  const g = fixture();
  const timedOut = assert.rejects(
    g.channel.impair('clear', {}, 'baseline'),
    (error) => statusOf(error) === 124,
  );
  [...g.timers.values()][0].callback();
  await timedOut;
  await refuses(g, 124);
});

test('cancellation and write failures retire the stream, timer and every listener', async () => {
  for (const code of [130, 143]) {
    const f = fixture();
    const failed = assert.rejects(
      f.channel.impair('clear', {}, 'baseline'),
      (error) => statusOf(error) === code,
    );
    f.channel.cancel(code);
    f.channel.cancel(143);
    await failed;
    await refuses(f, code);
  }
  for (const options of [{ writeError: new Error('PRIVATE_IO') }, { throwWrite: true }]) {
    const f = fixture(options);
    await assert.rejects(f.channel.impair('clear', {}, 'baseline'), (error) => {
      assert.doesNotMatch(error.message, /PRIVATE_/);
      return statusOf(error) === 1;
    });
    await refuses(f);
  }
});

const tcpSteps = [['block-udp'], ['clear', {}, 'baseline'], ['clear'], ['unblock-udp']];

test('TCP control requires block, baseline, final clear and unblock in that exact order', async () => {
  const f = fixture({ mode: 'tcp' });
  for (const [index, values] of tcpSteps.entries()) {
    const task = f.channel.impair(...values);
    assert.equal(f.timers.size, 1);
    f.ack(index + 1);
    await task;
  }
  assert.deepEqual(
    f.frames.map((raw) => JSON.parse(raw)),
    ['block', 'baseline', 'finish', 'unblock'].map((phase, index) => ({
      schemaVersion: 1,
      runId,
      sequence: index + 1,
      phase,
      action: tcpSteps[index][0],
      profile: null,
    })),
  );
  await f.channel.finish();
  f.assertClosed();
});

test('TCP failing prefixes allow final clear and unblock without claiming full completion', async () => {
  for (const prefix of [0, 1]) {
    const f = fixture({ mode: 'tcp' });
    if (prefix) {
      const blocked = f.channel.impair('block-udp');
      f.ack(1);
      await blocked;
    }
    for (const [index, action] of ['clear', 'unblock-udp'].entries()) {
      const task = f.channel.impair(action);
      f.ack(prefix + index + 1);
      await task;
    }
    assert.deepEqual(
      f.frames.slice(prefix).map((raw) => JSON.parse(raw).phase),
      ['finish', 'unblock'],
    );
    await refuses(f);
  }
});

test('TCP control refuses profile overrides, premature unblock and duplicate finalization', async () => {
  for (const values of [
    ['unblock-udp'],
    ['clear', {}, 'baseline'],
    ['block-udp', {}, 'block'],
    ['block-udp', { IMPAIR_LOSS: '0' }],
    ['apply', steps[1][1], 'lossy'],
  ]) {
    const f = fixture({ mode: 'tcp' });
    await assert.rejects(f.channel.impair(...values));
    assert.deepEqual(f.frames, []);
    await refuses(f);
  }
  const f = fixture({ mode: 'tcp' });
  const cleared = f.channel.impair('clear');
  f.ack(1);
  await cleared;
  await assert.rejects(f.channel.impair('clear'));
  assert.equal(f.frames.length, 1);
  await refuses(f);
  const stream = new Duplex({
    read() {},
    write(_bytes, _encoding, callback) {
      callback();
    },
  });
  assert.throws(() => createChannel({ runId, mode: 'foreign', stream }));
  assert.equal(stream.listenerCount('data'), 0, 'invalid modes admit no stream owner');
  stream.destroy();
});
