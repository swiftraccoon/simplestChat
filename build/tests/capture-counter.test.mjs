import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('../../web/e2e/community.cjs', import.meta.url), 'utf8');
const marker = 'await page.addInitScript(() => {';
const start = source.indexOf(marker) + 'await page.addInitScript('.length;
const end = source.indexOf('\n  });', start) + '\n  }'.length;
assert.ok(start >= marker.length && end > start, 'capture init script exists');
const init = source.slice(start, end);

function fixture() {
  const calls = [];
  const result = Promise.resolve({ owned: 'fake stream' });
  const devices = {
    getUserMedia(...args) { calls.push({ receiver: this, args }); return result; },
  };
  const native = devices.getUserMedia;
  const navigator = { mediaDevices: devices };
  const window = {};
  runInNewContext(`(${init})()`, { window, navigator });
  return { window, navigator, devices, native, calls, result };
}

test('capture observer roots the exact devices and wrapper without starting capture', () => {
  const { window, devices, calls } = fixture();
  assert.equal(window.__communityCaptureObservation.devices, devices);
  assert.equal(window.__communityCaptureObservation.getUserMedia, devices.getUserMedia);
  assert.equal(window.__communityCaptureRequests, 0);
  assert.deepEqual(calls, []);
});

test('capture observer counts each call once while preserving receiver, arguments and native result', async () => {
  const { window, devices, calls, result } = fixture();
  const constraints = { audio: true, video: false };
  assert.equal(devices.getUserMedia(constraints), result);
  assert.equal(window.__communityCaptureRequests, 1);
  assert.equal(calls[0].receiver, devices);
  assert.equal(calls[0].args[0], constraints);
  await result;
  assert.equal(devices.getUserMedia({ video: true }), result);
  assert.equal(window.__communityCaptureRequests, 2);
});

for (const changed of ['devices', 'method', 'root']) {
  test(`capture observer fails loudly if its ${changed} is replaced; it never silently reinstalls`, () => {
    const { window, navigator, devices, native } = fixture();
    devices.getUserMedia({ audio: true });
    if (changed === 'devices') navigator.mediaDevices = { getUserMedia: native };
    if (changed === 'method') devices.getUserMedia = native;
    if (changed === 'root') delete window.__communityCaptureObservation;
    assert.throws(() => window.__communityCaptureRequests, /Capture instrumentation was replaced/);
    assert.throws(() => window.__communityCaptureRequests, /Capture instrumentation was replaced/);
    if (changed === 'method') assert.equal(devices.getUserMedia, native);
    if (changed === 'root') assert.equal(window.__communityCaptureObservation, undefined);
  });
}

test('each document owns an independent observer and counter', () => {
  const first = fixture();
  const second = fixture();
  first.devices.getUserMedia({ audio: true });
  assert.equal(first.window.__communityCaptureRequests, 1);
  assert.equal(second.window.__communityCaptureRequests, 0);
  assert.notEqual(first.window.__communityCaptureObservation, second.window.__communityCaptureObservation);
});
