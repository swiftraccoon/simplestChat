const { test } = require('../runner-fixtures.cjs');
const { run } = require('../frontend-resilience.cjs');

test('late replies and unavailable browser services preserve current intent', async ({
  suiteRuntime,
}) => {
  await run(suiteRuntime);
});
