import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { Duplex } from 'node:stream';
import test from 'node:test';

const require = createRequire(import.meta.url);
const { createChannel, statusOf } = require('../e2e/restart-browser-channel.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
const phases = ['stop-first', 'start-second', 'stop-second'];
const flush = async () => {
  for (let index = 0; index < 20; index++) await Promise.resolve();
};
function fixture({ delayedWrite = false } = {}) {
  const timers = new Map();
  const frames = [];
  const callbacks = [];
  let clock = 0;
  let next = 0;
  const stream = new Duplex({
    read() {},
    write(bytes, _encoding, callback) {
      frames.push(bytes.toString());
      if (delayedWrite) callbacks.push(callback);
      else callback();
    },
  });
  const channel = createChannel({
    runId,
    stream,
    now: () => clock,
    setTimer(callback, milliseconds) {
      const id = ++next;
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
    timers,
    frames,
    callbacks,
    time(value) {
      clock = value;
    },
    ack(sequence, status = 0, extra = {}) {
      return `${JSON.stringify({ schemaVersion: 1, runId, sequence, passed: status === 0, status, ...extra })}\n`;
    },
    async step(index) {
      const task = channel.request(phases[index]);
      stream.push(this.ack(index + 1));
      await task;
    },
    assertClosed() {
      assert.equal(timers.size, 0);
      assert.equal(stream.closed, true);
      for (const event of ['data', 'end', 'error', 'close'])
        assert.equal(stream.listenerCount(event), 0, event);
    },
  };
}
async function refused(f, code = 1) {
  await assert.rejects(f.channel.finish(), (error) => statusOf(error) === code);
  f.assertClosed();
}

test('exactly three canonical transition frames bind UUID and sequence', async () => {
  const f = fixture();
  for (let index = 0; index < phases.length; index++) {
    await f.step(index);
    assert.equal(
      f.frames[index],
      `${JSON.stringify({ schemaVersion: 1, runId, sequence: index + 1, phase: phases[index] })}\n`,
    );
    assert.ok(Buffer.byteLength(f.frames[index]) <= 1024);
  }
  const done = f.channel.finish();
  assert.equal(f.channel.finish(), done);
  await done;
  f.assertClosed();
});

test('phase order, concurrent requests, prefixes and requests after completion refuse', async () => {
  for (const phase of ['start-second', 'stop-second', 'PRIVATE', undefined]) {
    const f = fixture();
    await assert.rejects(f.channel.request(phase));
    assert.equal(f.frames.length, 0);
    await refused(f);
  }
  for (let prefix = 0; prefix < 3; prefix++) {
    const f = fixture();
    for (let index = 0; index < prefix; index++) await f.step(index);
    await refused(f);
  }
  const f = fixture();
  for (let index = 0; index < 3; index++) await f.step(index);
  await assert.rejects(f.channel.request(undefined));
  await refused(f);
  const concurrent = fixture();
  const first = concurrent.channel.request('stop-first');
  const rejected = assert.rejects(first);
  await assert.rejects(concurrent.channel.request('stop-first'));
  await rejected;
  await refused(concurrent);
});

test('fragmented UTF8 acknowledgement is accepted only after its owned write settles', async () => {
  const f = fixture({ delayedWrite: true });
  let completed = false;
  const task = f.channel.request('stop-first').then(() => (completed = true));
  const bytes = Buffer.from(f.ack(1));
  f.stream.push(bytes.subarray(0, 13));
  f.stream.push(bytes.subarray(13));
  await flush();
  assert.equal(completed, false);
  f.callbacks.shift()();
  await task;
  assert.equal(completed, true);
  await refused(f);
});

test('unknown, duplicate, noncanonical, oversized and invalid acknowledgement bytes refuse', async () => {
  const base = `${JSON.stringify({ schemaVersion: 1, runId, sequence: 1, passed: true, status: 0 })}\n`;
  for (const mutate of [
    () => `${base.trimEnd()} \n`,
    () => `${base}${base}`,
    () => `${base}x`,
    () => base.replace('"schemaVersion":1', '"schemaVersion":1,"schemaVersion":1'),
    () => base.replace(runId, '00000000-0000-4000-8000-000000000002'),
    () => base.replace('"sequence":1', '"sequence":2'),
    () => base.replace('"passed":true', '"passed":false'),
    () => base.replace('"status":0', '"status":1.0'),
    () => base.replace('"status":0', '"status":true'),
    () => base.replace('"status":0', '"status":256'),
    () => base.replace('"status":0', '"status":0,"PRIVATE":1'),
    () => Buffer.from([0xff, 10]),
    () => 'x'.repeat(1024),
  ]) {
    const f = fixture();
    const task = f.channel.request('stop-first');
    const rejected = assert.rejects(task, (error) => statusOf(error) === 1);
    f.stream.push(mutate());
    await rejected;
    await refused(f);
  }
});

test('known negative status, EOF, deadline and cancellation are sticky and retire listeners', async () => {
  for (const code of [1, 42, 124, 130, 143, 255]) {
    const f = fixture();
    const task = f.channel.request('stop-first');
    const rejected = assert.rejects(task, (error) => statusOf(error) === code);
    f.stream.push(f.ack(1, code));
    await rejected;
    f.channel.cancel(143);
    await refused(f, code);
  }
  for (const action of ['eof', 'partial-eof', 'deadline', 'cancel']) {
    const f = fixture();
    const task = f.channel.request('stop-first');
    const expected = action === 'deadline' ? 124 : action === 'cancel' ? 130 : 1;
    const rejected = assert.rejects(task, (error) => statusOf(error) === expected);
    if (action === 'partial-eof') f.stream.push('{');
    if (action.endsWith('eof')) f.stream.push(null);
    else if (action === 'deadline') [...f.timers.values()][0].callback();
    else f.channel.cancel(130);
    await rejected;
    await refused(f, expected);
  }
  const late = fixture();
  late.time(75000);
  await assert.rejects(late.channel.request('stop-first'), (error) => statusOf(error) === 124);
  assert.equal(late.frames.length, 0);
  await refused(late, 124);
});

test('cancellation waits for a pending write callback even after socket closure', async () => {
  const f = fixture({ delayedWrite: true });
  const task = f.channel.request('stop-first');
  const rejected = assert.rejects(task, (error) => statusOf(error) === 143);
  f.channel.cancel(143);
  await rejected;
  let done = false;
  const retirement = f.channel.finish().catch((error) => {
    assert.equal(statusOf(error), 143);
    done = true;
  });
  await flush();
  assert.equal(done, false);
  f.callbacks.shift()(new Error('PRIVATE_WRITE'));
  await retirement;
  f.assertClosed();
});
