'use strict';

const failureMessage = 'Owned WebKit test network configuration failed.';

/** Native networking configuration for owned, disposable macOS WebKit pages. */
async function installWebKitTestNetwork(playwright, page, { browserName } = {}) {
  if (process.env.WEBKIT_TEST_NETWORK !== '1') {
    throw new Error('Owned WebKit test networking requires WEBKIT_TEST_NETWORK=1.');
  }
  if (process.platform !== 'darwin' || browserName !== 'webkit') {
    throw new Error('Owned WebKit test networking requires macOS WebKit.');
  }

  let delegate, original, context;
  try {
    // The private in-process transport is audited against this exact pin.
    // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/inprocess.ts#L45-L50
    if (
      require('playwright/package.json').version !== '1.64.0' ||
      playwright !== require('playwright')
    )
      throw new Error();
    const connection = playwright._connection;
    if (page?._connection !== connection || typeof connection?.toImpl !== 'function')
      throw new Error();
    const implementation = connection.toImpl(page);
    delegate = implementation?.delegate;
    if (delegate?.constructor.name !== '_WKPage' || delegate._page !== implementation)
      throw new Error();
    original = delegate._initializeSessionMayThrow;
    if (typeof original !== 'function' || Object.hasOwn(delegate, '_initializeSessionMayThrow'))
      throw new Error();
    context = page.context();
    if (typeof context?.close !== 'function') throw new Error();
  } catch {
    throw new Error(failureMessage);
  }

  let count = 0,
    failed = false,
    disposed = false,
    closing;
  const workBySession = new WeakMap();
  const pending = new Set();
  async function deadline(work) {
    let timer;
    try {
      return await Promise.race([
        work,
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error(failureMessage)), 5000);
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }
  async function closeFailedContext() {
    // Upstream may suppress errors from a provisional session. Close its owned
    // context before returning control, so an unconfigured target cannot run.
    if (!closing) {
      closing = deadline(
        Promise.resolve().then(() => context.close({ reason: failureMessage })),
      ).catch(() => {});
    }
    await closing;
  }
  function configure(session) {
    if (failed || disposed) return Promise.reject(new Error(failureMessage));
    const previous = session && workBySession.get(session);
    if (previous) return previous;
    const work = (async () => {
      try {
        if (typeof session?.send !== 'function' || session.isDisposed()) throw new Error();
        await deadline(
          session.send('Page.overrideSetting', {
            setting: 'ICECandidateFilteringEnabled',
            value: false,
          }),
        );
        count++;
      } catch {
        // A discarded provisional process is part of ordinary navigation.
        // Its paused target is already gone; retain upstream disposal handling.
        if (typeof session?.isDisposed === 'function' && session.isDisposed()) return;
        failed = true;
        await closeFailedContext();
        throw new Error(failureMessage);
      }
    })();
    if (session && typeof session === 'object') workBySession.set(session, work);
    return work;
  }
  function track(work) {
    pending.add(work);
    work.then(
      () => pending.delete(work),
      () => pending.delete(work),
    );
    return work;
  }

  // Every future WebProcess is initialized while paused. Playwright awaits this
  // method before Target.resume; patch only this page instance, never a prototype.
  // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/server/webkit/wkPage.ts#L164-L244
  // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/server/webkit/wkPage.ts#L329-L365
  const initialize = function (...args) {
    const session = args[0];
    return track(
      (async () => {
        try {
          await original.apply(this, args);
          if (session.isDisposed()) return;
          await configure(session);
        } catch {
          if (!failed && typeof session?.isDisposed === 'function' && session.isDisposed()) return;
          failed = true;
          await closeFailedContext();
          throw new Error(failureMessage);
        }
      })(),
    );
  };
  delegate._initializeSessionMayThrow = initialize;
  const controller = {
    setting: 'ICECandidateFilteringEnabled',
    value: false,
    playwrightVersion: '1.64.0',
    appliedCount: () => count,
    async ready() {
      if (failed || disposed || delegate._initializeSessionMayThrow !== initialize)
        throw new Error(failureMessage);
      try {
        while (pending.size) await deadline(Promise.all([...pending]));
        if (delegate._session.isDisposed()) throw new Error();
        await configure(delegate._session);
        if (delegate._session.isDisposed()) throw new Error();
      } catch {
        throw new Error(failureMessage);
      }
      if (failed) throw new Error(failureMessage);
    },
    async dispose() {
      if (disposed) return;
      try {
        while (pending.size) await deadline(Promise.all([...pending]));
      } catch {
        failed = true;
      }
      disposed = true;
      if (delegate._initializeSessionMayThrow === initialize)
        delete delegate._initializeSessionMayThrow;
      if (failed) throw new Error(failureMessage);
    },
  };
  try {
    await controller.ready();
  } catch {
    await controller.dispose().catch(() => {});
    throw new Error(failureMessage);
  }
  return controller;
}

module.exports = { installWebKitTestNetwork };
