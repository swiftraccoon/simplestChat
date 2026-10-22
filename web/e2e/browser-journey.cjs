/**
 * Fixed Chromium mechanics/DTX, adaptive and ICE-TCP journeys. Rust owns the server, this
 * CLI group, stdout capture and receipt publication; this adapter owns only the
 * BrowserServers admitted through createBrowserOwner. Runtime modules and the
 * existing suite's private artifact path remain caller-selected trust inputs.
 */
const path = require('node:path');
const { createBrowserOwner } = require('./runner-browser-owner.cjs');
const { run: impaired } = require('./impaired-network.cjs');

const LIMIT = 16 * 1024;
const MECHANICS = {
  BASE_URL: 'http://127.0.0.1:3109',
  E2E_BROWSER: 'chromium',
  IMPAIR_SCRIPT: 'none',
  IMPAIRED_PROFILES: 'baseline,silentAudio',
  IMPAIRED_SILENT_AUDIO: '1',
  IMPAIR_UDP_PORT: '41100',
  IMPAIR_WORKERS: '1',
  IMPAIR_LOSS: '0',
  IMPAIR_DELAY_MS: '0',
  IMPAIR_JITTER_MS: '0',
  IMPAIR_RATE_KBIT: '0',
  IMPAIR_DIRECTION: 'downlink',
  TEST_BROWSER_HTTPS: '0',
};
const ADAPTIVE = {
  ...MECHANICS,
  IMPAIR_SCRIPT: 'rust-owned',
  IMPAIRED_PROFILES: 'baseline,lossy,constrained,severe,recovery,lossyJoin',
  IMPAIRED_SILENT_AUDIO: '0',
};
const TCP = {
  ...MECHANICS,
  IMPAIR_SCRIPT: 'rust-owned',
  IMPAIRED_PROFILES: 'baseline',
  IMPAIRED_SILENT_AUDIO: '0',
  IMPAIRED_BLOCK_UDP: '1',
  WEBRTC_SERVER_TCP: 'true',
};
const CONFIGURATIONS = { mechanics: MECHANICS, adaptive: ADAPTIVE, tcp: TCP };
const RUN_IDS = {
  mechanics: 'BROWSER_MECHANICS_RUN_ID',
  adaptive: 'BROWSER_ADAPTIVE_RUN_ID',
  tcp: 'BROWSER_TCP_RUN_ID',
};
const LABELS = {
  mechanics: 'Browser mechanics',
  adaptive: 'Browser adaptive',
  tcp: 'Browser ICE-TCP',
};
const FORBIDDEN = [
  'CANARY_MODE',
  'CANARY_FORCE_RELAY',
  'CANARY_CORRELATION_FILE',
  'CALL_OUTCOME_E2E',
  'IMPAIRED_BLOCK_UDP',
  'IMPAIRED_ROOM',
  'WEBKIT_TEST_NETWORK',
  'NODE_OPTIONS',
];
function configuration(kind, input) {
  if (!Object.hasOwn(CONFIGURATIONS, kind))
    throw new Error('Browser journey configuration is invalid.');
  const fixed = CONFIGURATIONS[kind];
  const runIdKey = RUN_IDS[kind];
  const env = { ...input };
  for (const name of FORBIDDEN)
    if (
      !(kind === 'tcp' && name === 'IMPAIRED_BLOCK_UDP') &&
      env[name] !== undefined &&
      env[name] !== ''
    )
      throw new Error('Browser mechanics configuration is invalid.');
  for (const [name, value] of Object.entries(fixed))
    if (env[name] !== undefined && env[name] !== '' && env[name] !== value)
      throw new Error('Browser mechanics configuration is invalid.');
  if (
    typeof env[runIdKey] !== 'string' ||
    !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(env[runIdKey]) ||
    typeof env.E2E_ARTIFACTS !== 'string' ||
    !path.isAbsolute(env.E2E_ARTIFACTS) ||
    path.resolve(env.E2E_ARTIFACTS) !== env.E2E_ARTIFACTS ||
    Buffer.byteLength(env.E2E_ARTIFACTS) > 4096 ||
    Buffer.from(env.E2E_ARTIFACTS).some((byte) => byte < 32 || byte === 127)
  )
    throw new Error('Browser mechanics configuration is invalid.');
  return Object.freeze({ ...env, ...fixed });
}
function project(record) {
  const cleanup = record.browserCleanup;
  return {
    browserName: record.browserName === 'chromium' ? 'chromium' : null,
    launchMode: record.launchMode === 'browser-server' ? 'browser-server' : null,
    completed: cleanup?.completed === true,
    passed: cleanup?.passed === true,
    gracefulCloseCompleted: cleanup?.gracefulCloseCompleted === true,
    forceKillRequested: cleanup?.forceKillRequested === true,
    exitStatusObserved: cleanup?.exitStatusObserved === true,
    exitCode:
      Number.isSafeInteger(cleanup?.exitCode) && cleanup.exitCode >= 0 ? cleanup.exitCode : null,
    signalCode:
      typeof cleanup?.signalCode === 'string' && /^SIG[A-Z0-9]{1,16}$/.test(cleanup.signalCode)
        ? cleanup.signalCode
        : null,
  };
}
function validRecord(record) {
  const cleanup = record.browserCleanup;
  // Unknown negative evidence must not become an observed false/null merely
  // because the private projection drops malformed native fields.
  return (
    record.browserName === 'chromium' &&
    record.launchMode === 'browser-server' &&
    [
      'completed',
      'passed',
      'gracefulCloseCompleted',
      'forceKillRequested',
      'exitStatusObserved',
    ].every((key) => typeof cleanup?.[key] === 'boolean') &&
    (cleanup.exitCode === null ||
      (Number.isSafeInteger(cleanup.exitCode) && cleanup.exitCode >= 0)) &&
    (cleanup.signalCode === null ||
      (typeof cleanup.signalCode === 'string' && /^SIG[A-Z0-9]{1,16}$/.test(cleanup.signalCode)))
  );
}
function clean(record) {
  return (
    record.browserName === 'chromium' &&
    record.launchMode === 'browser-server' &&
    record.completed &&
    record.passed &&
    record.gracefulCloseCompleted &&
    !record.forceKillRequested &&
    record.exitStatusObserved &&
    record.exitCode === 0 &&
    record.signalCode === null
  );
}

