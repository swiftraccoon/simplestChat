/** Close only this runner's BrowserServer; forced or unobserved shutdown never passes. */
async function closeOwnedBrowser(browserServer, report, persist, options = {}) {
  const schedule = options.setTimeout ?? setTimeout;
  const cancel = options.clearTimeout ?? clearTimeout;
  const cleanup = {
    completed: false,
    passed: false,
    gracefulTimeoutMs: 10000,
    forceKillTimeoutMs: 5000,
    gracefulCloseRequested: false,
    gracefulCloseCompleted: false,
    gracefulCloseTimedOut: false,
    forceKillRequested: false,
    forceKillCompleted: false,
    forceKillTimedOut: false,
    exitStatusObserved: false,
    exitCode: null,
    signalCode: null,
    errors: [],
  };
  report.browserCleanup = cleanup;
  report.passed = false;
  const recordError = (message) => {
    if (!cleanup.errors.includes(message)) cleanup.errors.push(message);
    cleanup.passed = false;
    report.passed = false;
  };
  const save = async () => {
    try {
      await persist();
    } catch {
      // Native errors may contain endpoint credentials or private paths. Keep
      // the failing stage, not their payload, and still release the owned child.
      recordError('Browser cleanup report could not be retained');
    }
  };
  const bounded = (action, milliseconds) =>
    new Promise((resolve) => {
      let settled = false;
      const finish = (outcome) => {
        if (settled) return;
        settled = true;
        cancel(timer);
        resolve(outcome);
      };
      const timer = schedule(() => finish('timed-out'), milliseconds);
      Promise.resolve()
        .then(action)
        .then(
          () => finish('completed'),
          () => finish('failed'),
        );
    });

  let ownedProcess;
  try {
    if (
      typeof browserServer?.process !== 'function' ||
      typeof browserServer.close !== 'function' ||
      typeof browserServer.kill !== 'function'
    )
      throw new Error('Unavailable owned browser');
    ownedProcess = browserServer.process();
    if (!ownedProcess || typeof ownedProcess !== 'object')
      throw new Error('Unavailable owned process');
  } catch {
    recordError('Owned browser process unavailable');
    cleanup.completed = true;
    await save();
    throw new Error(cleanup.errors.join('; '));
  }

  const observeExit = () => {
    try {
      const { exitCode, signalCode } = ownedProcess;
      const validCode = Number.isSafeInteger(exitCode) && exitCode >= 0;
      const validSignal = typeof signalCode === 'string' && /^SIG[A-Z0-9]{1,16}$/.test(signalCode);
      cleanup.exitCode = validCode ? exitCode : null;
      cleanup.signalCode = validSignal ? signalCode : null;
      cleanup.exitStatusObserved =
        (validCode && signalCode === null) || (exitCode === null && validSignal);
    } catch {
      cleanup.exitCode = null;
      cleanup.signalCode = null;
      cleanup.exitStatusObserved = false;
    }
  };

  cleanup.gracefulCloseRequested = true;
  await save();
  const graceful = await bounded(() => browserServer.close(), cleanup.gracefulTimeoutMs);
  cleanup.gracefulCloseCompleted = graceful === 'completed';
  cleanup.gracefulCloseTimedOut = graceful === 'timed-out';
  if (graceful !== 'completed')
    recordError(
      graceful === 'timed-out'
        ? 'Graceful browser close timed out'
        : 'Graceful browser close failed',
    );
  observeExit();

  if (graceful !== 'completed' || !cleanup.exitStatusObserved) {
    // Persist the failed state before escalation. Even a subsequent zero exit
    // cannot turn a forced shutdown into successful lifecycle evidence.
    recordError('Forced browser termination was required');
    cleanup.forceKillRequested = true;
    await save();
    const forced = await bounded(() => browserServer.kill(), cleanup.forceKillTimeoutMs);
    cleanup.forceKillCompleted = forced === 'completed';
    cleanup.forceKillTimedOut = forced === 'timed-out';
    if (forced !== 'completed')
      recordError(
        forced === 'timed-out'
          ? 'Forced browser cleanup timed out'
          : 'Forced browser cleanup failed',
      );
    observeExit();
  }

  if (!cleanup.exitStatusObserved) recordError('Browser exit status was not observed');
  else if (cleanup.exitCode !== 0 || cleanup.signalCode !== null)
    recordError('Browser did not exit cleanly with status zero');
  cleanup.completed = true;
  cleanup.passed = cleanup.errors.length === 0;
  await save();
  if (cleanup.errors.length) throw new Error(cleanup.errors.join('; '));
  return cleanup;
}

/** Last-resort CLI watchdog; persist must synchronously retain the failed report. */
function startFinalizationWatchdog(report, persist, onTimeout, options = {}) {
  const schedule = options.setTimeout ?? setTimeout;
  const cancel = options.clearTimeout ?? clearTimeout;
  let active = true;
  const timer = schedule(() => {
    if (!active) return;
    active = false;
    report.passed = false;
    report.finalizationTimedOut = true;
    if (report.browserCleanup) report.browserCleanup.passed = false;
    if (report.finalization) report.finalization.completed = false;
    try {
      persist();
    } catch {
      // Failure to write cannot prevent the owned CLI's terminal action.
    }
    onTimeout();
  }, 25000);
  return () => {
    if (!active) return;
    active = false;
    cancel(timer);
  };
}

function canCancelFinalizationWatchdog(browserServer, report) {
  const cleanup = report.browserCleanup;
  return (
    !browserServer ||
    Boolean(
      cleanup?.exitStatusObserved && (cleanup.gracefulCloseCompleted || cleanup.forceKillCompleted),
    )
  );
}

module.exports = { closeOwnedBrowser, startFinalizationWatchdog, canCancelFinalizationWatchdog };
