const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { defineConfig } = require('@playwright/test');

const artifacts = process.env.E2E_ARTIFACTS
  ? path.resolve(process.env.E2E_ARTIFACTS)
  : fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-browser-runner-'));
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
fs.chmodSync(artifacts, 0o700);
const retries = Number(process.env.E2E_RETRIES ?? 0);
if (!Number.isSafeInteger(retries) || retries < 0 || retries > 2)
  throw new Error('E2E_RETRIES must be 0, 1, or 2.');

module.exports = defineConfig({
  testDir: './specs',
  outputDir: path.join(artifacts, 'test-results'),
  fullyParallel: false,
  workers: 1,
  retries,
  forbidOnly: Boolean(process.env.CI),
  timeout: 180_000,
  expect: { timeout: 10_000 },
  use: {
    // Action parameters and network bodies can contain credentials. The safe
    // reporter retains only explicit, fixed-name scenario steps and timings.
    trace: 'off',
    screenshot: 'off',
    video: 'off',
  },
  reporter: [['./runner-reporter.cjs', { artifacts }]],
  projects: [
    ...['chromium', 'firefox', 'webkit'].map((browserName) => ({
      name: `accounts-${browserName}`,
      testMatch: 'accounts.spec.cjs',
      timeout: 180_000,
      use: { browserName },
      metadata: { group: 'accounts', services: 'external Rust xtask test-env' },
    })),
    ...['chromium', 'firefox', 'webkit'].map((browserName) => ({
      name: `media-${browserName}`,
      testMatch: 'media.spec.cjs',
      timeout: 600_000,
      use: { browserName },
      metadata: { group: 'media', services: 'external Rust xtask test-env' },
    })),
    {
      name: 'stress-chromium',
      testMatch: 'stress.spec.cjs',
      timeout: 600_000,
      use: { browserName: 'chromium' },
      metadata: { group: 'stress', services: 'external Rust xtask test-env' },
    },
    ...['chromium', 'firefox', 'webkit'].map((browserName) => ({
      name: `community-${browserName}`,
      testMatch: 'community.spec.cjs',
      timeout: 600_000,
      use: { browserName },
      metadata: { group: 'accounts-media', services: 'external Rust xtask test-env' },
    })),
    ...['chromium', 'firefox', 'webkit'].map((browserName) => ({
      name: `layout-${browserName}`,
      testMatch: 'layout.spec.cjs',
      use: { browserName },
      metadata: { group: 'layout', services: 'intercepted browser fixtures' },
    })),
    ...['chromium', 'firefox'].map((browserName) => ({
      name: `resilience-${browserName}`,
      testMatch: 'resilience.spec.cjs',
      timeout: 120_000,
      use: { browserName },
      metadata: { group: 'accounts', services: 'intercepted browser fixtures' },
    })),
  ],
});
