import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';

async function files(t, sources) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-native-fixtures-'));
  t.after(() => t.after(() => rm(directory, { recursive: true, force: true })));
  await Promise.all(
    Object.entries(sources).map(([name, source]) => writeFile(join(directory, name), source)),
  );
  return (name) => pathToFileURL(join(directory, name));
}

test('native fixture modules retain independent browser globals and supported dependency mocks', async (t) => {
  const file = await files(t, {
    'dependency.ts': 'export const suffix = "real";',
    'reader.ts': `import { suffix } from './dependency';
      interface Browser { label: string }
      export function read(): string { return (document as unknown as Browser).label + suffix; }
      export async function later(): Promise<string> { await Promise.resolve(); return read(); }`,
  });
  const first = await loadTypeScript(file('reader.ts'), {
    globals: { document: { label: 'first:' } },
    modules: { './dependency': { suffix: 'one' } },
  });
  const second = await loadTypeScript(file('reader.ts'), {
    globals: { document: { label: 'second:' } },
    modules: { './dependency': { suffix: 'two' } },
  });
  assert.equal(first.read(), 'first:one');
  assert.equal(second.read(), 'second:two');
  assert.equal(await first.later(), 'first:one');
  assert.equal('document' in globalThis, false, 'fixture bindings never change Node globals');
  const real = await import(file('dependency.ts').href);
  assert.equal(real.suffix, 'real', 'fixture-scoped mocks never replace normal imports');
});

test('unmocked dependencies link normally and dynamic imports keep their fixture owner', async (t) => {
  const file = await files(t, {
    'dependency.ts': 'export function name(): string { return document.label; }',
    'reader.ts': `import { name } from './dependency';
      export const immediate = name;
      export async function deferred() { return (await import('./dependency')).name(); }`,
  });
  const first = await loadTypeScript(file('reader.ts'), {
    globals: { document: { label: 'first' } },
  });
  const second = await loadTypeScript(file('reader.ts'), {
    globals: { document: { label: 'second' } },
  });
  assert.equal(first.immediate(), 'first');
  assert.equal(second.immediate(), 'second');
  assert.equal(await first.deferred(), 'first');
  assert.equal(await second.deferred(), 'second');
});

test('a lazy mock is imported only on demand and a failed resolution can be retried explicitly', async (t) => {
  const file = await files(t, {
    'dependency.ts': 'export const marker = "real";',
    'reader.ts': 'export async function read() { return (await import("./dependency")).marker; }',
  });
  let imports = 0;
  const modules = {};
  Object.defineProperty(modules, './dependency', {
    get() {
      imports++;
      if (imports === 1) throw new Error('unavailable chunk');
      return { marker: 'recovered' };
    },
  });
  const reader = await loadTypeScript(file('reader.ts'), { modules });
  assert.equal(imports, 0);
  await assert.rejects(reader.read(), /unavailable chunk/);
  assert.equal(imports, 1);
  assert.equal(await reader.read(), 'recovered');
  assert.equal(imports, 2);
});

test('fixture bindings remain available while the owning test releases async resources', async (t) => {
  const file = await files(t, {
    'late.ts': 'export function label() { return document.label; }',
    'resource.ts': 'export async function dispose() { return (await import("./late")).label(); }',
  });
  const resource = await loadTypeScript(file('resource.ts'), {
    globals: { document: { label: 'owner' } },
  });
  t.after(async () => {
    assert.equal(await resource.dispose(), 'owner');
  });
});
