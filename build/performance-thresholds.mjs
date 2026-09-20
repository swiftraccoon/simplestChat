#!/usr/bin/env node
// Evaluate a benchmark-local comparison against coarse regression thresholds.
//
// The runner already refuses to publish medians for failed workloads or
// unclean shutdowns. This adds a budget on the candidate's regression versus
// the baseline measured in the same run on the same host. Hosted runners are
// noisy, so the default thresholds are deliberately wide: they catch the kind
// of change that has already been recorded in docs/performance-results.md (a
// fifth of throughput, a third of CPU), not single-digit drifts.
import { readFileSync } from 'node:fs';

export const DEFAULT_THRESHOLDS = {
  receiveReadyP99Ms: 0.25,
  sendReadyP99Ms: 0.25,
  serverCpuPercent: 0.25,
  serverPeakRssMiB: 0.15,
};

/** Return the list of budget violations for one comparison document. */
export function evaluate(comparison, thresholds = DEFAULT_THRESHOLDS) {
  const violations = [];
  if (!comparison || comparison.passed !== true) {
    violations.push('comparison did not pass: workload or server shutdown failed');
    return violations;
  }
  for (const [scenario, metrics] of Object.entries(comparison.comparison ?? {})) {
    for (const [metric, limit] of Object.entries(thresholds)) {
      const entry = metrics[metric];
      if (!entry) {
        violations.push(`${scenario}: metric ${metric} missing from comparison`);
        continue;
      }
      const { baselineMedian, candidateMedian } = entry;
      if (!(baselineMedian > 0) || !Number.isFinite(candidateMedian)) {
        violations.push(`${scenario}: ${metric} has no usable baseline (${baselineMedian}) or candidate (${candidateMedian})`);
        continue;
      }
      const change = (candidateMedian - baselineMedian) / baselineMedian;
      if (change > limit) {
        violations.push(`${scenario}: ${metric} regressed ${(change * 100).toFixed(1)}% (baseline ${baselineMedian}, candidate ${candidateMedian}, budget +${(limit * 100).toFixed(0)}%)`);
      }
    }
  }
  return violations;
}

export function summarize(comparison) {
  const lines = [];
  for (const [scenario, metrics] of Object.entries(comparison.comparison ?? {})) {
    lines.push(`${scenario}:`);
    for (const [metric, entry] of Object.entries(metrics)) {
      lines.push(`  ${metric}: baseline ${entry.baselineMedian} [${entry.baselineRange.join('–')}] candidate ${entry.candidateMedian} [${entry.candidateRange.join('–')}] (${entry.deltaPercent >= 0 ? '+' : ''}${entry.deltaPercent.toFixed(1)}%)`);
    }
  }
  return lines.join('\n');
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const path = process.argv[2];
  if (!path) {
    console.error('usage: performance-thresholds.mjs <comparison.json>');
    process.exit(2);
  }
  const comparison = JSON.parse(readFileSync(path, 'utf8'));
  console.log(summarize(comparison));
  const violations = evaluate(comparison);
  if (violations.length > 0) {
    console.error('\nRegression budget violated:');
    for (const violation of violations) console.error(`- ${violation}`);
    process.exit(1);
  }
  console.log('\nWithin regression budget.');
}
