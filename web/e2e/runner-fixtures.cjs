const fs = require('node:fs');
const path = require('node:path');
const { test: base, expect } = require('@playwright/test');
const { createBrowserOwner } = require('./runner-browser-owner.cjs');

/** The Rust harness owns PostgreSQL/application/TURN; the runner owns only its browsers. */
const test = base.extend({
  suiteRuntime: [
    async ({ browserName }, use, testInfo) => {
      const artifacts = testInfo.outputPath('suite');
      fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
      fs.chmodSync(artifacts, 0o700);
      fs.chmodSync(path.dirname(artifacts), 0o700);
      const lifecyclePath = testInfo.outputPath('browser-lifecycle.json');
      const owner = createBrowserOwner({
        browserName,
        playwright: require(process.env.PLAYWRIGHT_MODULE || 'playwright'),
        webKitTestNetwork: process.env.WEBKIT_TEST_NETWORK === '1',
        persist(records) {
          const evidence = records.map(({ browserName, launchMode, browserCleanup }) => ({
            browserName,
            launchMode,
            browserCleanup,
          }));
          fs.writeFileSync(lifecyclePath, JSON.stringify(evidence, null, 2) + '\n', {
            mode: 0o600,
          });
        },
      });
      const runtime = {
        ...owner,
        artifacts,
        env: { ...process.env, E2E_BROWSER: browserName, E2E_ARTIFACTS: artifacts },
        step: (name, work) => test.step(name, work),
      };
      let useFailed = false;
      let useError;
      try {
        await use(runtime);
      } catch (error) {
        useFailed = true;
        useError = error;
      }
      let cleanupError;
      try {
        await owner.closeAll();
      } catch (error) {
        cleanupError = error;
      }
      await testInfo.attach('owned-browser-lifecycle', {
        path: lifecyclePath,
        contentType: 'application/json',
      });
      if (cleanupError && useFailed)
        throw new AggregateError(
          [useError, cleanupError],
          'Suite and owned browser cleanup failed.',
        );
      if (cleanupError) throw cleanupError;
      if (useFailed) throw useError;
    },
    { timeout: 45_000 },
  ],
});
module.exports = { test, expect };