/**
 * Capture the environment once and run the fixed journey with one browser owner.
 * Signals stop admission and memoize retirement after all already-admitted
 * launches/closes settle. A hung runtime remains subject to the Rust watchdog;
 * this function does not claim to cancel an arbitrary Playwright promise.
 * Native errors stay in the returned cause, never in the public receipt.
 */
async function run(
  kind,
  {
    env = process.env,
    suite = impaired,
    createOwner = createBrowserOwner,
    playwright,
    signals = process,
    impair,
    onCancel,
    statusOf = () => 1,
  } = {},
) {
  const captured = configuration(kind, env);
  if (kind !== 'mechanics' && typeof impair !== 'function')
    throw new Error(`${LABELS[kind]} control is unavailable.`);
  let owner;
  let records = [];
  let recordsValid = false;
  let cancelled = null;
  let cause;
  let status = 0;
  let phasePassed = false;
  let cleanupPassed = false;
  let retirement;
  let launchCount = 0;
  const pending = new Set();
  const closes = new Map();
  const fail = (error, code = statusOf(error)) => {
    if (status !== 0) return;
    status = code;
    cause = error;
  };
  const track = (work) => {
    pending.add(work);
    // The caller observes the original promise; this boundary handles the
    // bookkeeping promise so an early signal cannot create a detached rejection.
    work.then(
      () => pending.delete(work),
      (error) => {
        pending.delete(work);
        fail(error);
      },
    );
    return work;
  };
  const retire = () => {
    retirement ??= (async () => {
      // Admission is closed before this snapshot. An in-flight launch may
      // acquire its BrowserServer only after a signal has already arrived.
      const settled = await Promise.allSettled([...pending]);
      for (const result of settled) if (result.status === 'rejected') fail(result.reason);
      if (owner) await owner.closeAll();
      cleanupPassed = true;
    })();
    return retirement;
  };
  const interrupt = (code) => {
    cancelled ??= code;
    fail(new Error('Browser mechanics interrupted.'), code);
    try {
      onCancel?.(code);
    } catch (error) {
      fail(error);
    }
    retire().catch((error) => fail(error));
  };
  const int = () => interrupt(130);
  const term = () => interrupt(143);
  signals.on('SIGINT', int);
  signals.on('SIGTERM', term);
  try {
    owner = createOwner({
      browserName: 'chromium',
      playwright: playwright ?? require(captured.PLAYWRIGHT_MODULE || 'playwright'),
      persist(value) {
        if (!Array.isArray(value) || value.length > 1)
          throw new Error('Browser mechanics lifecycle is invalid.');
        recordsValid = value.every(validRecord);
        records = value.map(project);
      },
    });
    if (cancelled !== null) throw new Error('Browser mechanics interrupted.');
    await suite({
      env: captured,
      artifacts: captured.E2E_ARTIFACTS,
      ...(kind !== 'mechanics' ? { impair } : {}),
      launchBrowser(name, options) {
        if (retirement || cancelled !== null || name !== 'chromium' || launchCount !== 0)
          return Promise.reject(new Error('Browser mechanics admission is closed.'));
        launchCount++;
        return track(Promise.resolve().then(() => owner.launchBrowser(name, options)));
      },
      closeBrowser(browser) {
        if (retirement) return retirement;
        if (!closes.has(browser))
          closes.set(browser, track(Promise.resolve().then(() => owner.closeBrowser(browser))));
        return closes.get(browser);
      },
    });
    phasePassed = true;
  } catch (error) {
    fail(error);
  } finally {
    try {
      await retire();
    } catch (error) {
      fail(error);
    }
    signals.removeListener('SIGINT', int);
    signals.removeListener('SIGTERM', term);
  }
  const browserCleanupPassed =
    cleanupPassed && recordsValid && records.length === 1 && records.every(clean);
  if (!phasePassed || !browserCleanupPassed) fail(new Error('Browser mechanics failed.'));
  const receipt = {
    schemaVersion: 1,
    runId: captured[RUN_IDS[kind]],
    completed: true,
    phasePassed,
    browserCleanupPassed,
    cancelled,
    browserCount: records.length,
    records,
  };
  if (Buffer.byteLength(JSON.stringify(receipt)) > LIMIT)
    throw new Error('Browser mechanics lifecycle is invalid.');
  return { status, receipt, cause };
}

