import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { evaluateTypeScript } from './source-loader.mjs';

const source = await readFile(new URL('../src/room.ts', import.meta.url), 'utf8');
const { ReceiveScheduler } = evaluateTypeScript(`${source}\nexport { ReceiveScheduler };`, {
  modules: { './receive-policy': {} },
});
const flush = async () => {
  for (let i = 0; i < 12; i++) await Promise.resolve();
};
function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test('voice overtakes queued video without overlapping a running native negotiation', async () => {
  const scheduler = new ReceiveScheduler();
  const gate = deferred();
  const calls = [];
  const first = scheduler.enqueue('video', async () => {
    calls.push('running');
    await gate.promise;
  });
  await flush();
  const video = scheduler.enqueue('video', async () => {
    calls.push('video');
  });
  const audio = scheduler.enqueue('audio', async () => {
    calls.push('voice');
  });
  let idle = false;
  const drained = scheduler.idle().then(() => {
    idle = true;
  });
  await flush();
  assert.deepEqual(calls, ['running']);
  assert.equal(idle, false);
  gate.resolve();
  await Promise.all([first, video, audio, drained]);
  assert.deepEqual(calls, ['running', 'voice', 'video']);
  assert.equal(idle, true);
});

test('receiver reset releases pending work and stale settlement cannot drain the replacement queue', async () => {
  const scheduler = new ReceiveScheduler();
  const old = deferred(),
    fresh = deferred();
  const calls = [];
  const abandoned = scheduler.enqueue('video', async () => {
    await old.promise;
  });
  await flush();
  const queued = scheduler.enqueue('audio', async () => {
    calls.push('abandoned');
  });
  const idle = scheduler.idle();
  scheduler.reset();
  await Promise.all([abandoned, queued, idle]);
  const active = scheduler.enqueue('video', async () => {
    calls.push('fresh');
    await fresh.promise;
  });
  const waiting = scheduler.enqueue('audio', async () => {
    calls.push('new voice');
  });
  await flush();
  old.resolve();
  await flush();
  assert.deepEqual(calls, ['new voice', 'fresh']);
  let done = false;
  const drained = scheduler.idle().then(() => {
    done = true;
  });
  await flush();
  assert.equal(done, false);
  fresh.resolve();
  await Promise.all([active, waiting, drained]);
  assert.equal(done, true);
});

test('bounded queue rejects overload and a failed job cannot block later voice', async () => {
  const scheduler = new ReceiveScheduler();
  const gate = deferred();
  const active = scheduler.enqueue('video', () => gate.promise);
  await flush();
  const jobs = Array.from({ length: 256 }, () => scheduler.enqueue('video', async () => {}));
  await assert.rejects(
    scheduler.enqueue('audio', async () => {}),
    /Too many incoming/,
  );
  scheduler.reset();
  await Promise.all([active, ...jobs]);
  const failed = scheduler.enqueue('video', async () => {
    throw new Error('codec failure');
  });
  await assert.rejects(failed, /codec failure/);
  let voice = false;
  await scheduler.enqueue('audio', async () => {
    voice = true;
  });
  assert.equal(voice, true);
  await scheduler.idle();
});
