import assert from 'node:assert/strict';
import test from 'node:test';
import metrics from '../e2e/performance-metrics.cjs';
import { readFileSync } from 'node:fs';

test('browser performance uses the shared fake-device options without autoplay bypass', () => {
  const source = readFileSync(new URL('../e2e/performance.cjs', import.meta.url), 'utf8');
  assert.match(source, /require\('\.\/browser-options\.cjs'\)/);
  assert.match(source, /chromium\.launch\(browserConfiguration\.launchOptions\)/);
  assert.match(source, /\.\.\.browserConfiguration\.contextOptions/);
  assert.doesNotMatch(source, /autoplay-policy|no-user-gesture-required|ignoreDefaultArgs/);
});

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
