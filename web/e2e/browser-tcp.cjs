/** Fixed ICE-TCP facade; Rust's live lease exclusively owns UDP blocking. */
const journey = require('./browser-journey.cjs');
const { createChannel, statusOf } = require('./browser-impairment-channel.cjs');

async function run(options = {}) {
  const env = journey.configuration('tcp', options.env ?? process.env);
  const channel = options.channel ?? createChannel({ runId: env.BROWSER_TCP_RUN_ID, mode: 'tcp' });
  let result;
  try {
    result = await journey.run('tcp', {
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
  return journey.publish(await run(options), options, 'tcp');
}
module.exports = { run, main };
if (require.main === module) journey.cli(main, 'Browser ICE-TCP');
