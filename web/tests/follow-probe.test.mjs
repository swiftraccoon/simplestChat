import assert from 'node:assert/strict';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import helpers from '../e2e/privacy-follow-checks.cjs';

const { followProbeFailure, installFollowCaptureObservation } = helpers;
const plain = (value) => JSON.parse(JSON.stringify(value));

test('Follow probe errors expose only fixed classifications and allowlisted source coordinates', () => {
  const error = new TypeError(
    'undefined is not an object (evaluating navigator.mediaDevices.secret)',
  );
  error.stack =
    'private-user@example.test?token=owned-secret\ninstallFollowCaptureObservation@https://private-host.test/path?token=owned-secret:27:11';
  assert.deepEqual(plain(followProbeFailure(error, 'goto')), {
    phase: 'goto',
    name: 'TypeError',
    category: 'media-devices-unavailable',
    frame: { source: 'follow-capture-init', line: 27, column: 11 },
  });
  error.message = 'owned-secret';
  error.stack = 'at boot (https://private-host.test/assets/index-SECRET.js:1:12345)';
  const asset = followProbeFailure(error, 'reload');
  assert.deepEqual(plain(asset.frame), { source: 'index.js', line: 1, column: 12345 });
  assert.doesNotMatch(JSON.stringify(asset), /private|SECRET|owned-secret|https/);
  error.stack = '/private/account/privacy-follow-checks.cjs:208:9';
  assert.deepEqual(plain(followProbeFailure(error, 'reload').frame), {
    source: 'privacy-follow-checks.cjs',
    line: 208,
    column: 9,
  });
});

test('Follow probe diagnostics classify known exceptions and reject arbitrary source metadata', () => {
  for (const [name, message, category] of [
    ['AbortError', 'owned-secret', 'AbortError'],
    ['InvalidStateError', 'owned-secret', 'InvalidStateError'],
    ['NotAllowedError', 'owned-secret', 'NotAllowedError'],
    ['Error', 'Attempted to assign to readonly property.', 'readonly-property'],
    ['TypeError', 'Failed to fetch dynamically imported module: owned-secret', 'module-load'],
    ['TypeError', 'Load failed: owned-secret', 'network-load'],
    ['owned-secret-name', 'owned-secret-message', 'other'],
  ]) {
    const value = followProbeFailure(
      { name, message, stack: 'owned-secret-stack:23:8' },
      'owned-secret',
    );
    assert.equal(value.category, category);
    assert.equal(value.phase, 'unknown');
    assert.equal(value.frame, null);
    assert.doesNotMatch(JSON.stringify(value), /owned-secret/);
  }
});

function captureFixture() {
  const calls = [];
  const devices = {
    getUserMedia(...args) {
      calls.push({ receiver: this, args, method: 'getUserMedia' });
      return 'camera-result';
    },
    getDisplayMedia(...args) {
      calls.push({ receiver: this, args, method: 'getDisplayMedia' });
      return 'screen-result';
    },
  };
  const window = {};
  const navigator = { mediaDevices: devices };
  runInNewContext(`(${installFollowCaptureObservation.toString()})()`, { window, navigator });
  return { window, navigator, devices, calls };
}

test('Follow capture observation roots the native wrapper and counts only explicit calls', () => {
  const f = captureFixture();
  assert.equal(f.window.__followCaptureObservation.devices, f.devices);
  assert.equal(f.window.__followPreferenceCaptures(), 0);
  assert.equal(f.calls.length, 0);
  const constraints = { video: true };
  assert.equal(f.devices.getUserMedia(constraints), 'camera-result');
  assert.equal(f.devices.getDisplayMedia(constraints), 'screen-result');
  assert.equal(f.window.__followPreferenceCaptures(), 2);
  assert.deepEqual(
    f.calls.map((call) => call.method),
    ['getUserMedia', 'getDisplayMedia'],
  );
  for (const call of f.calls) {
    assert.equal(call.receiver, f.devices);
    assert.equal(call.args[0], constraints);
  }
});

test('Follow capture observation fails closed when methods, wrapper or ownership change', () => {
  for (const replace of [
    (f) => {
      f.devices.getUserMedia = () => {};
    },
    (f) => {
      f.devices.getDisplayMedia = () => {};
    },
    (f) => {
      f.navigator.mediaDevices = { ...f.devices };
    },
    (f) => {
      f.window.__followCaptureObservation = {};
    },
  ]) {
    const f = captureFixture();
    replace(f);
    assert.throws(
      () => f.window.__followPreferenceCaptures(),
      /Follow capture observer was replaced/,
    );
  }
  assert.throws(
    () =>
      runInNewContext(`(${installFollowCaptureObservation.toString()})()`, {
        window: {},
        navigator: {},
      }),
    /Follow capture media devices unavailable/,
  );
});
