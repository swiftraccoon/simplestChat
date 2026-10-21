const { test } = require('../runner-fixtures.cjs');
const cases = [
  ['media-continuity', 'MEDIA_CONTINUITY_E2E'],
  ['chat-retry', 'CHAT_RETRY_E2E'],
  ['presentation', 'PRESENTATION_E2E'],
  ['impaired-network', 'CALL_OUTCOME_E2E'],
  ['accessibility', 'ACCESSIBILITY_E2E'],
];
for (const [name, optIn] of cases) {
  test(name, async ({ suiteRuntime }) => {
    test.skip(suiteRuntime.env[optIn] !== '1', `Requires explicit ${optIn}=1.`);
    if (name === 'accessibility')
      await require('../accessibility.cjs').run(suiteRuntime.env, suiteRuntime);
    else await require(`../${name}.cjs`).run(suiteRuntime);
  });
}
