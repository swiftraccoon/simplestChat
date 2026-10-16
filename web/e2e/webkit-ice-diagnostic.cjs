'use strict';

/** Native WebKit setting for the explicit, disposable ICE diagnostic only. */
async function configureWebKitIceDiagnostic(playwright, page, options = {}) {
  if (process.env.WEBKIT_ICE_DIAGNOSTIC !== '1') {
    throw new Error('WebKit ICE diagnostic requires WEBKIT_ICE_DIAGNOSTIC=1.');
  }
  if (
    process.platform !== 'darwin' ||
    options?.browserName !== 'webkit' ||
    typeof options.filteringEnabled !== 'boolean'
  ) {
    throw new Error('WebKit ICE diagnostic requires macOS WebKit and an explicit boolean setting.');
  }

  let timer;
  try {
    // This is an audited private transport, not a public Playwright API. Fail
    // closed when the package changes; do not guess another implementation.
    // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/inprocess.ts#L45-L50
    if (require('playwright/package.json').version !== '1.64.0') throw new Error();
    if (playwright !== require('playwright')) throw new Error();
    const connection = playwright._connection;
    if (
      !connection ||
      page?._connection !== connection ||
      typeof connection.toImpl !== 'function'
    ) {
      throw new Error();
    }
    const implementation = connection.toImpl(page);
    const delegate = implementation?.delegate;
    // v1.64.0's bundle preserves this class name and the owned server Page.
    // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/server/webkit/wkPage.ts#L50-L89
    if (delegate?.constructor.name !== '_WKPage' || delegate._page !== implementation) {
      throw new Error();
    }
    const session = delegate._session;
    if (typeof session?.send !== 'function' || session.isDisposed()) throw new Error();
    // Invoke after navigation, on the current owned WebProcess. The command
    // changes native candidate filtering; it does not create or inject ICE.
    // https://github.com/microsoft/playwright/blob/v1.64.0/packages/playwright-core/src/server/webkit/protocol.d.ts#L6786
    await Promise.race([
      session.send('Page.overrideSetting', {
        setting: 'ICECandidateFilteringEnabled',
        value: options.filteringEnabled,
      }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error()), 5000);
      }),
    ]);
  } catch {
    // Protocol errors may contain browser internals. Persist a fixed label.
    throw new Error('WebKit ICE diagnostic configuration failed.');
  } finally {
    clearTimeout(timer);
  }
  return {
    setting: 'ICECandidateFilteringEnabled',
    value: options.filteringEnabled,
    playwrightVersion: '1.64.0',
  };
}

module.exports = { configureWebKitIceDiagnostic };
