const { spawnSync } = require('node:child_process');
const path = require('node:path');

const suites = {
  community: { group: 'community', optIn: 'COMMUNITY_E2E' },
  layout: { group: 'layout' },
  resilience: { group: 'resilience' },
  'auth-lifecycle': { group: 'accounts', optIn: 'AUTH_LIFECYCLE_E2E' },
  'auth-cross-tab': { group: 'accounts', optIn: 'AUTH_CROSS_TAB_E2E' },
  'room-control-confirmation': { group: 'accounts', optIn: 'ROOM_CONTROL_E2E' },
  passkey: { group: 'accounts', optIn: 'PASSKEY_E2E' },
  'passkey-management': { group: 'accounts', optIn: 'PASSKEY_E2E' },
  'media-continuity': { group: 'media', optIn: 'MEDIA_CONTINUITY_E2E' },
  'chat-retry': { group: 'media', optIn: 'CHAT_RETRY_E2E' },
  presentation: { group: 'media', optIn: 'PRESENTATION_E2E' },
  'impaired-network': { group: 'media', optIn: 'CALL_OUTCOME_E2E' },
  accessibility: { group: 'media', optIn: 'ACCESSIBILITY_E2E' },
  'ui-stress': { group: 'stress', optIn: 'UI_STRESS_E2E' },
};
/** Existing CLI entrypoints invoke the same runner projects as npm and CI. */
function run(name, extra = process.argv.slice(2)) {
  const suite = suites[name];
  if (!suite) throw new Error('Unknown maintained browser suite.');
  const listing = extra.includes('--list');
  if (suite.optIn && process.env[suite.optIn] !== '1' && !listing)
    throw new Error(`Set ${suite.optIn}=1 against explicitly owned disposable test services.`);
  const browser = process.env.E2E_BROWSER || 'chromium';
  if (!['chromium', 'firefox', 'webkit'].includes(browser))
    throw new Error('Unsupported E2E browser.');
  if ((name.startsWith('passkey') || suite.group === 'stress') && browser !== 'chromium')
    throw new Error('This suite requires Chromium.');
  const args = [
    require.resolve('@playwright/test/cli'),
    'test',
    '--config',
    path.join(__dirname, 'playwright.config.cjs'),
    '--project',
    `${suite.group}-${browser}`,
  ];
  if (['accounts', 'media', 'stress'].includes(suite.group)) args.push('--grep', `${name}$`);
  args.push(...extra);
  const result = spawnSync(process.execPath, args, { stdio: 'inherit', env: process.env });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
}
module.exports = { run, suites };
if (require.main === module) {
  const [name, ...extra] = process.argv.slice(2);
  try {
    run(name, extra);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
