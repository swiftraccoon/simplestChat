import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import test from 'node:test';

const require = createRequire(import.meta.url);
const {
  closeOwnedBrowser,
  startFinalizationWatchdog,
  canCancelFinalizationWatchdog,
} = require('../e2e/lifecycle-cleanup.cjs');
const flush = async () => {
  for (let index = 0; index < 20; index++) await Promise.resolve();
};
const pending = () => new Promise(() => {});
function fixture() {
  const timers = new Set();
  const calls = [];
  const saved = [];
  const report = { completed: true, passed: true };
  const child = { exitCode: null, signalCode: null };
  const server = {
    process: () => child,
    close: () => {
      calls.push('close');
      child.exitCode = 0;
    },
    kill: () => {
      calls.push('kill');
      child.exitCode = 0;
    },
  };
  const options = {
    setTimeout(callback, milliseconds) {
      const timer = { callback, milliseconds };
      timers.add(timer);
      return timer;
    },
    clearTimeout(timer) {
      timers.delete(timer);
    },
  };
  return {
    timers,
    calls,
    saved,
    report,
    child,
    server,
    options,
    persist: () => saved.push(structuredClone(report)),
    async fire(milliseconds) {
      const timer = [...timers].find((timer) => timer.milliseconds === milliseconds);
      assert.ok(timer, `Expected ${milliseconds}ms timer`);
      timers.delete(timer);
      timer.callback();
      await flush();
    },
  };
}

test('owned browser cleanup retains pending evidence and requires a clean observed exit', async () => {
  const f = fixture();
  const result = await closeOwnedBrowser(f.server, f.report, f.persist, f.options);
  assert.deepEqual(f.calls, ['close']);
  assert.equal(f.saved[0].passed, false);
  assert.equal(f.saved[0].browserCleanup.completed, false);
  assert.equal(result.passed, true);
  assert.equal(result.exitCode, 0);
  assert.equal(result.signalCode, null);
  assert.equal(result.forceKillRequested, false);
  assert.equal(f.report.passed, false, 'Only finalization may declare the whole run passed');
  assert.equal(f.timers.size, 0);
});

test('nonzero or signaled browser exits fail even when close resolves', async () => {
  for (const state of [
    { exitCode: 9, signalCode: null },
    { exitCode: null, signalCode: 'SIGKILL' },
  ]) {
    const f = fixture();
    f.server.close = () => {
      f.calls.push('close');
      Object.assign(f.child, state);
    };
    await assert.rejects(
      closeOwnedBrowser(f.server, f.report, f.persist, f.options),
      /did not exit cleanly/,
    );
    assert.deepEqual(f.calls, ['close']);
    assert.equal(f.report.browserCleanup.passed, false);
    assert.equal(f.report.browserCleanup.exitStatusObserved, true);
  }
});

test('graceful timeout retains failure before owned escalation and cannot become a pass', async () => {
  const f = fixture();
  let settle;
  f.server.close = () => {
    f.calls.push('close');
    return new Promise((resolve) => {
      settle = resolve;
    });
  };
  const done = assert.rejects(
    closeOwnedBrowser(f.server, f.report, f.persist, f.options),
    /Graceful browser close timed out/,
  );
  await flush();
  await f.fire(10000);
  await done;
  assert.deepEqual(f.calls, ['close', 'kill']);
  assert.ok(
    f.saved.some((snapshot) => snapshot.browserCleanup.forceKillRequested && !snapshot.passed),
  );
  assert.equal(f.report.browserCleanup.passed, false);
  assert.equal(f.report.browserCleanup.forceKillCompleted, true);
  assert.equal(
    f.report.browserCleanup.exitCode,
    0,
    'A later zero exit cannot erase forced cleanup',
  );
  settle();
  await flush();
  assert.equal(f.report.browserCleanup.gracefulCloseTimedOut, true);
  assert.equal(f.report.browserCleanup.gracefulCloseCompleted, false);
  assert.equal(f.timers.size, 0);
});

