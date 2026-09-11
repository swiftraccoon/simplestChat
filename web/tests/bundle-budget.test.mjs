import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, rm, symlink, truncate, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { gzipSync } from 'node:zlib';
import { budgetFailures, measureBundle, parseBudgets } from '../scripts/check-bundle.mjs';

const limits = () => ({
  javascript: { bytes: 1000, gzipBytes: 500 },
  styles: { bytes: 1000, gzipBytes: 500 },
  html: { bytes: 1000, gzipBytes: 500 },
});

async function fixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-bundle.'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const dist = path.join(root, 'dist');
  await mkdir(path.join(dist, 'assets', 'lazy'), { recursive: true });
  const assets = {
    'index.html': '<!doctype html><title>Chat</title>',
    'help.html': '<!doctype html><title>Help</title>',
    'assets/index.js': 'export const chat = true;',
    'assets/index.css': 'body { color: #fff; }',
  };
  for (const [filename, contents] of Object.entries(assets)) {
    await writeFile(path.join(dist, filename), contents);
  }
  return { root, dist, assets };
}

test('budget configuration requires every known group and positive integer byte limits', () => {
  assert.deepEqual(parseBudgets(limits()), limits());
  for (const invalid of [null, [], {}, { ...limits(), typo: {} }]) {
    assert.throws(() => parseBudgets(invalid), /budgets must contain/);
  }
  for (const invalid of [0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, '1000']) {
    const config = limits();
    config.javascript.bytes = invalid;
    assert.throws(() => parseBudgets(config), /javascript.bytes/);
  }
  const unknownKey = limits();
  unknownKey.html = { bytes: 1000, gzpiBytes: 500 };
  assert.throws(() => parseBudgets(unknownKey), /html.gzipBytes/);
  const unknownGroup = limits();
  delete unknownGroup.styles;
  unknownGroup.style = { bytes: 1000, gzipBytes: 500 };
  assert.throws(() => parseBudgets(unknownGroup), /Invalid styles budget/);
  const incoherent = limits();
  incoherent.html.gzipBytes = 1001;
  assert.throws(() => parseBudgets(incoherent), /gzipBytes must not exceed bytes/);
});

test('all JS chunks, stylesheets and pages count, gzip is per file and imports have no CLI effects', async (t) => {
  const { dist, assets } = await fixture(t);
  const lazy = 'export const settings = "⚙";';
  await writeFile(path.join(dist, 'assets/lazy/settings.mjs'), lazy);
  await writeFile(path.join(dist, 'help.css'), 'main { margin: auto; }');
  await writeFile(path.join(dist, 'robots.txt'), 'User-agent: *');
  const sizes = await measureBundle(dist);
  assert.equal(sizes.javascript.bytes, Buffer.byteLength(assets['assets/index.js'] + lazy));
  assert.equal(
    sizes.javascript.gzipBytes,
    gzipSync(assets['assets/index.js'], { level: 6 }).length + gzipSync(lazy, { level: 6 }).length,
  );
  assert.equal(
    sizes.styles.bytes,
    Buffer.byteLength(assets['assets/index.css'] + 'main { margin: auto; }'),
  );
  assert.equal(sizes.html.bytes, Buffer.byteLength(assets['index.html'] + assets['help.html']));
  assert.deepEqual(budgetFailures(sizes, parseBudgets(limits())), []);
});

test('equal limits pass; raw and gzip overages are independently reported for every group', () => {
  const budgets = limits();
  assert.deepEqual(budgetFailures(budgets, budgets), []);
  const sizes = limits();
  sizes.javascript.bytes += 1;
  sizes.styles.gzipBytes += 1;
  sizes.html.bytes += 1;
  sizes.html.gzipBytes += 1;
  assert.deepEqual(budgetFailures(sizes, budgets), [
    'javascript.bytes: 1001 exceeds 1000 bytes',
    'styles.gzipBytes: 501 exceeds 500 bytes',
    'html.bytes: 1001 exceeds 1000 bytes',
    'html.gzipBytes: 501 exceeds 500 bytes',
  ]);
});

