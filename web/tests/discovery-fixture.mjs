import { loadContractModules, loadTypeScript } from './source-loader.mjs';

export async function loadDiscoveryFixture(fixture) {
  const contracts = await loadContractModules();
  return loadTypeScript('src/discovery.ts', {
    modules: { './ui': fixture.ui, './discovery-validation': contracts['./discovery-validation'] },
    globals: {
      window: fixture.window ?? {
        location: { origin: 'https://example.test', href: 'https://example.test/', hash: '' },
        history: { state: {}, replaceState() {} },
      },
      navigator: fixture.navigator ?? { clipboard: { writeText: async () => {} } },
    },
  });
}