async function write(stream, bytes) {
  await new Promise((resolve, reject) => {
    // Keep the error listener for the owned CLI's remaining lifetime: a stream
    // can emit error after invoking its write callback with that same failure.
    stream.on('error', () => {});
    stream.write(bytes, (error) => (error ? reject(error) : resolve()));
  });
}

/** Emit only the bounded receipt; scenario progress belongs to private stderr. */
async function main(kind, options = {}) {
  return publish(await run(kind, options), options, kind);
}
async function publish(result, options = {}, kind = 'mechanics') {
  const stdout = options.stdout ?? process.stdout;
  const stderr = options.stderr ?? process.stderr;
  try {
    await write(stdout, `${JSON.stringify(result.receipt)}\n`);
    if (result.status !== 0) await write(stderr, `${LABELS[kind] ?? LABELS.mechanics} failed.\n`);
  } catch {
    if (result.status === 0) throw new Error('Browser mechanics publication failed.');
  }
  return result.status;
}
function cli(entry, label = 'Browser mechanics') {
  console.log = (...values) => console.error(...values);
  console.info = (...values) => console.error(...values);
  console.debug = (...values) => console.error(...values);
  if (process.argv.length !== 2) {
    console.error(`${label} arguments are invalid.`);
    process.exitCode = 2;
  } else {
    entry().then(
      (status) => (process.exitCode = status),
      () => {
        console.error(`${label} failed.`);
        process.exitCode = 1;
      },
    );
  }
}

module.exports = { run, main, publish, cli, configuration };