test('splitting JavaScript into individually small chunks cannot bypass the aggregate budget', async (t) => {
  const { dist } = await fixture(t);
  await writeFile(path.join(dist, 'assets/lazy/first.js'), 'a'.repeat(600));
  await writeFile(path.join(dist, 'assets/lazy/second.js'), 'b'.repeat(600));
  const failures = budgetFailures(await measureBundle(dist), limits());
  assert.equal(failures.length, 1);
  assert.match(failures[0], /javascript.bytes: 1225 exceeds 1000 bytes/);
});

for (const page of ['index.html', 'help.html']) {
  test(`missing ${page} fails instead of reporting an artificially small bundle`, async (t) => {
    const { dist } = await fixture(t);
    await rm(path.join(dist, page));
    await assert.rejects(measureBundle(dist), new RegExp(`missing required pages: ${page}`));
  });
  test(`empty ${page} fails instead of reporting an artificially small bundle`, async (t) => {
    const { dist } = await fixture(t);
    await writeFile(path.join(dist, page), '');
    await assert.rejects(measureBundle(dist), new RegExp(`required page is empty: ${page}`));
  });
}

test('an accidentally huge artifact is rejected before compression', async (t) => {
  const { dist } = await fixture(t);
  await truncate(path.join(dist, 'assets/index.js'), 16 * 1024 * 1024 + 1);
  await assert.rejects(measureBundle(dist), /at most 16 MiB: assets\/index.js/);
});

test('missing or empty executable/style assets fail', async (t) => {
  const { dist } = await fixture(t);
  await writeFile(path.join(dist, 'assets/index.js'), '');
  await assert.rejects(measureBundle(dist), /nonempty JavaScript and CSS/);
  await writeFile(path.join(dist, 'assets/index.js'), 'void 0;');
  await rm(path.join(dist, 'assets/index.css'));
  await assert.rejects(measureBundle(dist), /nonempty JavaScript and CSS/);
});

test('symbolic links to files or directories are rejected even for otherwise uncounted assets', async (t) => {
  const { dist, root } = await fixture(t);
  const link = path.join(dist, 'linked.txt');
  await symlink(path.join(dist, 'index.html'), link);
  await assert.rejects(measureBundle(dist), /symlink or special file: linked.txt/);
  await rm(link);
  await symlink(root, path.join(dist, 'outside'));
  await assert.rejects(measureBundle(dist), /symlink or special file: outside/);
  const rootLink = path.join(root, 'output-link');
  await symlink(dist, rootLink);
  await assert.rejects(measureBundle(rootLink), /regular directory/);
});

test('CLI resolves output relative to its checkout, reports sizes, and fails on an exceeded budget', async (t) => {
  const { root } = await fixture(t);
  const scripts = path.join(root, 'scripts');
  await mkdir(scripts);
  const script = path.join(scripts, 'check-bundle.mjs');
  await copyFile(new URL('../scripts/check-bundle.mjs', import.meta.url), script);
  const config = path.join(root, 'bundle-budget.json');
  await writeFile(config, JSON.stringify(limits()));
  const options = { cwd: os.tmpdir(), encoding: 'utf8', timeout: 10_000 };
  const passed = spawnSync(process.execPath, [script], options);
  assert.equal(passed.status, 0, passed.stderr);
  assert.match(passed.stdout, /javascript: 25\/1000 bytes; gzip /);
  const budgets = limits();
  budgets.javascript = { bytes: 1, gzipBytes: 1 };
  await writeFile(config, JSON.stringify(budgets));
  const failed = spawnSync(process.execPath, [script], options);
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /Bundle budget exceeded/);
  assert.match(failed.stderr, /javascript.gzipBytes/);
  await writeFile(config, '{');
  assert.equal(spawnSync(process.execPath, [script], options).status, 1);
});
