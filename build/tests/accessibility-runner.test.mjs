import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import vm from 'node:vm';

const require = createRequire(import.meta.url);
const { configuration, summarizeFindings, scanPage, tags } = require('../../web/e2e/accessibility.cjs');
const runner = fileURLToPath(new URL('../../web/e2e/accessibility.cjs', import.meta.url));
const allowed = { ACCESSIBILITY_E2E: '1', DISPOSABLE_TEST_DATABASE: '1', BASE_URL: 'http://127.0.0.1:3119' };

test('accessibility runner requires both explicit accessibility and disposable-service opt-ins', () => {
  assert.throws(() => configuration({}), /ACCESSIBILITY_E2E/);
  assert.throws(() => configuration({ ...allowed, ACCESSIBILITY_E2E: '' }), /ACCESSIBILITY_E2E/);
  assert.throws(() => configuration({ ...allowed, DISPOSABLE_TEST_DATABASE: '' }), /DISPOSABLE_TEST_DATABASE/);
});

test('accessibility runner rejects remote, credentialed and non-origin URLs before launch', () => {
  for (const BASE_URL of ['https://127.0.0.1', 'http://example.test', 'http://user:secret@localhost', 'http://localhost/room', 'http://localhost/?x=1', 'http://localhost/#x']) {
    assert.throws(() => configuration({ ...allowed, BASE_URL }), /loopback HTTP/);
  }
  for (const BASE_URL of ['http://127.0.0.1:3119', 'http://localhost:3119', 'http://[::1]:3119']) {
    assert.equal(configuration({ ...allowed, BASE_URL }).base, BASE_URL);
  }
  assert.throws(() => configuration({ ...allowed, E2E_BROWSER: 'unknown' }), /Unsupported E2E browser/);
});

test('accessibility scan uses every requested WCAG tag without exclusions or disabled rules', async () => {
  const page = {};
  const calls = [];
  class AxeBuilder {
    constructor(options) { assert.equal(options.page, page); }
    withTags(selected) { calls.push(selected); return this; }
    async analyze() { return { testEngine: { version: 'fixture' }, violations: [], incomplete: [] }; }
  }
  const result = await scanPage(page, 'fixture-view', AxeBuilder);
  assert.deepEqual(tags, ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']);
  assert.deepEqual(calls, [[...tags]]);
  assert.equal(result.name, 'fixture-view');
  assert.equal(result.axeVersion, 'fixture');
  assert.equal(result.violations.ruleCount, 0);
});

test('reports retain bounded rule/selector counts but omit HTML and check payloads', () => {
  const findings = Array.from({ length: 51 }, (_, index) => ({
    id: `rule-${index}`, impact: 'serious', description: 'PRIVATE_DESCRIPTION',
    nodes: Array.from({ length: 21 }, () => ({
      target: [`#${'x'.repeat(600)}`], html: '<input value="PRIVATE_HTML">',
      any: [{ message: 'PRIVATE_CHECK_PAYLOAD' }, { id: 'color-contrast', data: { messageKey: 'PRIVATE_REASON' } },
        { id: 'color-contrast', data: { messageKey: 'bgGradient', privateValue: 'PRIVATE_DATA' } }], relatedNodes: [{ html: 'PRIVATE_RELATED' }],
    })),
  }));
  const summary = summarizeFindings(findings);
  assert.equal(summary.ruleCount, 51);
  assert.equal(summary.omittedRules, 1);
  assert.equal(summary.rules.length, 50);
  assert.equal(summary.rules[0].nodeCount, 21);
  assert.equal(summary.rules[0].omittedNodes, 1);
  assert.equal(summary.rules[0].targets.length, 20);
  assert.equal(summary.rules[0].targets[0].length, 512);
  assert.deepEqual(summary.rules[0].reasonCodes[0], ['bgGradient']);
  assert.doesNotMatch(JSON.stringify(summary), /PRIVATE_/);
});

test('incomplete rule findings remain visible for manual review', async () => {
  class AxeBuilder {
    withTags() { return this; }
    async analyze() { return { testEngine: { version: 'fixture' }, violations: [], incomplete: [{ id: 'color-contrast', impact: 'serious', nodes: [{ target: ['#fixture'] }] }] }; }
  }
  const result = await scanPage({}, 'review-needed', AxeBuilder);
  assert.equal(result.violations.ruleCount, 0);
  assert.equal(result.incomplete.ruleCount, 1);
  assert.equal(result.incomplete.rules[0].id, 'color-contrast');
});

test('contrast diagnostics retain only bounded selectors and computed CSS, not content', async () => {
  const node = { textContent: 'PRIVATE_TEXT', value: 'PRIVATE_VALUE', innerHTML: 'PRIVATE_HTML', parentElement: null };
  node.parentElement = node;
  const context = { document: { querySelector: () => node }, getComputedStyle: () => ({
    color: 'rgb(255, 255, 255)', backgroundColor: 'rgb(59, 130, 246)', opacity: '1', fontSize: '14px', fontWeight: '500',
    backgroundImage: 'url(PRIVATE_IMAGE)',
  }) };
  const page = { async evaluate(work, selectors) {
    assert.equal(selectors.length, 50);
    context.selectors = selectors;
    return vm.runInNewContext(`(${work.toString()})(selectors)`, context);
  } };
  class AxeBuilder {
    withTags() { return this; }
    async analyze() { return { testEngine: { version: 'fixture' }, incomplete: [], violations: [{
      id: 'color-contrast', nodes: Array.from({ length: 60 }, () => ({ target: ['#fixture'] })),
    }] }; }
  }
  const result = await scanPage(page, 'contrast', AxeBuilder);
  assert.equal(result.contrastStyles.length, 50);
  assert.equal(result.contrastStyles[0].backgrounds.length, 6);
  assert.equal(result.contrastStyles[0].opacity, '1');
  assert.equal(result.contrastStyles[0].color, 'rgb(255, 255, 255)');
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE_/);
});

test('failed accessibility launch exits nonzero with a private failure report', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-a11y-runner.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const tooling = path.join(directory, 'tooling');
  await mkdir(tooling);
  await writeFile(path.join(tooling, 'package.json'), JSON.stringify({ version: 'fixture', main: 'index.cjs' }));
  await writeFile(path.join(tooling, 'index.cjs'), "module.exports={chromium:{async launch(){throw new Error('PRIVATE_BROWSER_PAYLOAD')}}};\n");
  const artifacts = path.join(directory, 'artifacts');
  const result = spawnSync(process.execPath, [runner], { env: { PATH: process.env.PATH, ...allowed, PLAYWRIGHT_MODULE: tooling, E2E_ARTIFACTS: artifacts }, encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(await readFile(path.join(artifacts, 'accessibility-results.json'), 'utf8'));
  assert.equal(report.passed, false);
  assert.equal(report.complete, false);
  assert.equal(report.failedStep, 'launch');
  assert.ok(Date.parse(report.finishedAt));
  assert.doesNotMatch(JSON.stringify(report) + result.stderr, /PRIVATE_BROWSER_PAYLOAD/);
});

test('CI invokes the accessibility smoke through owned services and preserves its own artifacts', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
  assert.match(workflow, /ACCESSIBILITY_E2E: ['"]1['"]/);
  assert.match(workflow, /build\/with-test-server\.sh node web\/e2e\/accessibility\.cjs/);
  assert.match(workflow, /name: accessibility-e2e/);
});
