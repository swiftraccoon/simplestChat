import { existsSync } from 'node:fs';
import { createRequire, registerHooks } from 'node:module';
import { getTestContext, mock } from 'node:test';
import { pathToFileURL } from 'node:url';

const webRoot = new URL('../', import.meta.url);
// Only fixture bindings are customized. Node performs TypeScript stripping, ESM
// linking, evaluation and export mocking; ordinary dependencies import normally.
const cases = new Map();
const query = 'test-fixture';
let sequence = 0;

function scoped(url, id) {
  const result = new URL(url);
  result.searchParams.set(query, id);
  return result;
}
function localUrl(specifier, parent) {
  const url = new URL(specifier, parent);
  if (!existsSync(url)) {
    const typed = new URL(url);
    typed.pathname += '.ts';
    if (existsSync(typed)) return typed;
  }
  return url;
}
// A query gives each fixture its own native module instances. Captured mock
// globals remain lexical, so two live browser fixtures cannot replace each other.
registerHooks({
  resolve(specifier, context, nextResolve) {
    const parent = context.parentURL && new URL(context.parentURL);
    const id = parent?.searchParams.get(query);
    const fixture = cases.get(id);
    if (!fixture) return nextResolve(specifier, context);
    if (fixture.mocks.has(specifier)) {
      // A lazy fixture export is resolved only when the native import occurs.
      fixture.lazy.get(specifier)?.();
      return nextResolve(fixture.mocks.get(specifier).href, context);
    }
    if (specifier.startsWith('.'))
      return nextResolve(scoped(localUrl(specifier, parent), id).href, context);
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    const fixture = cases.get(new URL(url).searchParams.get(query));
    if (!fixture) return nextLoad(url, context);
    if (new URL(url).pathname.endsWith('.css'))
      return { format: 'module', source: 'export {};', shortCircuit: true };
    const loaded = nextLoad(url, context);
    if (loaded.format !== 'module-typescript' && loaded.format !== 'module') return loaded;
    if (!fixture.globals.length) return loaded;
    return {
      ...loaded,
      source: `import { ${fixture.globals.join(', ')} } from ${JSON.stringify(fixture.globalUrl)};\n${loaded.source}`,
    };
  },
});
/** Import actual source with optional dependency exports and browser globals. */
export async function loadTypeScript(relativePath, { modules = {}, globals = {} } = {}) {
  const source = new URL(relativePath, webRoot);
  const id = String(++sequence);
  const tracker = getTestContext()?.mock ?? mock;
  const fixture = {
    mocks: new Map(),
    lazy: new Map(),
    globals: Object.keys(globals),
    globalUrl: `data:text/javascript,export {}#fixture-globals-${id}`,
  };
  cases.set(id, fixture);
  tracker.module(fixture.globalUrl, { exports: globals });
  for (const specifier of Object.getOwnPropertyNames(modules)) {
    const resolved = specifier.startsWith('.')
      ? localUrl(specifier, source)
      : pathToFileURL(createRequire(source).resolve(specifier));
    const url = scoped(resolved, id);
    fixture.mocks.set(specifier, url);
    const descriptor = Object.getOwnPropertyDescriptor(modules, specifier);
    if (descriptor.get) {
      fixture.lazy.set(specifier, () => {
        tracker.module(url, { exports: modules[specifier] });
        fixture.lazy.delete(specifier);
      });
    } else tracker.module(url, { exports: modules[specifier] });
  }
  const context = getTestContext();
  // Owner cleanup may await one last native import. Retire fixture bindings after
  // the cleanup callbacks already registered by the test, rather than before them.
  context?.after(() => context.after(() => cases.delete(id)));
  return import(scoped(source, id).href);
}
export async function loadProtocol() {
  const generated = await loadTypeScript('src/protocol.generated.ts');
  return loadTypeScript('src/protocol.ts', { modules: { './protocol.generated': generated } });
}
export async function loadContractModules() {
  const modules = { './validation': await loadTypeScript('src/validation.ts') };
  modules['./protocol'] = await loadProtocol();
  modules['./avatar-colors'] = await loadTypeScript('src/avatar-colors.ts');
  for (const name of [
    'api-validation',
    'protocol-validation',
    'room-administration-validation',
    'chat-history-validation',
    'discovery-validation',
    'notification-validation',
  ])
    modules[`./${name}`] = await loadTypeScript(`src/${name}.ts`, { modules });
  return modules;
}
