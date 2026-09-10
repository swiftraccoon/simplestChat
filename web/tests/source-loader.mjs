import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { compileFunction } from 'node:vm';
// TypeScript 7 supplies tsc; Microsoft's compatibility package retains the
// synchronous compiler API needed by these isolated source-level tests.
import ts from '@typescript/typescript6';

/** Load the current browser source without emitting files or requiring a browser. */
export async function loadTypeScript(relativePath, options = {}) {
  const sourceUrl = new URL(`../${relativePath}`, import.meta.url);
  const source = await readFile(sourceUrl, 'utf8');
  return evaluateTypeScript(source, { ...options, fileName: fileURLToPath(sourceUrl) });
}

/** Browser APIs and runtime imports are injected locally, leaving globals untouched. */
export function evaluateTypeScript(source, {
  fileName = 'test-source.ts',
  modules = {},
  globals = {},
} = {}) {
  const { outputText } = ts.transpileModule(source, {
    fileName,
    compilerOptions: {
      target: ts.ScriptTarget.ES2020,
      module: ts.ModuleKind.CommonJS,
      useDefineForClassFields: true,
    },
  });
  const requireMock = (name) => {
    if (!Object.hasOwn(modules, name)) {
      throw new Error(`Missing test mock for ${name} imported by ${fileName}`);
    }
    return modules[name];
  };
  const module = { exports: {} };
  const execute = compileFunction(
    outputText,
    ['require', 'module', 'exports', ...Object.keys(globals)],
    { filename: fileName },
  );
  execute(requireMock, module, module.exports, ...Object.values(globals));
  return module.exports;
}
