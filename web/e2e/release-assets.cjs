const assert = require('node:assert/strict');
const { execFile } = require('node:child_process');
const { readFile, writeFile } = require('node:fs/promises');
const path = require('node:path');
const { promisify } = require('node:util');

const revisionPattern = /^[a-f0-9]{40}$/;

class RolloverError extends Error {
  constructor(stage) {
    super('Release asset rollover check failed');
    this.stage = stage;
  }
}

async function atStage(stage, work) {
  try {
    return await work();
  } catch {
    throw new RolloverError(stage);
  }
}

async function checkRollover(driver, original, candidate) {
  await atStage('configuration', () => {
    assert(revisionPattern.test(original) && revisionPattern.test(candidate));
    assert.notEqual(original, candidate);
  });
  const oldTab = await atStage('open-original', () => driver.open(original));
  await atStage('deploy-candidate', () => driver.deploy(candidate));
  await atStage('load-old', () => oldTab.loadAfterReplacement());
  const newTab = await atStage('open-candidate', () => driver.open(candidate));
  await atStage('rollback', () => driver.deploy(original));
  await atStage('load-new', () => newTab.loadAfterReplacement());
  await atStage('identity', async () => {
    await oldTab.unchanged();
    await newTab.unchanged();
  });
  return {
    passed: true,
    editions: 2,
    oldTabAfterUpgrade: true,
    newTabAfterRollback: true,
    backendMissesVerified: 2,
    documentsUnchanged: true,
    captureCalls: 0,
    htmlUncached: true,
    selectedVersionVerified: true,
  };
}

function denyCapture() {
  globalThis.__releaseCaptureCalls = 0;
  const reject = () => {
    globalThis.__releaseCaptureCalls += 1;
    return Promise.reject(
      new DOMException('Capture is forbidden in release checks', 'NotAllowedError'),
    );
  };
  if (navigator.mediaDevices) {
    navigator.mediaDevices.getUserMedia = reject;
    navigator.mediaDevices.getDisplayMedia = reject;
  }
}

async function browserDriver(browser, execute = promisify(execFile)) {
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  await context.addInitScript(denyCapture);
  const selectedVersion = async (revision) => {
    const selected = await context.request.get('https://localhost/app-version.json');
    assert.equal(selected.status(), 200);
    assert.match(selected.headers()['cache-control'] || '', /no-store/);
    assert.deepEqual(await selected.json(), { revision });
  };
  const deploy = async (revision) => {
    assert(revisionPattern.test(revision));
    const { stdout } = await execute(
      '/usr/bin/python3',
      [
        '-B',
        path.resolve(__dirname, '../../ops/ansible/files/release-public.py'),
        'deploy',
        revision,
      ],
      { timeout: 150_000, maxBuffer: 2 * 1024 * 1024, killSignal: 'SIGTERM', windowsHide: true },
    );
    const result = JSON.parse(stdout.trim());
    assert.equal(result.passed, true);
    assert.equal(result.revision, revision);
    await selectedVersion(revision);
  };
  return {
    deploy,
    async open(revision) {
      const page = await context.newPage();
      const response = await page.goto('https://localhost/asset-rollover.html');
      assert(response && response.ok());
      assert.match(response.headers()['cache-control'] || '', /no-store/);
      const document = await page.evaluate(() => ({
        revision: globalThis.releaseRollover.revision,
        identity: globalThis.releaseRollover.identity,
        chunk: globalThis.releaseRollover.chunk,
      }));
      assert.equal(document.revision, revision);
      assert.match(document.chunk, /^\/assets\/rollover-[a-f0-9]{16}\.js$/);
      await selectedVersion(revision);
      const unchanged = async () => {
        const state = await page.evaluate(() => ({
          identity: globalThis.releaseRollover.identity,
          revision: globalThis.releaseRollover.revision,
          captureCalls: globalThis.__releaseCaptureCalls,
        }));
        assert.equal(state.identity, document.identity);
        assert.equal(state.revision, revision);
        assert.equal(state.captureCalls, 0);
      };
      return {
        unchanged,
        async loadAfterReplacement() {
          // The selected app has no retained store. Prove that delivery came
          // through the stable proxy after the actual backend returned 404.
          const miss = await fetch('http://127.0.0.1:3000' + document.chunk, {
            redirect: 'error',
            signal: AbortSignal.timeout(5000),
          });
          try {
            assert.equal(miss.status, 404);
          } finally {
            await miss.body?.cancel();
          }
          const [received, loaded] = await Promise.all([
            page.waitForResponse((result) => new URL(result.url()).pathname === document.chunk, {
              timeout: 10_000,
            }),
            page.evaluate(() => globalThis.releaseRollover.load()),
          ]);
          assert.equal(loaded, revision);
          assert.equal(received.status(), 200);
          await unchanged();
        },
      };
    },
    close: () => context.close(),
  };
}

async function closeBrowser(browser, driver) {
  const results = await Promise.allSettled([
    Promise.resolve().then(() => driver?.close()),
    Promise.resolve().then(() => browser.close()),
  ]);
  if (results.some((result) => result.status === 'rejected')) throw new RolloverError('cleanup');
}

async function main() {
  const [configurationFile, chromiumExecutable, output] = process.argv.slice(2);
  assert(configurationFile && chromiumExecutable && output);
  assert(
    path.isAbsolute(configurationFile) &&
      path.isAbsolute(chromiumExecutable) &&
      path.isAbsolute(output),
  );
  const configuration = JSON.parse(await readFile(configurationFile, 'utf8'));
  assert.deepEqual(Object.keys(configuration).sort(), ['candidate', 'original']);
  const { chromium } = require('playwright');
  const browser = await chromium.launch({ executablePath: chromiumExecutable, headless: true });
  let driver;
  const failures = [];
  try {
    driver = await browserDriver(browser);
    const report = await checkRollover(driver, configuration.original, configuration.candidate);
    await writeFile(output, JSON.stringify(report) + '\n', { flag: 'wx', mode: 0o600 });
  } catch (error) {
    failures.push(error);
  }
  try {
    await closeBrowser(browser, driver);
  } catch (error) {
    failures.push(error);
  }
  if (failures.length) throw failures[0];
}

module.exports = { checkRollover, denyCapture, browserDriver, closeBrowser, RolloverError };
if (require.main === module) {
  main().catch((error) => {
    // Raw child/browser errors can contain private command output. Only the
    // fixed stage enters the controller's private diagnostics.
    const stage = error instanceof RolloverError ? error.stage : 'setup-or-cleanup';
    process.stderr.write(`Release asset rollover browser check failed: ${stage}\n`);
    process.exitCode = 1;
  });
}
