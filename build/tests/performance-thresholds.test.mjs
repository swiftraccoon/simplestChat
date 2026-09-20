import assert from 'node:assert/strict';
import test from 'node:test';
import { evaluate, summarize } from '../performance-thresholds.mjs';

const entry = (baseline, candidate) => ({
  baselineMedian: baseline, candidateMedian: candidate,
  delta: candidate - baseline, deltaPercent: ((candidate - baseline) / baseline) * 100,
  baselineRange: [baseline, baseline], candidateRange: [candidate, candidate],
});
const comparison = (overrides = {}) => ({
  passed: true,
  comparison: {
    'conference-10': {
      receiveReadyP99Ms: entry(412, 412),
      sendReadyP99Ms: entry(410, 410),
      serverCpuPercent: entry(18.8, 18.5),
      serverPeakRssMiB: entry(95, 94),
      ...overrides,
    },
  },
});

test('a comparison within the budget has no violations', () => {
  assert.deepEqual(evaluate(comparison()), []);
  assert.match(summarize(comparison()), /conference-10:/);
});

test('a failed workload is a violation before any metric is read', () => {
  assert.deepEqual(evaluate({ passed: false, comparison: {} }), [
    'comparison did not pass: workload or server shutdown failed',
  ]);
});

test('regressions past the coarse budget are named with their size', () => {
  const violations = evaluate(comparison({ serverCpuPercent: entry(20, 26), serverPeakRssMiB: entry(100, 120) }));
  assert.equal(violations.length, 2);
  assert.match(violations[0], /serverCpuPercent regressed 30\.0%/);
  assert.match(violations[1], /serverPeakRssMiB regressed 20\.0%/);
});

test('improvements and a missing baseline are handled explicitly', () => {
  assert.deepEqual(evaluate(comparison({ serverCpuPercent: entry(20, 10) })), []);
  const [violation] = evaluate(comparison({ serverCpuPercent: entry(0, 10) }));
  assert.match(violation, /no usable baseline/);
});
