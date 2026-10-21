const { test } = require('../runner-fixtures.cjs');
const cases = [
  ['auth-lifecycle', 'AUTH_LIFECYCLE_E2E'],
  ['auth-cross-tab', 'AUTH_CROSS_TAB_E2E'],
  ['room-control-confirmation', 'ROOM_CONTROL_E2E'],
  ['passkey', 'PASSKEY_E2E'],
  ['passkey-management', 'PASSKEY_E2E'],
];
for (const [name, optIn] of cases) {
  test(name, async ({ suiteRuntime, browserName }) => {
    test.skip(suiteRuntime.env[optIn] !== '1', `Requires explicit ${optIn}=1.`);
    test.skip(
      name.startsWith('passkey') && browserName !== 'chromium',
      'Virtual WebAuthn authenticators require Chromium CDP.',
    );
    await require(`../${name}.cjs`).run(suiteRuntime);
  });
}
