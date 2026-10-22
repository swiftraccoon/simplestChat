/** Fixed mechanics/DTX facade over the shared browser ownership lifecycle. */
const journey = require('./browser-journey.cjs');
const run = (options) => journey.run('mechanics', options);
const main = (options) => journey.main('mechanics', options);
module.exports = { run, main };
if (require.main === module) journey.cli(main);
