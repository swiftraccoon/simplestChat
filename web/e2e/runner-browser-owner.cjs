const { closeOwnedBrowser } = require('./lifecycle-cleanup.cjs');
const { launchOwnedWebKit } = require('./runner-webkit-owner.cjs');

/** Owns only browser processes launched by this test, including failed connections. */
function createBrowserOwner({ browserName, playwright, persist, webKitTestNetwork = false }) {
  const owned = new Map();
  const lifecycle = [];
  const save = () => persist(lifecycle);
  async function closeBrowser(browser, report, retainReport) {
    const owner = owned.get(browser);
    if (!owner) return;
    const target = report ?? owner.record;
    await closeOwnedBrowser(owner.server, target, () => {
      owner.record.browserCleanup = target.browserCleanup;
      save();
      retainReport?.();
    });
    owned.delete(browser);
  }
  return {
    async launchBrowser(name, options) {
      if (name !== browserName) throw new Error('Suite browser must match its runner project.');
      const record = { browserName: name, launchMode: 'browser-server' };
      let server, browser;
      if (webKitTestNetwork) {
        if (name !== 'webkit' || playwright !== require('playwright'))
          throw new Error('Owned in-process WebKit browser configuration failed.');
        const launched = await launchOwnedWebKit(playwright, options, {
          version: require('playwright/package.json').version,
          platform: process.platform,
        });
        server = launched.server;
        browser = launched.browser;
        record.launchMode = 'in-process-webkit-test-network';
      } else {
        server = await playwright[name].launchServer(options);
      }
      lifecycle.push(record);
      owned.set(server, { server, record });
      try {
        save();
        if (!browser) browser = await playwright[name].connect(server.wsEndpoint());
        owned.delete(server);
        owned.set(browser, { server, record });
        return browser;
      } catch (error) {
        try {
          await closeBrowser(server);
        } catch (cleanupError) {
          throw new AggregateError(
            [error, cleanupError],
            'Browser connection and owned cleanup failed.',
          );
        }
        throw error;
      }
    },
    browserServer(browser) {
      const owner = owned.get(browser);
      if (!owner) throw new Error('Browser server is not owned by this test.');
      return owner.server;
    },
    closeBrowser,
    async closeAll() {
      const results = await Promise.allSettled(
        [...owned.keys()].map((browser) => closeBrowser(browser)),
      );
      save();
      const failed = results.find((result) => result.status === 'rejected');
      if (failed) throw failed.reason;
    },
  };
}
module.exports = { createBrowserOwner };