test('rejected close and unobserved exit both trigger only the supplied owner cleanup', async () => {
  for (const outcome of ['rejected', 'unknown']) {
    const f = fixture();
    f.server.close = () => {
      f.calls.push('close');
      if (outcome === 'rejected') throw new Error('PRIVATE_ENDPOINT');
    };
    await assert.rejects(
      closeOwnedBrowser(f.server, f.report, f.persist, f.options),
      /Forced browser termination/,
    );
    assert.deepEqual(f.calls, ['close', 'kill']);
    assert.doesNotMatch(JSON.stringify(f.report), /PRIVATE_ENDPOINT/);
    assert.equal(f.report.browserCleanup.passed, false);
  }
});

test('never-settling forced cleanup fails with unknown exit and keeps the final watchdog necessary', async () => {
  const f = fixture();
  f.server.close = pending;
  f.server.kill = pending;
  const done = assert.rejects(
    closeOwnedBrowser(f.server, f.report, f.persist, f.options),
    /Forced browser cleanup timed out/,
  );
  await flush();
  await f.fire(10000);
  await f.fire(5000);
  await done;
  assert.equal(f.report.browserCleanup.exitStatusObserved, false);
  assert.equal(f.report.browserCleanup.forceKillTimedOut, true);
  assert.equal(canCancelFinalizationWatchdog(f.server, f.report), false);
  assert.equal(f.timers.size, 0);
});

test('report-write failures do not skip owned cleanup or expose underlying errors', async () => {
  const f = fixture();
  await assert.rejects(
    closeOwnedBrowser(
      f.server,
      f.report,
      () => {
        throw new Error('PRIVATE_PATH');
      },
      f.options,
    ),
    /report could not be retained/,
  );
  assert.deepEqual(f.calls, ['close']);
  assert.equal(f.report.passed, false);
  assert.equal(f.report.browserCleanup.passed, false);
  assert.doesNotMatch(JSON.stringify(f.report), /PRIVATE_PATH/);
  const unknown = fixture();
  unknown.server.process = () => null;
  await assert.rejects(
    closeOwnedBrowser(unknown.server, unknown.report, unknown.persist, unknown.options),
    /process unavailable/,
  );
  assert.equal(canCancelFinalizationWatchdog(unknown.server, unknown.report), false);
});

test('final watchdog retains nonpassing evidence before its fatal callback, including write failure', async () => {
  for (const writeFailure of [false, true]) {
    const f = fixture();
    f.report.browserCleanup = { passed: true };
    f.report.finalization = { completed: true };
    startFinalizationWatchdog(
      f.report,
      () => {
        f.calls.push('save');
        assert.equal(f.report.passed, false);
        assert.equal(f.report.finalizationTimedOut, true);
        if (writeFailure) throw new Error('fixture write failure');
      },
      () => f.calls.push('fatal'),
      f.options,
    );
    await f.fire(25000);
    assert.deepEqual(f.calls, ['save', 'fatal']);
    assert.equal(f.report.browserCleanup.passed, false);
    assert.equal(f.report.finalization.completed, false);
  }
});

test('watchdog cancellation requires known terminal cleanup and prevents the callback', () => {
  const f = fixture();
  assert.equal(canCancelFinalizationWatchdog(null, {}), true);
  assert.equal(canCancelFinalizationWatchdog(f.server, {}), false);
  for (const exitStatusObserved of [false, true]) {
    for (const gracefulCloseCompleted of [false, true]) {
      for (const forceKillCompleted of [false, true]) {
        assert.equal(
          canCancelFinalizationWatchdog(f.server, {
            browserCleanup: { exitStatusObserved, gracefulCloseCompleted, forceKillCompleted },
          }),
          exitStatusObserved && (gracefulCloseCompleted || forceKillCompleted),
        );
      }
    }
  }
  const cancel = startFinalizationWatchdog(
    f.report,
    f.persist,
    () => f.calls.push('fatal'),
    f.options,
  );
  const timer = [...f.timers][0];
  cancel();
  cancel();
  timer.callback();
  assert.equal(f.timers.size, 0);
  assert.deepEqual(f.calls, []);
  assert.equal(f.report.passed, true);
});
