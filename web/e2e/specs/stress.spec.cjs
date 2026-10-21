const { test, expect } = require('../runner-fixtures.cjs');
const { run } = require('../ui-stress.cjs');

test('ui-stress', async ({ suiteRuntime }) => {
  if (suiteRuntime.env.UI_STRESS_E2E !== '1') throw new Error('Requires explicit UI_STRESS_E2E=1.');
  const report = await suiteRuntime.step('bounded authenticated room workload and cleanup', () =>
    run(suiteRuntime.env, suiteRuntime),
  );
  expect(report.passed).toBe(true);
});
