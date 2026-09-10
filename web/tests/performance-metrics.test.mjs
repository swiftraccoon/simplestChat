import assert from 'node:assert/strict';
import test from 'node:test';
import metrics from '../e2e/performance-metrics.cjs';

test('browser frame deltas ignore an idle probation SSRC before the active receiver', () => {
  const before = [
    { id: 'probation', kind: 'video', framesDecoded: 0 },
    { id: 'camera', kind: 'video', framesDecoded: 1 },
  ];
  const after = [
    { id: 'probation', kind: 'video', framesDecoded: 0 },
    { id: 'camera', kind: 'video', framesDecoded: 82 },
    { id: 'audio', kind: 'audio' },
  ];
  assert.equal(metrics.decodedVideoFrames(before, after), 81);
  assert.equal(metrics.decodedVideoFrames(after, after), 0);
});
