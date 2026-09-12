import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtempSync, readFileSync, statSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import reports from '../e2e/performance-report.cjs';

test('initialization refuses stale passing reports and leftover pending evidence', (t) => {
  const directory = mkdtempSync(join(tmpdir(), 'simplestchat-performance-report-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const filename = join(directory, 'browser-performance.json');
  writeFileSync(filename, JSON.stringify({ passed: true, runId: 'older' }), { flag: 'wx' });
  assert.throws(() => reports.initializePerformanceReport(filename, { passed: false }), /EEXIST/);
  assert.deepEqual(JSON.parse(readFileSync(filename, 'utf8')), { passed: true, runId: 'older' });
  const blocked = join(directory, 'blocked.json');
  writeFileSync(`${blocked}.pending`, 'previous partial evidence', { flag: 'wx' });
  assert.throws(
    () => reports.initializePerformanceReport(blocked, { passed: false }),
    /pending.*already exists/,
  );
  assert.throws(() => statSync(blocked), /ENOENT/);
  const fresh = join(directory, 'fresh.json');
  reports.initializePerformanceReport(fresh, { passed: false });
  assert.equal(statSync(fresh).mode & 0o777, 0o600);
  assert.deepEqual(JSON.parse(readFileSync(fresh, 'utf8')), { passed: false });
});

test('combined workload and finalization failures preserve the original and log later errors', async () => {
  const report = { completed: false, passed: false, failure: 'original failure' };
  const logged = [];
  let writes = 0;
  await reports.finalizePerformanceReport(
    report,
    async () => {
      throw new Error('close failed');
    },
    () => {
      if (++writes === 2) throw new Error('final write failed');
    },
    (message) => logged.push(message),
  );
  assert.equal(report.failure, 'original failure');
  assert.deepEqual(logged, [
    'Browser cleanup: close failed',
    'Final report write: final write failed',
  ]);
});

test('atomic report persistence retains the previous snapshot when a write cannot start', (t) => {
  const directory = mkdtempSync(join(tmpdir(), 'simplestchat-performance-report-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const filename = join(directory, 'browser-performance.json');
  reports.persistPerformanceReport(filename, { passed: false });
  assert.equal(statSync(filename).mode & 0o777, 0o600);
  writeFileSync(`${filename}.pending`, 'retained partial write', { flag: 'wx' });
  assert.throws(() => reports.persistPerformanceReport(filename, { passed: true }), /EEXIST/);
  assert.deepEqual(JSON.parse(readFileSync(filename, 'utf8')), { passed: false });
  assert.equal(readFileSync(`${filename}.pending`, 'utf8'), 'retained partial write');
});

test('atomic report persistence replaces a completed snapshot without leaving a temporary file', (t) => {
  const directory = mkdtempSync(join(tmpdir(), 'simplestchat-performance-report-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const filename = join(directory, 'browser-performance.json');
  reports.persistPerformanceReport(filename, { passed: false });
  reports.persistPerformanceReport(filename, { passed: true });
  assert.deepEqual(JSON.parse(readFileSync(filename, 'utf8')), { passed: true });
  assert.throws(() => statSync(`${filename}.pending`), /ENOENT/);
});

test('performance success is persisted only after browser cleanup', async () => {
  const report = { completed: true, passed: true };
  const snapshots = [];
  await reports.finalizePerformanceReport(
    report,
    async () => {
      assert.equal(snapshots.length, 1);
      assert.equal(snapshots[0].passed, false);
      assert.equal(snapshots[0].finalization.completed, false);
    },
    (value) => snapshots.push(structuredClone(value)),
  );
  assert.equal(snapshots.length, 2);
  assert.equal(report.passed, true);
  assert.equal(report.finalization.browserCleanupCompleted, true);
  assert.equal(report.finalization.completed, true);
});

test('browser cleanup failure is retained and fails a passing workload', async () => {
  const report = { completed: true, passed: true };
  const snapshots = [];
  await assert.rejects(
    reports.finalizePerformanceReport(
      report,
      async () => {
        throw new Error('close failed');
      },
      (value) => snapshots.push(structuredClone(value)),
    ),
    /Browser cleanup: close failed/,
  );
  assert.equal(report.passed, false);
  assert.equal(report.finalization.browserCleanupCompleted, false);
  assert.equal(snapshots.at(-1).passed, false);
});

test('cleanup errors cannot replace the original workload failure', async () => {
  const report = { completed: false, passed: false, failure: 'original workload failure' };
  await reports.finalizePerformanceReport(
    report,
    async () => {
      throw new Error('close failed');
    },
    () => {},
  );
  assert.equal(report.failure, 'original workload failure');
  assert.equal(report.passed, false);
  assert.deepEqual(report.finalization.errors, ['Browser cleanup: close failed']);
});

test('initial report write failure still closes the browser and fails final evidence', async () => {
  const report = { completed: true, passed: true };
  let writes = 0;
  let closed = false;
  await assert.rejects(
    reports.finalizePerformanceReport(
      report,
      async () => {
        closed = true;
      },
      () => {
        if (++writes === 1) throw new Error('disk write failed');
      },
    ),
    /Initial report write: disk write failed/,
  );
  assert.equal(closed, true);
  assert.equal(writes, 2);
  assert.equal(report.passed, false);
});

test('final report write failure leaves the retained snapshot nonpassing', async () => {
  const report = { completed: true, passed: true };
  let snapshot;
  await assert.rejects(
    reports.finalizePerformanceReport(
      report,
      async () => {},
      (value) => {
        if (snapshot) throw new Error('disk write failed');
        snapshot = structuredClone(value);
      },
    ),
    /Final report write: disk write failed/,
  );
  assert.equal(snapshot.passed, false);
  assert.equal(snapshot.finalization.completed, false);
  assert.equal(report.passed, false);
});
