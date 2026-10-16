// Maintain one assigned GitHub issue per failing scheduled workflow. Only main
// metadata is consumed; no triggering source, artifacts, caches or logs run here.
import { readFileSync, statSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const REPOSITORY = 'swiftraccoon/simplestChat';
export const WORKFLOWS = {
  'security.yml': ['Security', ['Fast security gate', 'Scheduled policy mutation checks']],
  'infrastructure-security.yml': ['Disposable infrastructure security', ['Debian VM deployment and restore policy']],
  'performance.yml': ['Performance', ['Baseline versus candidate on one hosted runner', 'Adaptive path under an impaired downlink', 'Session soak with a database restart']],
  'browser-compatibility.yml': ['Browser compatibility', ['firefox on ubuntu-24.04', 'webkit on macos-26']],
  'codeql.yml': ['CodeQL', ['CodeQL (actions)', 'CodeQL (javascript-typescript)', 'CodeQL (python)', 'CodeQL (rust)']],
  'canary.yml': ['Canary', ['Owned browsers against the public deployment']],
  'availability.yml': ['Public availability', ['External HTTPS readiness']],
};
const FAILURES = new Set(['failure', 'timed_out', 'action_required', 'startup_failure', 'stale']);
const MAX_BYTES = 2 * 1024 * 1024;

class AlertError extends Error {}

function requireValue(condition, reason) {
  if (!condition) throw new AlertError(reason);
}

function trustedRun(run) {
  return run?.repository?.full_name === REPOSITORY &&
    run.head_repository?.full_name === REPOSITORY && run.head_branch === 'main' &&
    ['schedule', 'workflow_dispatch'].includes(run.event) &&
    Number.isSafeInteger(run.id) && run.id > 0 &&
    Number.isSafeInteger(run.run_attempt) && run.run_attempt > 0 &&
    /^[a-f0-9]{40}$/.test(run.head_sha);
}

export function workflowFor(run) {
  if (!trustedRun(run)) return undefined;
  return Object.keys(WORKFLOWS).find(file =>
    run.path === `.github/workflows/${file}` && run.name === WORKFLOWS[file][0]);
}

export function github(token, fetcher = fetch) {
  requireValue(typeof token === 'string' && token.length > 0, 'missing GitHub token');
  return async (suffix, method = 'GET', body) => {
    requireValue((method === 'GET' && /^(actions\/(runs\/\d+(\/jobs\?per_page=100)?|workflows\/[a-z-]+\.yml\/runs\?branch=main&per_page=100)|issues\?state=all&creator=github-actions%5Bbot%5D&per_page=100&page=\d+)$/.test(suffix)) ||
      (method === 'POST' && suffix === 'issues') ||
      (method === 'PATCH' && /^issues\/\d+$/.test(suffix)), 'unsupported GitHub endpoint');
    const response = await fetcher(`https://api.github.com/repos/${REPOSITORY}/${suffix}`, {
      method, redirect: 'error', signal: AbortSignal.timeout(20_000),
      headers: {
        Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28', 'Content-Type': 'application/json',
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    requireValue(response.ok, `GitHub request failed (${response.status})`);
    requireValue(response.body, 'missing GitHub response');
    const chunks = [];
    let bytes = 0;
    for await (const chunk of response.body) {
      bytes += chunk.length;
      requireValue(bytes <= MAX_BYTES, 'GitHub response too large');
      chunks.push(chunk);
    }
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  };
}

export async function reconcile(event, api) {
  if (event?.repository?.full_name !== REPOSITORY || event.action !== 'completed') return 'ignored event';
  const file = workflowFor(event.workflow_run);
  if (!file) return 'ignored run';
  let run = await api(`actions/runs/${event.workflow_run.id}`);
  requireValue(workflowFor(run) === file && run.id === event.workflow_run.id &&
    run.run_attempt === event.workflow_run.run_attempt &&
    run.head_sha === event.workflow_run.head_sha, 'run identity changed');
  if (run.status !== 'completed') return 'ignored conclusion';

  const jobCache = new Map();
  const fullSuccess = async candidate => {
    const key = `${candidate.id}:${candidate.run_attempt}`;
    if (!jobCache.has(key)) {
      const result = await api(`actions/runs/${candidate.id}/jobs?per_page=100`);
      requireValue(Array.isArray(result.jobs) && result.total_count === result.jobs.length &&
        result.jobs.length <= 100, 'incomplete job list');
      jobCache.set(key, WORKFLOWS[file][1].every(name => result.jobs.some(job => job.name === name && job.conclusion === 'success')));
    }
    return jobCache.get(key);
  };
  const listing = await api(`actions/workflows/${file}/runs?branch=main&per_page=100`);
  requireValue(Array.isArray(listing.workflow_runs), 'invalid workflow run list');
  let latest;
  for (const candidate of listing.workflow_runs) {
    if (workflowFor(candidate) !== file || candidate.status !== 'completed') continue;
    if (FAILURES.has(candidate.conclusion) ||
      (candidate.conclusion === 'success' && await fullSuccess(candidate))) {
      latest = candidate;
      break;
    }
  }
  // Diagnostic-only successes neither resolve an incident nor supersede a full
  // run that started earlier and completed later. Reconcile the latest useful
  // result even when this event was delayed or replaced in the concurrency queue.
  if (!latest) return run.conclusion === 'success' ? 'partial success' : 'ignored conclusion';
  if (latest.id !== run.id || latest.run_attempt !== run.run_attempt) {
    const current = await api(`actions/runs/${latest.id}`);
    requireValue(workflowFor(current) === file && current.id === latest.id &&
      current.run_attempt === latest.run_attempt && current.head_sha === latest.head_sha &&
      current.status === 'completed' && current.conclusion === latest.conclusion, 'latest run identity changed');
    run = current;
  }

  const marker = `<!-- scheduled-check:${file} -->`;
  let issue;
  for (let page = 1; page <= 10; page++) {
    const issues = await api(`issues?state=all&creator=github-actions%5Bbot%5D&per_page=100&page=${page}`);
    requireValue(Array.isArray(issues) && issues.length <= 100, 'invalid issue list');
    issue = issues.find(item => !item.pull_request && item.user?.login === 'github-actions[bot]' && item.body?.startsWith(`${marker}\n`));
    if (issue || issues.length < 100) break;
    requireValue(page < 10, 'issue inventory incomplete');
  }
  if (!issue && run.conclusion === 'success') return 'healthy';
  const state = run.conclusion === 'success' ? 'closed' : 'open';
  const link = `https://github.com/${REPOSITORY}/actions/runs/${run.id}/attempts/${run.run_attempt}`;
  const body = `${marker}\n\n${WORKFLOWS[file][0]} ${state === 'closed' ? 'recovered' : 'failed'} on main.\n\n` +
    `[Run ${run.id}, attempt ${run.run_attempt}](${link}) · ${run.conclusion} · commit ${run.head_sha.slice(0, 12)}\n\n` +
    'This issue tracks the latest completed scheduled or manual run. Repeated failures update this issue; a successful full run closes it. Cancelled runs and partial diagnostic successes do not resolve it.\n';
  if (issue) {
    requireValue(Number.isSafeInteger(issue.number) && issue.number > 0, 'invalid issue number');
    if (issue.body === body && issue.state === state) return 'already current';
    await api(`issues/${issue.number}`, 'PATCH', { body, state });
  } else {
    await api('issues', 'POST', { title: `[Scheduled checks] ${WORKFLOWS[file][0]}`, body, assignees: ['swiftraccoon'] });
  }
  return state === 'closed' ? 'closed recovery issue' : 'updated failure issue';
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    requireValue(process.env.GITHUB_REPOSITORY === REPOSITORY && process.env.GITHUB_EVENT_NAME === 'workflow_run', 'unsupported execution context');
    const eventPath = process.env.GITHUB_EVENT_PATH;
    requireValue(eventPath && statSync(eventPath).size <= MAX_BYTES, 'invalid event file');
    const event = JSON.parse(readFileSync(eventPath, 'utf8'));
    console.log(await reconcile(event, github(process.env.GH_TOKEN)));
  } catch (error) {
    // Do not print raw HTTP bodies, credentials, logs or source content.
    console.error(error instanceof AlertError ? error.message : 'scheduled check notification failed');
    process.exitCode = 1;
  }
}
