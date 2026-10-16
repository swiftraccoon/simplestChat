import assert from 'node:assert/strict';
import test from 'node:test';
import { github, reconcile, workflowFor, WORKFLOWS } from '../scheduled-check-alerts.mjs';

const repository = { full_name: 'swiftraccoon/simplestChat' };
const base = {
  id: 123, run_attempt: 1, name: 'Security', path: '.github/workflows/security.yml',
  head_branch: 'main', head_sha: 'a'.repeat(40), repository, head_repository: repository,
  event: 'schedule', status: 'completed', conclusion: 'failure',
};
const event = run => ({ repository, action: 'completed', workflow_run: run });
const marker = '<!-- scheduled-check:security.yml -->';
const issue = { number: 19, state: 'open', user: { login: 'github-actions[bot]' }, body: `${marker}\nold failure` };

function fixture({ run = base, latest = run, issues = [], jobs, jobMap = {}, firstPage } = {}) {
  const calls = [];
  const api = async (suffix, method = 'GET', body) => {
    calls.push({ suffix, method, body });
    if (method !== 'GET') return {};
    if (suffix === `actions/runs/${base.id}`) return run;
    const current = (Array.isArray(latest) ? latest : [latest]).find(item => suffix === `actions/runs/${item.id}`);
    if (current) return current;
    if (suffix.includes('/runs?branch=')) return { workflow_runs: Array.isArray(latest) ? latest : [latest] };
    if (suffix.endsWith('/jobs?per_page=100')) {
      const all = jobMap[suffix] ?? jobs ?? WORKFLOWS['security.yml'][1].map(name => ({ name, conclusion: 'success' }));
      return { jobs: all, total_count: all.length };
    }
    if (suffix.endsWith('&page=1') && firstPage) return firstPage;
    if (suffix.startsWith('issues?')) return issues;
    throw new Error(`Unexpected fixture endpoint: ${suffix}`);
  };
  return { calls, api, mutations: () => calls.filter(call => call.method !== 'GET') };
}

test('reject forks, PRs, non-main, unknown workflows and malformed run identities before API access', async () => {
  const changes = [
    { repository: { full_name: 'other/repo' } }, { head_repository: { full_name: 'fork/repo' } },
    { event: 'pull_request' }, { event: 'push' }, { head_branch: 'feature' },
    { name: 'Other' }, { path: '.github/workflows/other.yml' },
    { id: '../issues' }, { run_attempt: 0 }, { head_sha: 'not-a-commit' },
  ];
  for (const change of changes) {
    const setup = fixture();
    assert.equal(workflowFor({ ...base, ...change }), undefined);
    assert.equal(await reconcile(event({ ...base, ...change }), setup.api), 'ignored run');
    assert.equal(setup.calls.length, 0);
  }
  const setup = fixture();
  assert.equal(await reconcile({ ...event(base), repository: {} }, setup.api), 'ignored event');
  assert.equal(await reconcile({ ...event(base), action: 'requested' }, setup.api), 'ignored event');
  assert.equal(setup.calls.length, 0);
});

test('independently verify the triggering run and reject changed API identities', async () => {
  for (const change of [{ id: 124 }, { run_attempt: 2 }, { head_sha: 'b'.repeat(40) }, { event: 'push' }]) {
    const setup = fixture({ run: { ...base, ...change } });
    await assert.rejects(reconcile(event(base), setup.api), /identity changed/);
    assert.equal(setup.mutations().length, 0);
  }
});

test('new failures create one assigned issue containing only run metadata', async () => {
  for (const conclusion of ['failure', 'timed_out', 'action_required', 'startup_failure', 'stale']) {
    const run = { ...base, conclusion };
    const setup = fixture({ run });
    assert.equal(await reconcile(event(run), setup.api), 'updated failure issue');
    const [creation] = setup.mutations();
    assert.equal(creation.suffix, 'issues');
    assert.equal(creation.method, 'POST');
    assert.deepEqual(creation.body.assignees, ['swiftraccoon']);
    assert.equal(creation.body.title, '[Scheduled checks] Security');
    assert.ok(creation.body.body.startsWith(`${marker}\n`));
    assert.match(creation.body.body, /actions\/runs\/123\/attempts\/1/);
    assert.equal(setup.mutations().length, 1);
  }
});

test('repeated failures update the existing issue without new issues or comments', async () => {
  const setup = fixture({ issues: [issue] });
  await reconcile(event(base), setup.api);
  const [update] = setup.mutations();
  assert.equal(update.method, 'PATCH');
  assert.equal(update.suffix, 'issues/19');
  assert.equal(update.body.state, 'open');
  const repeated = fixture({ issues: [{ ...issue, ...update.body }] });
  assert.equal(await reconcile(event(base), repeated.api), 'already current');
  assert.equal(repeated.mutations().length, 0);
});

