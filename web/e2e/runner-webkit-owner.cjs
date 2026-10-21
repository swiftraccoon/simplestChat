const { ChildProcess } = require('node:child_process');

const failureMessage = 'Owned in-process WebKit browser configuration failed.';

/** The audited WebKit page hook needs Playwright's in-process connection.
 * These are the same browserProcess methods wrapped by launchServer in 1.64.0;
 * retain the exact ChildProcess so ordinary owned-exit checks still apply. */
async function launchOwnedWebKit(playwright, options, { version, platform }) {
  const connection = playwright?._connection;
  if (
    version !== '1.64.0' ||
    platform !== 'darwin' ||
    typeof connection?.toImpl !== 'function' ||
    typeof playwright.webkit?.launch !== 'function'
  )
    throw new Error(failureMessage);
  const browser = await playwright.webkit.launch(options);
  try {
    if (browser?._connection !== connection || browser?._type !== 'Browser')
      throw new Error(failureMessage);
    const implementation = connection.toImpl(browser);
    const browserProcess = implementation?.options?.browserProcess;
    const child = browserProcess?.process;
    if (
      implementation?.constructor?.name !== '_WKBrowser' ||
      implementation.options.name !== 'webkit' ||
      implementation.options.browserType !== 'webkit' ||
      !(child instanceof ChildProcess) ||
      !Number.isSafeInteger(child.pid) ||
      child.pid <= 0 ||
      typeof browserProcess.close !== 'function' ||
      typeof browserProcess.kill !== 'function'
    )
      throw new Error(failureMessage);
    return {
      browser,
      server: {
        process: () => child,
        close: () => browserProcess.close(),
        kill: () => browserProcess.kill(),
      },
    };
  } catch {
    // The returned browser belongs to this launch even if its unexpected
    // implementation cannot supply an auditable process. Close via the public
    // API, refuse the run, and never create a page or apply a network setting.
    try {
      await browser.close();
    } catch {
      throw new Error('Owned in-process WebKit browser cleanup failed.');
    }
    throw new Error(failureMessage);
  }
}

module.exports = { launchOwnedWebKit };
