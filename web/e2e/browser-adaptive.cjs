/** Linux adaptive facade; only Rust's live lease can acknowledge impairment. */
const journey = require('./browser-journey.cjs');
const { createChannel, statusOf } = require('./browser-impairment-channel.cjs');

async function run(options = {}) {
  const env = journey.configuration('adaptive', options.env ?? process.env);
  const channel = options.channel ?? createChannel({ runId: env.BROWSER_ADAPTIVE_RUN_ID });
  let result;
  try {
    result = await journey.run('adaptive', {
      ...options,
      env,
      impair: (...values) => channel.impair(...values),
      onCancel: (code) => channel.cancel(code),
      statusOf,
    });
  } finally {
    try {
      await channel.finish();
    } catch (error) {
      if (result?.status === 0) {
        result.status = statusOf(error);
        result.cause = error;
        result.receipt.phasePassed = false;
      }
    }
  }
  return result;
}
async function main(options = {}) {
  return journey.publish(await run(options), options, 'adaptive');
}
module.exports = { run, main };
if (require.main === module) journey.cli(main, 'Browser adaptive');
