const { test } = require('../runner-fixtures.cjs');
const { run } = require('../community.cjs');

test('accounts, room membership and native media remain consistent', async ({ suiteRuntime }) => {
  if (suiteRuntime.env.COMMUNITY_E2E !== '1')
    throw new Error('Requires an explicitly owned Rust test-env/test-server invocation.');
  await run(suiteRuntime);
});
