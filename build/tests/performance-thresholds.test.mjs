import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test, { after } from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
execFileSync('cargo', ['build', '--locked', '-q', '-p', 'xtask'], {
  cwd: root,
  stdio: 'inherit',
});
const binary = path.join(root, 'target/debug/xtask');
const directory = mkdtempSync(path.join(tmpdir(), 'simplestchat-thresholds-'));
after(() => rmSync(directory, { recursive: true, force: true }));
let nextFixture = 0;

const entry = (baseline, candidate) => ({
  baselineMedian: baseline,
  candidateMedian: candidate,
  delta: candidate - baseline,
  // A zero baseline is unusable for the budget, but the summary remains a
  // valid typed document so that the command can name the policy violation.
  deltaPercent: baseline === 0 ? 0 : ((candidate - baseline) / baseline) * 100,
  baselineRange: [baseline, baseline],
  candidateRange: [candidate, candidate],
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
function run(comparison) {
  const filename = path.join(directory, `${nextFixture++}.json`);
  writeFileSync(
    filename,
    typeof comparison === 'string' ? comparison : JSON.stringify(comparison),
  );
  return spawnSync(binary, ['performance-thresholds', filename], {
    cwd: root,
    encoding: 'utf8',
  });
}

test('a comparison within the budget has no violations', () => {
  const result = run(comparison());
  assert.equal(result.status, 0);
  assert.equal(result.stderr, '');
  assert.match(result.stdout, /conference-10:/);
  assert.match(result.stdout, /Within regression budget\.\n$/);
});

test('a failed workload is a violation before any metric is read', () => {
  const result = run({ passed: false, comparison: {} });
  assert.equal(result.status, 1);
  assert.equal(
    result.stderr,
    '\nRegression budget violated:\n- comparison did not pass: workload or server shutdown failed\n',
  );
});

test('regressions past the coarse budget are named with their size', () => {
  const result = run(
    comparison({
      serverCpuPercent: entry(20, 26),
      serverPeakRssMiB: entry(100, 120),
    }),
  );
  assert.equal(result.status, 1);
  const violations = result.stderr
    .split('\n')
    .filter((line) => line.startsWith('- '));
  assert.equal(violations.length, 2);
  assert.match(violations[0], /serverCpuPercent regressed 30\.0%/);
  assert.match(violations[1], /serverPeakRssMiB regressed 20\.0%/);
});

test('improvements and a missing baseline are handled explicitly', () => {
  assert.equal(run(comparison({ serverCpuPercent: entry(20, 10) })).status, 0);
  const result = run(comparison({ serverCpuPercent: entry(0, 10) }));
  assert.equal(result.status, 1);
  assert.match(result.stderr, /no usable baseline/);
});

test('exact budget boundaries pass and a missing required metric fails by name', () => {
  assert.equal(
    run(
      comparison({
        serverCpuPercent: entry(100, 125),
        serverPeakRssMiB: entry(100, 115),
      }),
    ).status,
    0,
  );
  assert.equal(
    run(comparison({ serverCpuPercent: entry(1e-6, 1e-6 * 1.25) })).status,
    0,
  );
  const incomplete = comparison();
  delete incomplete.comparison['conference-10'].sendReadyP99Ms;
  const result = run(incomplete);
  assert.equal(result.status, 1);
  assert.match(
    result.stderr,
    /conference-10: metric sendReadyP99Ms missing from comparison/,
  );
});

test('the CLI preserves source order, summaries and JavaScript decimal rounding', () => {
  const fixture = {
    passed: true,
    comparison: {
      z: {
        z: { ...entry(1e-7, 1e21), deltaPercent: 2.55 },
        a: { ...entry(1, 1), deltaPercent: 1.25 },
      },
      a: {},
    },
  };
  const result = run(fixture);
  assert.equal(
    result.stdout,
    'z:\n  z: baseline 1e-7 [1e-7–1e-7] candidate 1e+21 [1e+21–1e+21] (+2.5%)\n  a: baseline 1 [1–1] candidate 1 [1–1] (+1.3%)\na:\n',
  );
  assert.equal(result.status, 1, 'required metrics are still enforced');
});

test('malformed documents, unreadable files and missing arguments fail explicitly', () => {
  assert.equal(run('{broken').status, 1);
  assert.match(
    run({ passed: true, comparison: { a: { serverCpuPercent: {} } } }).stderr,
    /Decode benchmark comparison/,
  );
  const missing = spawnSync(
    binary,
    ['performance-thresholds', path.join(directory, 'missing.json')],
    { encoding: 'utf8' },
  );
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /Read benchmark comparison/);
  const usage = spawnSync(binary, ['performance-thresholds'], {
    encoding: 'utf8',
  });
  assert.equal(usage.status, 2);
  assert.match(
    usage.stderr,
    /^usage: .*performance-thresholds <comparison\.json>/,
  );
});

test('the performance workflow invokes the maintained Rust policy reader', () => {
  const workflow = readFileSync(
    path.join(root, '.github/workflows/performance.yml'),
    'utf8',
  );
  assert.match(
    workflow,
    /cargo run --locked -q -p xtask -- performance-thresholds/,
  );
  assert.doesNotMatch(workflow, /node build\/performance-thresholds\.mjs/);
});
