import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';
import helper from '../../web/e2e/browser-options.cjs';

const { browserOptions } = helper;
const source = await readFile(new URL('../../web/e2e/browser-options.cjs', import.meta.url), 'utf8');

// Test platform restrictions without changing the host process or launching any
// browser. The helper intentionally requires neither Playwright nor a profile.
function forPlatform(platform, arch = 'arm64') {
  const module = { exports: {} };
  vm.runInNewContext(source, { module, process: { platform, arch } }, { timeout: 1000 });
  return module.exports.browserOptions;
}

const plain = value => JSON.parse(JSON.stringify(value));

test('browser options default to headless Chromium with fake capture and loopback only', () => {
  const expected = {
    name: 'chromium',
    launchOptions: {
      headless: true,
      args: [
        '--use-fake-device-for-media-stream',
        '--use-fake-ui-for-media-stream',
        '--allow-loopback-in-peer-connection',
      ],
    },
    contextOptions: { permissions: ['camera', 'microphone'] },
  };
  assert.deepEqual(browserOptions(), expected);
  assert.deepEqual(browserOptions(undefined), expected);
  assert.deepEqual(browserOptions('chromium'), expected);
});

test('browser options reject every non-allowlisted name without loading an engine', () => {
  for (const invalid of ['', 'Chromium', ' firefox', 'webkit ', 'chrome', 'safari',
    'constructor', '__proto__', 'toString', null, false, 0, [], {}, new String('chromium'), Symbol('chromium')]) {
    assert.throws(() => browserOptions(invalid), /Unsupported E2E browser; expected chromium, firefox, or webkit/);
  }
});

test('Firefox uses its own fake-media preferences without Chromium arguments or context grants', () => {
  assert.deepEqual(browserOptions('firefox'), {
    name: 'firefox',
    launchOptions: {
      headless: true,
      firefoxUserPrefs: {
        'media.navigator.streams.fake': true,
        'media.navigator.permission.disabled': true,
        'media.peerconnection.ice.loopback': true,
      },
    },
    contextOptions: {},
  });
});

test('Firefox preserves host-address privacy settings on every platform', () => {
  const preference = 'media.peerconnection.ice.obfuscate_host_addresses';
  for (const [platform, arch] of [['darwin', 'arm64'], ['darwin', 'x64'], ['linux', 'arm64'], ['linux', 'x64'], ['win32', 'arm64'], ['win32', 'x64']]) {
    const preferences = forPlatform(platform, arch)('firefox').launchOptions.firefoxUserPrefs;
    assert.equal(Object.hasOwn(preferences, preference), false, `${platform}/${arch} must retain its default host-address policy`);
    assert.deepEqual(plain(preferences), {
      'media.navigator.streams.fake': true,
      'media.navigator.permission.disabled': true,
      'media.peerconnection.ice.loopback': true,
    });
  }
  for (const name of ['chromium', 'webkit']) {
    assert.doesNotMatch(JSON.stringify(forPlatform('darwin', 'arm64')(name)), /obfuscate_host_addresses/);
  }
});

test('WebKit uses verified macOS mock-capture defaults and blocks unaudited platforms', () => {
  assert.deepEqual(plain(forPlatform('darwin')('webkit')), {
    name: 'webkit',
    launchOptions: { headless: true },
    contextOptions: { permissions: ['camera', 'microphone'] },
  });
  for (const platform of ['linux', 'win32', 'freebsd']) {
    assert.throws(() => forPlatform(platform)('webkit'), /WebKit fake capture is verified only on macOS/);
    assert.equal(forPlatform(platform)('chromium').name, 'chromium');
    assert.equal(forPlatform(platform)('firefox').name, 'firefox');
  }
});

test('engine options do not leak across calls or share mutable nested objects', () => {
  const options = forPlatform('darwin');
  const baseline = Object.fromEntries(['chromium', 'firefox', 'webkit'].map(name => [name, plain(options(name))]));
  for (const name of Object.keys(baseline)) {
    const changed = options(name);
    changed.launchOptions.headless = false;
    changed.launchOptions.args?.push('--fixture-only');
    if (changed.launchOptions.firefoxUserPrefs) changed.launchOptions.firefoxUserPrefs['media.navigator.streams.fake'] = false;
    changed.contextOptions.permissions?.push('geolocation');
    changed.contextOptions.fixtureOnly = true;
    for (const engine of Object.keys(baseline)) assert.deepEqual(plain(options(engine)), baseline[engine]);
  }
});

test('no engine options bypass autoplay policy or select an installed browser profile', () => {
  const options = forPlatform('darwin');
  for (const name of ['chromium', 'firefox', 'webkit']) {
    const value = options(name);
    assert.doesNotMatch(JSON.stringify(value), /autoplay|no-user-gesture|required-user-gesture|userDataDir|executablePath|channel|storageState/i);
  }
});

test('WebKit mock-capture assumptions stay tied to the audited Playwright pin', async () => {
  const specification = JSON.parse(await readFile(new URL('../../web/e2e/package.json', import.meta.url), 'utf8'));
  const lock = JSON.parse(await readFile(new URL('../../web/e2e/package-lock.json', import.meta.url), 'utf8'));
  const message = 'Re-audit bundled WebKit fake capture before changing the Playwright pin.';
  assert.equal(specification.devDependencies.playwright, '1.63.0', message);
  assert.equal(lock.packages['node_modules/playwright-core'].version, '1.63.0', message);
});