test('full recovery closes the incident and another failure reopens the same issue', async () => {
  const run = { ...base, conclusion: 'success' };
  const setup = fixture({ run, issues: [issue] });
  assert.equal(await reconcile(event(run), setup.api), 'closed recovery issue');
  const [update] = setup.mutations();
  assert.equal(update.body.state, 'closed');
  assert.match(update.body.body, /Security recovered on main/);
  const recurrence = fixture({ issues: [{ ...issue, ...update.body }] });
  await reconcile(event(base), recurrence.api);
  assert.equal(recurrence.mutations()[0].suffix, 'issues/19');
  assert.equal(recurrence.mutations()[0].body.state, 'open');
});

test('healthy workflows create no issues and cancellation never resolves an incident', async () => {
  const run = { ...base, conclusion: 'success' };
  const setup = fixture({ run });
  assert.equal(await reconcile(event(run), setup.api), 'healthy');
  assert.equal(setup.mutations().length, 0);
  for (const conclusion of ['cancelled', 'skipped', 'neutral']) {
    const cancelled = { ...base, conclusion };
    const other = fixture({ run: cancelled, issues: [issue] });
    assert.equal(await reconcile(event(cancelled), other.api), 'ignored conclusion');
    assert.equal(other.mutations().length, 0);
  }
});

test('delayed events reconcile the latest result, including rerun attempts', async () => {
  for (const latest of [{ ...base, id: 124, conclusion: 'success' }, { ...base, id: 124, run_attempt: 2 }]) {
    const setup = fixture({ latest, issues: [issue] });
    await reconcile(event({ ...base }), setup.api);
    const [update] = setup.mutations();
    assert.match(update.body.body, /runs\/124\/attempts\//);
    assert.equal(update.body.state, latest.conclusion === 'success' ? 'closed' : 'open');
  }
});

test('a newer diagnostic success cannot suppress a full failure or recovery', async () => {
  for (const conclusion of ['failure', 'success']) {
    const run = { ...base, conclusion };
    const diagnostic = { ...base, id: 124, conclusion: 'success', event: 'workflow_dispatch' };
    const setup = fixture({ run, latest: [diagnostic, run], issues: [issue], jobMap: { 'actions/runs/124/jobs?per_page=100': [] } });
    await reconcile(event(run), setup.api);
    const [update] = setup.mutations();
    assert.match(update.body.body, /runs\/123\/attempts\/1/);
    assert.equal(update.body.state, conclusion === 'success' ? 'closed' : 'open');
  }
});

test('a cancelled completion still reconciles an earlier failure queued behind it', async () => {
  const cancelled = { ...base, conclusion: 'cancelled' };
  const failure = { ...base, id: 122 };
  const setup = fixture({ run: cancelled, latest: [cancelled, failure] });
  assert.equal(await reconcile(event(cancelled), setup.api), 'updated failure issue');
  assert.match(setup.mutations()[0].body.body, /runs\/122\/attempts\/1/);
});

test('partial or skipped diagnostic runs cannot claim a full recovery', async () => {
  const run = { ...base, event: 'workflow_dispatch', conclusion: 'success' };
  for (const jobs of [[], [{ name: 'Fast security gate', conclusion: 'success' }],
    [{ name: 'Fast security gate', conclusion: 'success' }, { name: 'Scheduled policy mutation checks', conclusion: 'skipped' }]]) {
    const setup = fixture({ run, issues: [issue], jobs });
    assert.equal(await reconcile(event(run), setup.api), 'partial success');
    assert.equal(setup.mutations().length, 0);
  }
});

test('ignore user-authored lookalikes and paginate before creating duplicates', async () => {
  const fake = { ...issue, user: { login: 'someone-else' } };
  const setup = fixture({ firstPage: Array.from({ length: 100 }, () => fake), issues: [issue] });
  await reconcile(event(base), setup.api);
  assert.ok(setup.calls.some(call => call.suffix.endsWith('page=2')));
  assert.equal(setup.mutations()[0].suffix, 'issues/19');
});

test('GitHub transport fixes the host, scopes endpoints, refuses redirects and bounds responses', async () => {
  const calls = [];
  const api = github('fixture-token', async (url, options) => {
    calls.push({ url, options });
    return new Response('{}');
  });
  await api('issues', 'POST', { title: 'Example' });
  const [call] = calls;
  assert.equal(call.url, 'https://api.github.com/repos/swiftraccoon/simplestChat/issues');
  assert.equal(call.options.redirect, 'error');
  assert.equal(call.options.headers.Authorization, 'Bearer fixture-token');
  assert.ok(call.options.signal instanceof AbortSignal);
  assert.equal(call.options.body, '{"title":"Example"}');
  for (const [suffix, method] of [['../other/repo', 'GET'], ['issues/1', 'DELETE'], ['actions/runs/1', 'PATCH']]) {
    await assert.rejects(api(suffix, method), /unsupported GitHub endpoint/);
  }
  assert.equal(calls.length, 1);
  await assert.rejects(github('fixture-token', async () => new Response('private body', { status: 403 }))('actions/runs/1'), /GitHub request failed \(403\)/);
  await assert.rejects(github('fixture-token', async () => new Response('x'.repeat(2 * 1024 * 1024 + 1)))('actions/runs/1'), /response too large/);
});
