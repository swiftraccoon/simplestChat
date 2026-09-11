import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/ice-isolation.yml', import.meta.url), 'utf8');
const compatibility = readFileSync(new URL('../../.github/workflows/browser-compatibility.yml', import.meta.url), 'utf8');
const steps = workflow.split(/(?=^      - (?:name|uses):)/m).slice(1);
const step = name => {
  const found = steps.find(value => value.startsWith(`      - name: ${name}\n`));
  assert.ok(found, `missing step: ${name}`);
  return found;
};

test('ICE isolation stays manual and runs both engines in one macOS job without the app', () => {
  assert.match(workflow, /on:\n  workflow_dispatch:\n\npermissions:/);
  assert.deepEqual([...workflow.split('\njobs:\n')[1].matchAll(/^  ([\w-]+):$/gm)].map(match => match[1]), ['same-host']);
  assert.match(workflow, /runs-on: macos-26/);
  assert.match(workflow, /timeout-minutes: 15/);
  assert.doesNotMatch(workflow, /matrix:|services:|cargo |with-test-server|with-test-postgres|community\.cjs/);
});

test('ICE isolation retains read-only permissions and the existing pinned tooling', () => {
  assert.match(workflow, /permissions:\n  contents: read\n/);
  assert.match(workflow, /persist-credentials: false/);
  const actions = [...workflow.matchAll(/uses: ([^\s]+) /g)].map(match => match[1]);
  assert.equal(actions.length, 3);
  for (const action of actions) {
    assert.match(action, /@[a-f0-9]{40}$/);
    assert.ok(compatibility.includes(`uses: ${action}`), 'reuse the audited action pins');
  }
  assert.match(workflow, /node-version: 26\.8\.1/);
  assert.match(workflow, /cache-dependency-path: web\/e2e\/package-lock\.json/);
  const tooling = step('Install pinned browser tooling');
  assert.match(tooling, /id: tooling/);
  assert.match(tooling, /npm --prefix web\/e2e ci --ignore-scripts/);
  assert.match(tooling, /npm --prefix web\/e2e exec -- playwright install webkit chromium/);
  assert.match(tooling, /node build\/test-media-ip\.mjs/);
});

test('both ICE probes run after successful tooling even when the preceding engine fails', () => {
  const guard = "if: ${{ !cancelled() && steps.tooling.outcome == 'success' }}";
  assert.match(workflow, /ICE_ISOLATION_E2E: '1'/);
  for (const [name, engine] of [['WebKit', 'webkit'], ['Chromium', 'chromium']]) {
    const probe = step(`${name} receive-only ICE`);
    assert.ok(probe.includes(guard));
    assert.match(probe, /timeout-minutes: 3/);
    assert.ok(probe.includes(`E2E_BROWSER: ${engine}`));
    assert.ok(probe.includes(`E2E_ARTIFACTS: \${{ runner.temp }}/ice-isolation/${engine}`));
    assert.match(probe, /run: node web\/e2e\/ice-isolation\.cjs\n/);
    assert.doesNotMatch(probe, /continue-on-error|\|\|\s*true/);
  }
  assert.ok(workflow.indexOf('name: WebKit receive-only ICE') < workflow.indexOf('name: Chromium receive-only ICE'));
});

test('same-host ICE reports and OS provenance remain available after a probe failure', () => {
  assert.match(step('Record macOS version'), /run: sw_vers/);
  const upload = step('Preserve both browser reports');
  assert.ok(upload.includes('if: ${{ !cancelled() }}'));
  assert.ok(upload.includes('path: ${{ runner.temp }}/ice-isolation\n'));
  assert.match(upload, /retention-days: 7/);
});
