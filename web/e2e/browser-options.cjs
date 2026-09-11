/** Options for fresh, isolated Playwright test browsers, never installed profiles. */
function browserOptions(name = 'chromium') {
  switch (name) {
    case 'chromium':
      return {
        name,
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
    case 'firefox':
      return {
        name,
        launchOptions: {
          headless: true,
          firefoxUserPrefs: {
            'media.navigator.streams.fake': true,
            'media.navigator.permission.disabled': true,
            'media.peerconnection.ice.loopback': true,
          },
        },
        // Camera/microphone context grants are not supported by this engine.
        contextOptions: {},
      };
    case 'webkit':
      // Playwright 1.63.0's macOS embedder enables mock capture in its default
      // configuration, copied into each isolated context; no custom args needed.
      // https://github.com/microsoft/playwright/blob/v1.63.0/browser_patches/webkit/embedder/Playwright/mac/AppDelegate.m#L201-L225
      // Other platforms remain blocked until their fake-capture path is audited.
      if (process.platform !== 'darwin') {
        throw new Error(
          'WebKit fake capture is verified only on macOS; use chromium or firefox on this platform.',
        );
      }
      return {
        name,
        launchOptions: { headless: true },
        contextOptions: { permissions: ['camera', 'microphone'] },
      };
    default:
      throw new Error('Unsupported E2E browser; expected chromium, firefox, or webkit.');
  }
}

module.exports = { browserOptions };
