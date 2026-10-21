const { test } = require('../runner-fixtures.cjs');
const { run } = require('../homepage-layout.cjs');

test('home directory and account layouts fit all maintained viewports', async ({
  suiteRuntime,
}) => {
  await run(suiteRuntime.env, suiteRuntime);
});
